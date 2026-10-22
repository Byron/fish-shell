//! Windows specific definitions
#![allow(clippy::try_err)] // suggested fix does not work (cannot infer...)

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io;
use std::mem;
use std::os::windows::io::IntoRawHandle;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, warn};
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Foundation::{self as foundation, BOOL, FALSE, HANDLE, TRUE};
use windows_sys::Win32::System::Console as console;
use windows_sys::Win32::System::Threading as threading;
use windows_sys::Win32::UI::Input::KeyboardAndMouse;

use super::{width, Event, RawMode, RawReader, Renderer, Term};
use crate::config::{Behavior, BellStyle, ColorMode, Config};
use crate::highlight::Highlighter;
use crate::keys::{KeyCode as K, KeyEvent, Modifiers as M};
use crate::layout::{Layout, Position};
use crate::line_buffer::LineBuffer;
use crate::{error, Cmd, Result};

#[path = "windows_vt.rs"]
mod vt;

const BRACKETED_PASTE_ON: &[u16] = &[27, 91, 63, 50, 48, 48, 52, 104];
const BRACKETED_PASTE_OFF: &[u16] = &[27, 91, 63, 50, 48, 48, 52, 108];

fn get_std_handle(fd: console::STD_HANDLE) -> Result<HANDLE> {
    let handle = unsafe { console::GetStdHandle(fd) };
    check_handle(handle)
}

fn check_handle(handle: HANDLE) -> Result<HANDLE> {
    if handle == foundation::INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())?;
    } else if handle == 0 {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "no stdio handle available for this process",
        ))?;
    }
    Ok(handle)
}

fn check(rc: BOOL) -> io::Result<()> {
    if rc == FALSE {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn get_win_size(handle: HANDLE) -> (usize, usize) {
    let mut info = unsafe { mem::zeroed() };
    match unsafe { console::GetConsoleScreenBufferInfo(handle, &mut info) } {
        FALSE => (80, 24),
        _ => (
            info.dwSize.X as usize,
            (1 + info.srWindow.Bottom - info.srWindow.Top) as usize,
        ), // (info.srWindow.Right - info.srWindow.Left + 1)
    }
}

fn get_console_mode(handle: HANDLE) -> Result<console::CONSOLE_MODE> {
    let mut original_mode = 0;
    check(unsafe { console::GetConsoleMode(handle, &mut original_mode) })?;
    Ok(original_mode)
}

#[derive(Default)]
pub struct ConsoleBuffer {
    pending: VecDeque<ConsoleInput>,
}
#[cfg(not(test))]
pub type Buffer = ConsoleBuffer;
type ConsoleKeyMap = ();
#[cfg(not(test))]
pub type KeyMap = ConsoleKeyMap;
#[cfg(not(test))]
pub type Mode = ConsoleMode;

#[must_use = "You must restore default mode (disable_raw_mode)"]
#[derive(Clone, Debug)]
pub struct ConsoleMode {
    original_conin_mode: console::CONSOLE_MODE,
    conin: HANDLE,
    original_conout_mode: Option<console::CONSOLE_MODE>,
    conout: HANDLE,
    raw_mode: Arc<AtomicBool>,
    bracketed_paste: bool,
}

impl RawMode for ConsoleMode {
    fn disable_raw_mode(&self) -> Result<()> {
        // Turn off the protocol while VT output is still enabled. Attempt all
        // restoration steps even if a console handle closes during shutdown.
        let paste = if self.bracketed_paste {
            write_all(self.conout, BRACKETED_PASTE_OFF)
        } else {
            Ok(())
        };
        let input: Result<()> =
            check(unsafe { console::SetConsoleMode(self.conin, self.original_conin_mode) })
                .map_err(Into::into);
        let output: Result<()> = if let Some(mode) = self.original_conout_mode {
            check(unsafe { console::SetConsoleMode(self.conout, mode) }).map_err(Into::into)
        } else {
            Ok(())
        };
        self.raw_mode.store(false, Ordering::SeqCst);
        paste.and(input).and(output)
    }
}

/// Windows console events and raw ConPTY VT input share one editing transport.
pub struct ConsoleRawReader {
    conin: HANDLE,
    pipe_reader: Option<Rc<AsyncPipe>>,
    vt_input: bool,
    decoder: vt::Decoder,
    utf16: ConsoleInputDecoder,
    pending: VecDeque<ConsoleInput>,
    escape_timeout: Duration,
    escape_deadline: Option<Instant>,
    paste: Option<vt::Paste>,
}

impl ConsoleRawReader {
    fn create(
        conin: HANDLE,
        pipe_reader: Option<Rc<AsyncPipe>>,
        vt_input: bool,
        config: &Config,
        buffer: Option<ConsoleBuffer>,
    ) -> Self {
        Self {
            conin,
            pipe_reader,
            vt_input,
            decoder: vt::Decoder::default(),
            utf16: ConsoleInputDecoder::default(),
            pending: buffer.unwrap_or_default().pending,
            escape_timeout: Duration::from_millis(u64::from(config.keyseq_timeout().unwrap_or(50))),
            escape_deadline: None,
            paste: None,
        }
    }

    fn next_event(&mut self, external_print: bool) -> Result<Event> {
        use foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use threading::{WaitForMultipleObjects, INFINITE};

        // A resize may interrupt read_pasted_text. Resume through normal key
        // dispatch so the editor can repaint without losing the paste buffer.
        if self.paste.is_some() {
            return Ok(Event::KeyPress(KeyEvent(K::BracketedPasteStart, M::NONE)));
        }
        loop {
            if let Some(input) = self.pending.pop_front() {
                if let Some(key) = self.decode_input(input) {
                    return Ok(Event::KeyPress(key));
                }
                continue;
            }
            let pipe = self.pipe_reader.as_ref().filter(|_| external_print);
            let handles = [self.conin, pipe.map_or(0, |pipe| pipe.event.0)];
            let count = if pipe.is_some() { 2 } else { 1 };
            let timeout = self.escape_deadline.map_or(INFINITE, |deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                u32::try_from(remaining.as_millis().saturating_add(1)).unwrap_or(u32::MAX - 1)
            });
            let rc = unsafe { WaitForMultipleObjects(count, handles.as_ptr(), FALSE, timeout) };
            if rc == WAIT_OBJECT_0 {
                self.read_record()?;
            } else if rc == WAIT_OBJECT_0 + 1 && pipe.is_some() {
                let pipe = pipe.unwrap();
                check(unsafe { threading::ResetEvent(pipe.event.0) })?;
                match pipe.receiver.recv() {
                    Ok(msg) => return Ok(Event::ExternalPrint(msg)),
                    Err(e) => Err(io::Error::new(io::ErrorKind::InvalidInput, e))?,
                }
            } else if rc == WAIT_TIMEOUT && self.decoder.escape_pending() {
                self.escape_deadline = None;
                return Ok(Event::KeyPress(self.decoder.finish_escape()));
            } else {
                return Err(io::Error::last_os_error().into());
            }
        }
    }

    fn decode_input(&mut self, input: ConsoleInput) -> Option<KeyEvent> {
        // ConPTY's VT input uses virtual key 0. Classic Windows virtual keys
        // retain their existing interpretation, including AltGr and modifiers.
        let key = if self.vt_input && input.virtual_key == 0 {
            let key = self.decoder.feed(input.character?);
            if self.decoder.escape_pending() {
                self.escape_deadline
                    .get_or_insert_with(|| Instant::now() + self.escape_timeout);
            } else {
                self.escape_deadline = None;
            }
            key?
        } else {
            if self.decoder.escape_pending() {
                self.pending.push_front(input);
                self.escape_deadline = None;
                return Some(self.decoder.finish_escape());
            }
            input.key
        };
        if key == KeyEvent(K::BracketedPasteStart, M::NONE) {
            self.paste = Some(vt::Paste::default());
        }
        Some(key)
    }

    fn read_record(&mut self) -> Result<()> {
        let mut record: console::INPUT_RECORD = unsafe { mem::zeroed() };
        let mut count = 0;
        check(unsafe { console::ReadConsoleInputW(self.conin, &mut record, 1, &mut count) })?;
        if count == 0 {
            return Err(error::ReadlineError::Eof);
        }
        if u32::from(record.EventType) == console::WINDOW_BUFFER_SIZE_EVENT {
            return Err(error::ReadlineError::WindowResized);
        }
        if u32::from(record.EventType) == console::KEY_EVENT {
            let event = unsafe { record.Event.KeyEvent };
            if let Some(input) = self.utf16.decode(event)? {
                self.pending
                    .extend(std::iter::repeat(input).take(usize::from(event.wRepeatCount.max(1))));
            }
        }
        Ok(())
    }
}

impl RawReader for ConsoleRawReader {
    type Buffer = ConsoleBuffer;

    fn wait_for_input(&mut self, _: bool) -> Result<Event> {
        self.next_event(true)
    }

    fn next_key(&mut self, _: bool) -> Result<KeyEvent> {
        match self.next_event(false)? {
            Event::KeyPress(key) => Ok(key),
            Event::ExternalPrint(_) => unreachable!("external printing was disabled"),
        }
    }

    fn read_pasted_text(&mut self) -> Result<String> {
        if self.paste.is_none() {
            return Ok(clipboard_win::get_clipboard_string()?);
        }
        loop {
            if let Some(input) = self.pending.pop_front() {
                if let Some(character) = input.character {
                    if self.paste.as_mut().unwrap().feed(character) {
                        return Ok(self.paste.take().unwrap().finish());
                    }
                }
            } else {
                // No timeout and no key dispatch inside a paste. Enter, Tab,
                // and control characters are data until the exact end marker.
                // A resize preserves both the paste and pending UTF-16 state.
                self.read_record()?;
            }
        }
    }

    fn find_binding(&self, _: &KeyEvent) -> Option<Cmd> {
        None
    }

    fn unbuffer(self) -> Option<ConsoleBuffer> {
        Some(ConsoleBuffer {
            pending: self.pending,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct ConsoleInput {
    key: KeyEvent,
    character: Option<char>,
    virtual_key: u16,
}

#[derive(Default)]
struct ConsoleInputDecoder {
    surrogate: u16,
}

impl ConsoleInputDecoder {
    fn decode(&mut self, key_event: console::KEY_EVENT_RECORD) -> Result<Option<ConsoleInput>> {
        use console::{
            LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED,
            SHIFT_PRESSED,
        };
        use std::char::decode_utf16;
        if key_event.bKeyDown == 0 && key_event.wVirtualKeyCode != KeyboardAndMouse::VK_MENU {
            return Ok(None);
        }
        let alt_gr = key_event.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED)
            == (LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED);
        let mut mods = M::NONE;
        if !alt_gr && key_event.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
            mods |= M::CTRL;
        }
        if !alt_gr && key_event.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
            mods |= M::ALT;
        }
        if key_event.dwControlKeyState & SHIFT_PRESSED != 0 {
            mods |= M::SHIFT;
        }
        let utf16 = unsafe { key_event.uChar.UnicodeChar };
        let key_code = match key_event.wVirtualKeyCode {
            KeyboardAndMouse::VK_LEFT => K::Left,
            KeyboardAndMouse::VK_RIGHT => K::Right,
            KeyboardAndMouse::VK_UP => K::Up,
            KeyboardAndMouse::VK_DOWN => K::Down,
            KeyboardAndMouse::VK_DELETE => K::Delete,
            KeyboardAndMouse::VK_HOME => K::Home,
            KeyboardAndMouse::VK_END => K::End,
            KeyboardAndMouse::VK_PRIOR => K::PageUp,
            KeyboardAndMouse::VK_NEXT => K::PageDown,
            KeyboardAndMouse::VK_INSERT => K::Insert,
            KeyboardAndMouse::VK_F1 => K::F(1),
            KeyboardAndMouse::VK_F2 => K::F(2),
            KeyboardAndMouse::VK_F3 => K::F(3),
            KeyboardAndMouse::VK_F4 => K::F(4),
            KeyboardAndMouse::VK_F5 => K::F(5),
            KeyboardAndMouse::VK_F6 => K::F(6),
            KeyboardAndMouse::VK_F7 => K::F(7),
            KeyboardAndMouse::VK_F8 => K::F(8),
            KeyboardAndMouse::VK_F9 => K::F(9),
            KeyboardAndMouse::VK_F10 => K::F(10),
            KeyboardAndMouse::VK_F11 => K::F(11),
            KeyboardAndMouse::VK_F12 => K::F(12),
            KeyboardAndMouse::VK_BACK => K::Backspace,
            KeyboardAndMouse::VK_RETURN => K::Enter,
            KeyboardAndMouse::VK_ESCAPE => K::Esc,
            KeyboardAndMouse::VK_TAB => {
                if mods.contains(M::SHIFT) {
                    mods.remove(M::SHIFT);
                    K::BackTab
                } else {
                    K::Tab
                }
            }
            _ => {
                if utf16 == 0 && key_event.wVirtualKeyCode != 0 {
                    return Ok(None);
                }
                K::UnknownEscSeq
            }
        };
        let character = if utf16 == 0 && key_event.wVirtualKeyCode != 0 {
            None
        } else {
            if (0xD800..0xDC00).contains(&utf16) {
                self.surrogate = utf16;
                return Ok(None);
            }
            let surrogate = mem::take(&mut self.surrogate);
            let decoded = if surrogate == 0 {
                decode_utf16(Some(utf16)).next()
            } else {
                decode_utf16([surrogate, utf16].iter().copied()).next()
            };
            Some(decoded.ok_or(error::ReadlineError::Eof)??)
        };
        let key = if key_code != K::UnknownEscSeq {
            KeyEvent(key_code, mods)
        } else if utf16 == 27 {
            KeyEvent(K::Esc, mods)
        } else {
            KeyEvent::new(character.expect("a character was decoded"), mods)
        };
        Ok(Some(ConsoleInput {
            key,
            character,
            virtual_key: key_event.wVirtualKeyCode,
        }))
    }
}

pub struct ConsoleRenderer {
    conout: HANDLE,
    cols: usize, // Number of columns in terminal
    buffer: String,
    utf16: Vec<u16>,
    colors_enabled: bool,
    bell_style: BellStyle,
}

impl ConsoleRenderer {
    fn new(conout: HANDLE, colors_enabled: bool, bell_style: BellStyle) -> ConsoleRenderer {
        // Multi line editing is enabled by ENABLE_WRAP_AT_EOL_OUTPUT mode
        let (cols, _) = get_win_size(conout);
        ConsoleRenderer {
            conout,
            cols,
            buffer: String::with_capacity(1024),
            utf16: Vec::with_capacity(1024),
            colors_enabled,
            bell_style,
        }
    }

    fn get_console_screen_buffer_info(&self) -> Result<console::CONSOLE_SCREEN_BUFFER_INFO> {
        let mut info = unsafe { mem::zeroed() };
        check(unsafe { console::GetConsoleScreenBufferInfo(self.conout, &mut info) })?;
        Ok(info)
    }

    fn set_console_cursor_position(
        &mut self,
        mut pos: console::COORD,
        size: console::COORD,
    ) -> Result<console::COORD> {
        use std::cmp::{max, min};
        // https://docs.microsoft.com/en-us/windows/console/setconsolecursorposition
        // > The coordinates must be within the boundaries of the console screen buffer.
        // pos.X = max(0, min(size.X - 1, pos.X));
        pos.Y = max(0, min(size.Y - 1, pos.Y));
        check(unsafe { console::SetConsoleCursorPosition(self.conout, pos) })?;
        Ok(pos)
    }

    fn clear(&mut self, length: u32, pos: console::COORD, attr: u16) -> Result<()> {
        let mut _count = 0;
        check(unsafe {
            console::FillConsoleOutputCharacterA(self.conout, b' ', length, pos, &mut _count)
        })?;
        Ok(check(unsafe {
            console::FillConsoleOutputAttribute(self.conout, attr, length, pos, &mut _count)
        })?)
    }

    fn set_cursor_visibility(&mut self, visible: bool) -> Result<Option<ConsoleCursorGuard>> {
        set_cursor_visibility(self.conout, visible)
    }

    // You can't have both ENABLE_WRAP_AT_EOL_OUTPUT and
    // ENABLE_VIRTUAL_TERMINAL_PROCESSING. So we need to wrap manually.
    fn wrap_at_eol(&mut self, s: &str, mut col: usize) -> usize {
        let mut esc_seq = 0;
        for c in s.graphemes(true) {
            if c == "\n" {
                col = 0;
            } else {
                let cw = width(c, &mut esc_seq);
                col += cw;
                if col > self.cols {
                    self.buffer.push('\n');
                    col = cw;
                }
            }
            self.buffer.push_str(c);
        }
        if col == self.cols {
            self.buffer.push('\n');
            col = 0;
        }
        col
    }

    // position at the start of the prompt, clear to end of previous input
    fn clear_old_rows(
        &mut self,
        info: &console::CONSOLE_SCREEN_BUFFER_INFO,
        layout: &Layout,
    ) -> Result<()> {
        let current_row = layout.cursor.row;
        let old_rows = layout.end.row;
        let mut coord = info.dwCursorPosition;
        coord.X = 0;
        coord.Y -= current_row as i16;
        let coord = self.set_console_cursor_position(coord, info.dwSize)?;
        self.clear(
            (info.dwSize.X * (old_rows as i16 + 1)) as u32,
            coord,
            info.wAttributes,
        )
    }
}

pub struct ConsoleCursorGuard(HANDLE);

impl Drop for ConsoleCursorGuard {
    fn drop(&mut self) {
        let _ = set_cursor_visibility(self.0, true);
    }
}

fn set_cursor_visibility(handle: HANDLE, visible: bool) -> Result<Option<ConsoleCursorGuard>> {
    let mut info = unsafe { mem::zeroed() };
    check(unsafe { console::GetConsoleCursorInfo(handle, &mut info) })?;
    let b = if visible { TRUE } else { FALSE };
    if info.bVisible == b {
        return Ok(None);
    }
    info.bVisible = b;
    check(unsafe { console::SetConsoleCursorInfo(handle, &info) })?;
    Ok(if visible {
        None
    } else {
        Some(ConsoleCursorGuard(handle))
    })
}

impl Renderer for ConsoleRenderer {
    type Reader = ConsoleRawReader;

    fn move_cursor(&mut self, old: Position, new: Position) -> Result<()> {
        let info = self.get_console_screen_buffer_info()?;
        let mut cursor = info.dwCursorPosition;
        if new.row > old.row {
            cursor.Y += (new.row - old.row) as i16;
        } else {
            cursor.Y -= (old.row - new.row) as i16;
        }
        if new.col > old.col {
            cursor.X += (new.col - old.col) as i16;
        } else {
            cursor.X -= (old.col - new.col) as i16;
        }
        self.set_console_cursor_position(cursor, info.dwSize)
            .map(|_| ())
    }

    fn refresh_line(
        &mut self,
        prompt: &str,
        line: &LineBuffer,
        hint: Option<&str>,
        old_layout: &Layout,
        new_layout: &Layout,
        highlighter: Option<&dyn Highlighter>,
    ) -> Result<()> {
        let default_prompt = new_layout.default_prompt;
        let cursor = new_layout.cursor;
        let end_pos = new_layout.end;

        self.buffer.clear();
        let mut col = 0;
        if let Some(highlighter) = highlighter {
            // TODO handle ansi escape code (SetConsoleTextAttribute)
            // append the prompt
            col = self.wrap_at_eol(&highlighter.highlight_prompt(prompt, default_prompt), col);
            // append the input line
            col = self.wrap_at_eol(&highlighter.highlight(line, line.pos()), col);
        } else if self.colors_enabled {
            // append the prompt
            col = self.wrap_at_eol(prompt, col);
            // append the input line
            col = self.wrap_at_eol(line, col);
        } else {
            // append the prompt
            self.buffer.push_str(prompt);
            // append the input line
            self.buffer.push_str(line);
        }
        // append hint
        if let Some(hint) = hint {
            if let Some(highlighter) = highlighter {
                self.wrap_at_eol(&highlighter.highlight_hint(hint), col);
            } else if self.colors_enabled {
                self.wrap_at_eol(hint, col);
            } else {
                self.buffer.push_str(hint);
            }
        }
        let info = self.get_console_screen_buffer_info()?;
        // just to avoid flickering
        let mut guard = self.set_cursor_visibility(false)?;
        // position at the start of the prompt, clear to end of previous input
        self.clear_old_rows(&info, old_layout)?;
        // display prompt, input line and hint
        write_to_console(self.conout, self.buffer.as_str(), &mut self.utf16)?;

        // position the cursor
        let info = self.get_console_screen_buffer_info()?;
        let mut coord = info.dwCursorPosition;
        coord.X = cursor.col as i16;
        coord.Y -= (end_pos.row - cursor.row) as i16;
        self.set_console_cursor_position(coord, info.dwSize)?;
        guard.take();
        Ok(())
    }

    fn write_and_flush(&mut self, buf: &str) -> Result<()> {
        write_to_console(self.conout, buf, &mut self.utf16)
    }

    /// Characters with 2 column width are correctly handled (not split).
    fn calculate_position(&self, s: &str, orig: Position) -> Position {
        let mut pos = orig;
        // Use the same visible widths as wrap_at_eol. Counting the characters
        // inside SGR sequences shifts the cursor and can invent wrapped rows;
        // the next refresh would then erase output above the actual prompt.
        let mut esc_seq = 0;
        for c in s.graphemes(true) {
            if c == "\n" {
                pos.col = 0;
                pos.row += 1;
            } else {
                let cw = width(c, &mut esc_seq);
                pos.col += cw;
                if pos.col > self.cols {
                    pos.row += 1;
                    pos.col = cw;
                }
            }
        }
        if pos.col == self.cols {
            pos.col = 0;
            pos.row += 1;
        }
        pos
    }

    fn beep(&mut self) -> Result<()> {
        match self.bell_style {
            BellStyle::Audible => write_all(self.conout, &[7; 1]),
            _ => Ok(()),
        }
    }

    /// Clear the screen. Used to handle ctrl+l
    fn clear_screen(&mut self) -> Result<()> {
        let info = self.get_console_screen_buffer_info()?;
        let coord = console::COORD { X: 0, Y: 0 };
        check(unsafe { console::SetConsoleCursorPosition(self.conout, coord) })?;
        let n = info.dwSize.X as u32 * info.dwSize.Y as u32;
        self.clear(n, coord, info.wAttributes)
    }

    fn clear_rows(&mut self, layout: &Layout) -> Result<()> {
        let info = self.get_console_screen_buffer_info()?;
        self.clear_old_rows(&info, layout)
    }

    /// Try to get the number of columns in the current terminal,
    /// or assume 80 if it fails.
    fn update_size(&mut self) {
        let (cols, _) = get_win_size(self.conout);
        self.cols = cols;
    }

    fn get_columns(&self) -> usize {
        self.cols
    }

    /// Try to get the number of rows in the current terminal,
    /// or assume 24 if it fails.
    fn get_rows(&self) -> usize {
        let (_, rows) = get_win_size(self.conout);
        rows
    }

    fn colors_enabled(&self) -> bool {
        self.colors_enabled
    }

    fn move_cursor_at_leftmost(&mut self, _: &mut ConsoleRawReader) -> Result<()> {
        let info = self.get_console_screen_buffer_info()?;
        let mut cursor = info.dwCursorPosition;
        if cursor.X == 0 {
            return Ok(());
        }
        debug!(target: "rustyline", "initial cursor location: {:?}, {:?}", cursor.X, cursor.Y);
        cursor.X = 0;
        cursor.Y += 1;
        let res = self.set_console_cursor_position(cursor, info.dwSize);
        if let Err(error::ReadlineError::Io(ref e)) = res {
            if e.raw_os_error() == Some(foundation::ERROR_INVALID_PARAMETER as i32) {
                warn!(target: "rustyline", "invalid cursor position: ({:?}, {:?}) in ({:?}, {:?})", cursor.X, cursor.Y, info.dwSize.X, info.dwSize.Y);
                write_all(self.conout, &[10; 1])?;
                return Ok(());
            }
        }
        res.map(|_| ())
    }
}

fn write_to_console(handle: HANDLE, s: &str, utf16: &mut Vec<u16>) -> Result<()> {
    utf16.clear();
    utf16.extend(s.encode_utf16());
    write_all(handle, utf16.as_slice())
}

// See write_valid_utf8_to_console
// /src/rust/library/std/src/sys/windows/stdio.rs:171
fn write_all(handle: HANDLE, mut data: &[u16]) -> Result<()> {
    use std::io::{Error, ErrorKind};
    while !data.is_empty() {
        let slice = if data.len() < 8192 {
            data
        } else if (0xD800..0xDC00).contains(&data[8191]) {
            &data[..8191]
        } else {
            &data[..8192]
        };
        let mut written = 0;
        check(unsafe {
            console::WriteConsoleW(
                handle,
                slice.as_ptr().cast::<std::ffi::c_void>(),
                slice.len() as u32,
                &mut written,
                ptr::null_mut(),
            )
        })?;
        if written == 0 {
            Err(Error::new(ErrorKind::WriteZero, "WriteConsoleW"))?;
        }
        data = &data[(written as usize)..];
    }
    Ok(())
}

#[cfg(not(test))]
pub type Terminal = Console;

#[derive(Clone, Debug)]
pub struct Console {
    conin_isatty: bool,
    conin: HANDLE,
    conout_isatty: bool,
    conout: HANDLE,
    close_on_drop: bool,
    pub(crate) color_mode: ColorMode,
    ansi_colors_supported: bool,
    enable_bracketed_paste: bool,
    vt_input: bool,
    bell_style: BellStyle,
    raw_mode: Arc<AtomicBool>,
    // external print reader
    pipe_reader: Option<Rc<AsyncPipe>>,
    // external print writer
    pipe_writer: Option<SyncSender<String>>,
}

impl Console {
    fn colors_enabled(&self) -> bool {
        // TODO ANSI Colors & Windows <10
        match self.color_mode {
            ColorMode::Enabled => self.conout_isatty && self.ansi_colors_supported,
            ColorMode::Forced => true,
            ColorMode::Disabled => false,
        }
    }
}

impl Term for Console {
    type Buffer = ConsoleBuffer;
    type CursorGuard = ConsoleCursorGuard;
    type ExternalPrinter = ExternalPrinter;
    type KeyMap = ConsoleKeyMap;
    type Mode = ConsoleMode;
    type Reader = ConsoleRawReader;
    type Writer = ConsoleRenderer;

    fn new(
        color_mode: ColorMode,
        behavior: Behavior,
        _tab_stop: usize,
        bell_style: BellStyle,
        enable_bracketed_paste: bool,
        _enable_signals: bool,
    ) -> Result<Console> {
        let (conin, conout, close_on_drop) = if behavior == Behavior::PreferTerm {
            if let (Ok(conin), Ok(conout)) = (
                OpenOptions::new().read(true).write(true).open("CONIN$"),
                OpenOptions::new().read(true).write(true).open("CONOUT$"),
            ) {
                (
                    Ok(conin.into_raw_handle() as HANDLE),
                    Ok(conout.into_raw_handle() as HANDLE),
                    true,
                )
            } else {
                (
                    get_std_handle(console::STD_INPUT_HANDLE),
                    get_std_handle(console::STD_OUTPUT_HANDLE),
                    false,
                )
            }
        } else {
            (
                get_std_handle(console::STD_INPUT_HANDLE),
                get_std_handle(console::STD_OUTPUT_HANDLE),
                false,
            )
        };
        let conin_isatty = match conin {
            Ok(handle) => {
                // If this function doesn't fail then fd is a TTY
                get_console_mode(handle).is_ok()
            }
            Err(_) => false,
        };

        let conout_isatty = match conout {
            Ok(handle) => {
                // If this function doesn't fail then fd is a TTY
                get_console_mode(handle).is_ok()
            }
            Err(_) => false,
        };

        Ok(Console {
            conin_isatty,
            conin: conin.unwrap_or(0),
            conout_isatty,
            conout: conout.unwrap_or(0),
            close_on_drop,
            color_mode,
            ansi_colors_supported: false,
            enable_bracketed_paste,
            vt_input: false,
            bell_style,
            raw_mode: Arc::new(AtomicBool::new(false)),
            pipe_reader: None,
            pipe_writer: None,
        })
    }

    /// Checking for an unsupported TERM in windows is a no-op
    fn is_unsupported(&self) -> bool {
        false
    }

    fn is_input_tty(&self) -> bool {
        self.conin_isatty
    }

    fn is_output_tty(&self) -> bool {
        self.conout_isatty
    }

    // pub fn install_sigwinch_handler(&mut self) {
    // See ReadConsoleInputW && WINDOW_BUFFER_SIZE_EVENT
    // }

    /// Enable RAW mode for the terminal.
    fn enable_raw_mode(&mut self) -> Result<(ConsoleMode, ConsoleKeyMap)> {
        if !self.conin_isatty {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "no stdio handle available for this process",
            )
            .into());
        }
        // Read both original modes before changing either. The restore object
        // also rolls back partially completed setup if an API call fails.
        let mut restore = ConsoleMode {
            original_conin_mode: get_console_mode(self.conin)?,
            conin: self.conin,
            original_conout_mode: if self.conout_isatty {
                Some(get_console_mode(self.conout)?)
            } else {
                None
            },
            conout: self.conout,
            raw_mode: Arc::clone(&self.raw_mode),
            bracketed_paste: false,
        };
        let setup = (|| -> Result<()> {
            let want_paste = self.enable_bracketed_paste && self.conout_isatty;
            let base_input = restore.original_conin_mode
                & !(console::ENABLE_LINE_INPUT
                    | console::ENABLE_ECHO_INPUT
                    | console::ENABLE_PROCESSED_INPUT)
                | console::ENABLE_EXTENDED_FLAGS
                | console::ENABLE_INSERT_MODE
                | console::ENABLE_QUICK_EDIT_MODE
                | console::ENABLE_WINDOW_INPUT;
            let mut input_mode = base_input;
            if want_paste {
                input_mode |= console::ENABLE_VIRTUAL_TERMINAL_INPUT;
            }
            if unsafe { console::SetConsoleMode(self.conin, input_mode) } == FALSE {
                // Older classic consoles may reject VT input. Keep their
                // virtual-key editor and clipboard paste without advertising
                // a protocol whose delimiters that console would discard.
                if input_mode != base_input {
                    input_mode = base_input;
                    check(unsafe { console::SetConsoleMode(self.conin, input_mode) })?;
                } else {
                    return Err(io::Error::last_os_error().into());
                }
            }
            self.vt_input = input_mode & console::ENABLE_VIRTUAL_TERMINAL_INPUT != 0;
            self.ansi_colors_supported = false;
            if let Some(original_output) = restore.original_conout_mode {
                let mut output_mode = original_output | console::ENABLE_WRAP_AT_EOL_OUTPUT;
                // Protocol negotiation needs VT output even when visible
                // coloring is disabled; colors_enabled still honors that choice.
                if self.color_mode != ColorMode::Disabled || want_paste {
                    output_mode |= console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                } else {
                    output_mode &= !console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                }
                if unsafe { console::SetConsoleMode(self.conout, output_mode) } == FALSE {
                    output_mode &= !console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                    check(unsafe { console::SetConsoleMode(self.conout, output_mode) })?;
                }
                self.ansi_colors_supported =
                    output_mode & console::ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0;
            }
            if want_paste && self.vt_input && self.ansi_colors_supported {
                // Mark it before writing so even a partial failed write triggers
                // the matching disable sequence during rollback.
                restore.bracketed_paste = true;
                write_all(self.conout, BRACKETED_PASTE_ON)?;
            }
            Ok(())
        })();
        if let Err(error) = setup {
            let _ = restore.disable_raw_mode();
            return Err(error);
        }
        self.raw_mode.store(true, Ordering::SeqCst);
        // The restore guard itself owns one extra reference.
        if Arc::strong_count(&self.raw_mode) == 2 {
            self.pipe_writer = None;
            self.pipe_reader = None;
        }
        Ok((restore, ()))
    }

    fn create_reader(
        &self,
        buffer: Option<ConsoleBuffer>,
        config: &Config,
        _: ConsoleKeyMap,
    ) -> ConsoleRawReader {
        ConsoleRawReader::create(
            self.conin,
            self.pipe_reader.clone(),
            self.vt_input,
            config,
            buffer,
        )
    }

    fn create_writer(&self) -> ConsoleRenderer {
        ConsoleRenderer::new(self.conout, self.colors_enabled(), self.bell_style)
    }

    fn writeln(&self) -> Result<()> {
        write_all(self.conout, &[10; 1])
    }

    fn create_external_printer(&mut self) -> Result<ExternalPrinter> {
        if let Some(ref sender) = self.pipe_writer {
            return Ok(ExternalPrinter {
                event: self.pipe_reader.as_ref().unwrap().event.0,
                sender: sender.clone(),
                raw_mode: self.raw_mode.clone(),
                conout: self.conout,
            });
        }
        if !self.is_input_tty() || !self.is_output_tty() {
            Err(io::Error::from(io::ErrorKind::Other))?; // FIXME
        }
        let event = unsafe { threading::CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null()) };
        if event == 0 {
            Err(io::Error::last_os_error())?;
        }
        let (sender, receiver) = sync_channel(1);

        let reader = Rc::new(AsyncPipe {
            event: Handle(event),
            receiver,
        });
        self.pipe_reader.replace(reader);
        self.pipe_writer.replace(sender.clone());
        Ok(ExternalPrinter {
            event,
            sender,
            raw_mode: self.raw_mode.clone(),
            conout: self.conout,
        })
    }

    fn set_cursor_visibility(&mut self, visible: bool) -> Result<Option<ConsoleCursorGuard>> {
        if self.conout_isatty {
            set_cursor_visibility(self.conout, visible)
        } else {
            Ok(None)
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if self.close_on_drop {
            unsafe { foundation::CloseHandle(self.conin) };
            unsafe { foundation::CloseHandle(self.conout) };
        }
    }
}

unsafe impl Send for Console {}
unsafe impl Sync for Console {}

#[derive(Debug)]
struct AsyncPipe {
    event: Handle,
    receiver: Receiver<String>,
}

#[derive(Debug)]
pub struct ExternalPrinter {
    event: HANDLE,
    sender: SyncSender<String>,
    raw_mode: Arc<AtomicBool>,
    conout: HANDLE,
}

unsafe impl Send for ExternalPrinter {}
unsafe impl Sync for ExternalPrinter {}

impl super::ExternalPrinter for ExternalPrinter {
    fn print(&mut self, msg: String) -> Result<()> {
        // write directly to stdout/stderr while not in raw mode
        if !self.raw_mode.load(Ordering::SeqCst) {
            let mut utf16 = Vec::new();
            write_to_console(self.conout, msg.as_str(), &mut utf16)
        } else {
            self.sender
                .send(msg)
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?; // FIXME
            Ok(check(unsafe { threading::SetEvent(self.event) })?)
        }
    }
}

#[derive(Debug)]
struct Handle(HANDLE);

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { foundation::CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod test {
    use super::Console;

    #[test]
    fn test_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Console>();
    }

    #[test]
    fn test_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Console>();
    }
}
