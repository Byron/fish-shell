//! Native terminal metadata, emitted outside the line editor's prompt string.
//!
//! Rustyline understands SGR widths but not OSC widths. Keeping these sequences
//! separate also prevents a redraw from reporting the same command repeatedly.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
    SetConsoleMode,
};

pub(super) struct TerminalIntegration {
    console: Option<(HANDLE, u32)>,
    command_active: bool,
}

impl TerminalIntegration {
    /// Native consoles need VT processing enabled before they forward OSC
    /// metadata through ConPTY. Unsupported or redirected output simply omits
    /// metadata; it never prevents the shell from starting.
    pub(super) fn new(enabled: bool) -> Self {
        let console = if enabled
            && io::stdout().is_terminal()
            && !std::env::var("TERM").is_ok_and(|term| term == "dumb")
        {
            let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            let mut mode = 0;
            if !handle.is_null()
                && handle != INVALID_HANDLE_VALUE
                && unsafe { GetConsoleMode(handle, &mut mode) } != 0
                && unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } != 0
            {
                Some((handle, mode))
            } else {
                None
            }
        } else {
            None
        };
        Self {
            console,
            command_active: false,
        }
    }

    /// Report the evaluator's current directory immediately before a fresh
    /// prompt. Passing cwd explicitly keeps this independent of host globals.
    pub(super) fn prompt(&mut self, cwd: &Path) -> io::Result<()> {
        if self.console.is_none() {
            return Ok(());
        }
        let mut sequence = String::new();
        if let Some(uri) = file_uri(cwd) {
            sequence.push_str("\x1b]7;");
            sequence.push_str(&uri);
            sequence.push('\x07');
        }
        sequence.push_str("\x1b]133;A\x07");
        emit(&sequence)
    }

    /// Publish accepted source, never an autosuggestion. The encoded payload
    /// gives Rustty/tmuxx exact multiline command text without screen scraping.
    pub(super) fn command_start(&mut self, source: &str) -> io::Result<()> {
        if self.console.is_none() || source.trim().is_empty() {
            return Ok(());
        }
        emit(&command_start_sequence(source))?;
        self.command_active = true;
        Ok(())
    }

    pub(super) fn command_end(&mut self, status: i32) -> io::Result<()> {
        if !self.command_active {
            return Ok(());
        }
        self.command_active = false;
        emit(&format!("\x1b]133;D;{status}\x07"))
    }
}

impl Drop for TerminalIntegration {
    fn drop(&mut self) {
        if let Some((handle, mode)) = self.console {
            // Restore the mode with which the parent launched this shell.
            unsafe { SetConsoleMode(handle, mode) };
        }
    }
}

fn emit(sequence: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()
}

fn command_start_sequence(source: &str) -> String {
    let source = source.trim_end_matches(['\r', '\n']);
    // Rustty caps decoded command metadata at 16 KiB. Still report lifecycle
    // for larger inputs without sending an oversized terminal control string.
    if source.len() > 16_384 || source.contains('\0') {
        return "\x1b]133;C\x07".into();
    }
    format!(
        "\x1b]133;C;cmdline_url={}\x07",
        percent_encode(source, false)
    )
}

/// A native absolute drive or UNC path becomes a standards-shaped file URI.
/// Separators and the drive colon are structural; all other punctuation and
/// UTF-8 bytes are escaped, so no path can terminate or inject an OSC sequence.
fn file_uri(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let mut path = path.to_str()?.replace('\\', "/");
    if path
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/"))
    {
        path = format!("//{}", &path[8..]);
    } else if path.starts_with("//?/") {
        path.drain(..4);
    }
    if let Some(network) = path.strip_prefix("//") {
        let (server, rest) = network.split_once('/')?;
        if server.is_empty() || matches!(server, "." | "?") || rest.is_empty() {
            return None;
        }
        return Some(format!(
            "file://{}/{}",
            percent_encode(server, false),
            percent_encode(rest, true)
        ));
    }
    let bytes = path.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1..3) == Some(b":/") {
        Some(format!(
            "file:///{}:{}",
            &path[..1],
            percent_encode(&path[2..], true)
        ))
    } else {
        None
    }
}

fn percent_encode(value: &str, path: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || path && byte == b'/' {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_uri_preserves_drive_unicode_and_network_paths() {
        for (path, expected) in [
            (
                r"C:\Users\🐟 space\a#b%",
                "file:///C:/Users/%F0%9F%90%9F%20space/a%23b%25",
            ),
            (r"C:\", "file:///C:/"),
            (r"\\server\share\some dir", "file://server/share/some%20dir"),
            (r"\\?\C:\long path", "file:///C:/long%20path"),
            (
                r"\\?\UNC\server\share\🐟",
                "file://server/share/%F0%9F%90%9F",
            ),
        ] {
            assert_eq!(
                file_uri(Path::new(path)).as_deref(),
                Some(expected),
                "{path}"
            );
        }
        for path in ["relative", r"C:relative", r"\rooted", r"\\.\pipe\device"] {
            assert_eq!(file_uri(Path::new(path)), None, "{path}");
        }
    }

    #[test]
    fn metadata_escapes_controls_delimiters_and_multiline_source() {
        assert_eq!(
            command_start_sequence("echo 🐟;\necho '\x1b]133;D;0\x07'\n"),
            "\x1b]133;C;cmdline_url=echo%20%F0%9F%90%9F%3B%0Aecho%20%27%1B%5D133%3BD%3B0%07%27\x07"
        );
        assert_eq!(
            file_uri(Path::new("C:/a\x1b\x07;\n")),
            Some("file:///C:/a%1B%07%3B%0A".into())
        );
        assert_eq!(
            command_start_sequence(&"x".repeat(16_385)),
            "\x1b]133;C\x07"
        );
        assert_eq!(command_start_sequence("echo \0"), "\x1b]133;C\x07");
    }
}
