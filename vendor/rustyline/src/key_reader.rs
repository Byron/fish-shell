//! Read individual Windows keys through the editor's console and VT decoder.
use crate::tty::{RawMode, RawReader, Term, Terminal};
use crate::{Config, KeyCode, KeyEvent, Result};

/// A standalone raw input session which restores console modes when dropped.
pub struct RawKeyReader {
    reader: <Terminal as Term>::Reader,
    mode: <Terminal as Term>::Mode,
    // Keep the console handles alive until the raw mode has been restored.
    _terminal: Terminal,
}

impl RawKeyReader {
    /// Open the standard console input without loading history or key bindings.
    pub fn new() -> Result<Self> {
        let config = Config::default();
        let mut terminal = Terminal::new(
            config.color_mode(),
            config.behavior(),
            config.tab_stop(),
            config.bell_style(),
            config.enable_bracketed_paste(),
            false,
        )?;
        let (mode, key_map) = terminal.enable_raw_mode()?;
        let reader = terminal.create_reader(None, &config, key_map);
        Ok(Self {
            reader,
            mode,
            _terminal: terminal,
        })
    }

    /// Read the next decoded key, retaining incomplete escape sequences.
    pub fn read_key(&mut self) -> Result<KeyEvent> {
        self.reader.next_key(false)
    }

    /// Consume the data following a bracketed-paste start event.
    pub fn read_paste(&mut self, key: KeyEvent) -> Result<Option<String>> {
        if key.0 == KeyCode::BracketedPasteStart {
            self.reader.read_pasted_text().map(Some)
        } else {
            Ok(None)
        }
    }
}

impl Drop for RawKeyReader {
    fn drop(&mut self) {
        let _ = self.mode.disable_raw_mode();
    }
}
