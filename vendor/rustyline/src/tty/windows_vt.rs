//! Pure decoder for Windows virtual-terminal console input.
//!
//! The transport supplies complete Unicode characters and owns the timeout for
//! a lone Escape. CSI and SS3 state survives fragmented reads and resize events.

use crate::keys::{KeyCode as K, KeyEvent, Modifiers as M};

#[derive(Default)]
pub(super) struct Decoder {
    state: State,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Sequence(Sequence),
    LinuxFunction,
}

struct Sequence {
    ss3: bool,
    parameters: String,
    invalid: bool,
}

impl Decoder {
    /// Decode one Unicode character without assuming console-read boundaries.
    pub(super) fn feed(&mut self, ch: char) -> Option<KeyEvent> {
        match std::mem::take(&mut self.state) {
            State::Ground => {
                if ch == '\x1b' {
                    self.state = State::Escape;
                    None
                } else {
                    Some(character(ch, M::NONE))
                }
            }
            State::Escape => {
                if ch == '\x1b' {
                    // Repeated Escape cancels once per press. The second one
                    // may still introduce a sequence or time out on its own.
                    self.state = State::Escape;
                    Some(KeyEvent(K::Esc, M::NONE))
                } else if ch == '[' || ch == 'O' {
                    self.state = State::Sequence(Sequence {
                        ss3: ch == 'O',
                        parameters: String::new(),
                        invalid: false,
                    });
                    None
                } else {
                    Some(character(ch, M::ALT))
                }
            }
            State::LinuxFunction => Some(match ch {
                'A'..='E' => KeyEvent(K::F(ch as u8 - b'A' + 1), M::NONE),
                '\x03' => KeyEvent::ctrl('C'),
                _ => unknown(),
            }),
            State::Sequence(mut sequence) => {
                if ch == '\x03' {
                    // A truncated terminal sequence must not trap interrupt.
                    return Some(KeyEvent::ctrl('C'));
                }
                if ch == '\x1b' {
                    // A new escape cancels a malformed or incomplete sequence.
                    self.state = State::Escape;
                    return Some(unknown());
                }
                if matches!(ch, '\x18' | '\x1a') {
                    return Some(unknown()); // ECMA-48 CAN/SUB cancellation
                }
                if !sequence.ss3 && sequence.parameters.is_empty() && ch == '[' {
                    self.state = State::LinuxFunction;
                    return None;
                }
                if ('\x40'..='\x7e').contains(&ch) {
                    return Some(if sequence.invalid {
                        unknown()
                    } else {
                        sequence_key(sequence.ss3, &sequence.parameters, ch).unwrap_or_else(unknown)
                    });
                }
                // Consume unrecognized parameter/intermediate bytes through
                // the final byte. They must never become editable input.
                if ch.is_ascii_digit() || ch == ';' {
                    if sequence.parameters.len() < 64 && !sequence.invalid {
                        sequence.parameters.push(ch);
                    } else {
                        sequence.invalid = true;
                    }
                } else {
                    sequence.invalid = true;
                }
                self.state = State::Sequence(sequence);
                None
            }
        }
    }

    /// Only a lone Escape needs a short transport timeout.
    pub(super) fn escape_pending(&self) -> bool {
        matches!(self.state, State::Escape)
    }

    pub(super) fn finish_escape(&mut self) -> KeyEvent {
        self.state = State::Ground;
        KeyEvent(K::Esc, M::NONE)
    }
}

fn character(ch: char, modifiers: M) -> KeyEvent {
    match ch {
        '\r' => KeyEvent(K::Enter, modifiers),
        '\t' if modifiers.contains(M::SHIFT) => KeyEvent(K::BackTab, modifiers - M::SHIFT),
        '\t' => KeyEvent(K::Tab, modifiers),
        '\x08' | '\x7f' => KeyEvent(K::Backspace, modifiers),
        _ => KeyEvent::normalize(KeyEvent::new(ch, modifiers)),
    }
}

fn unknown() -> KeyEvent {
    KeyEvent(K::UnknownEscSeq, M::NONE)
}

fn parameters(text: &str) -> Option<Vec<u32>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    text.split(';')
        .map(|parameter| {
            if parameter.is_empty() {
                Some(0)
            } else {
                parameter.parse().ok()
            }
        })
        .collect()
}

fn modifiers(value: u32) -> Option<M> {
    let bits = value.checked_sub(1)?;
    if bits > 7 {
        return None;
    }
    let mut result = M::NONE;
    if bits & 1 != 0 {
        result |= M::SHIFT;
    }
    if bits & 2 != 0 {
        result |= M::ALT;
    }
    if bits & 4 != 0 {
        result |= M::CTRL;
    }
    Some(result)
}

fn sequence_key(ss3: bool, text: &str, final_byte: char) -> Option<KeyEvent> {
    let parameters = parameters(text)?;
    if !ss3 && final_byte == '~' {
        return tilde_key(&parameters);
    }
    // CSI-u and xterm modifyOtherKeys preserve modified printable keys and
    // distinguish Alt-Enter from a literal Escape followed by Enter.
    if !ss3 && final_byte == 'u' {
        let (&codepoint, rest) = parameters.split_first()?;
        let modifier = match rest {
            [] => M::NONE,
            [value] => modifiers(*value)?,
            _ => return None,
        };
        return Some(character(char::from_u32(codepoint)?, modifier));
    }
    let modifier = match parameters.as_slice() {
        [] | [0 | 1] => M::NONE,
        [0 | 1, value] => modifiers(*value)?,
        _ => return None,
    };
    let code = match final_byte {
        'A' => K::Up,
        'B' => K::Down,
        'C' => K::Right,
        'D' => K::Left,
        'H' => K::Home,
        'F' => K::End,
        'P' => K::F(1),
        'Q' => K::F(2),
        'R' => K::F(3),
        'S' => K::F(4),
        'Z' if !ss3 => return Some(KeyEvent(K::BackTab, modifier - M::SHIFT)),
        // rxvt variants of modified arrow keys.
        'a'..='d' => {
            let code = match final_byte {
                'a' => K::Up,
                'b' => K::Down,
                'c' => K::Right,
                _ => K::Left,
            };
            return Some(KeyEvent(
                code,
                modifier | if ss3 { M::CTRL } else { M::SHIFT },
            ));
        }
        _ => return None,
    };
    Some(KeyEvent(code, modifier))
}

fn tilde_key(parameters: &[u32]) -> Option<KeyEvent> {
    if let [27, modifier, codepoint] = *parameters {
        return Some(character(char::from_u32(codepoint)?, modifiers(modifier)?));
    }
    let (&number, rest) = parameters.split_first()?;
    let modifier = match rest {
        [] => M::NONE,
        [value] => modifiers(*value)?,
        _ => return None,
    };
    let code = match number {
        1 | 7 => K::Home,
        2 => K::Insert,
        3 => K::Delete,
        4 | 8 => K::End,
        5 => K::PageUp,
        6 => K::PageDown,
        11..=15 => K::F((number - 10) as u8),
        17..=21 => K::F((number - 11) as u8),
        23..=24 => K::F((number - 12) as u8),
        200 if rest.is_empty() => K::BracketedPasteStart,
        201 if rest.is_empty() => K::BracketedPasteEnd,
        _ => return None,
    };
    Some(KeyEvent(code, modifier))
}

/// Accumulate a bracketed paste as literal text, never as individual key events.
#[derive(Default)]
pub(super) struct Paste {
    text: String,
    marker: usize,
    finished: bool,
}

impl Paste {
    pub(super) fn feed(&mut self, ch: char) -> bool {
        const END: &[u8] = b"\x1b[201~";
        if self.finished {
            return true;
        }
        if ch == char::from(END[self.marker]) {
            self.marker += 1;
            if self.marker == END.len() {
                self.marker = 0;
                self.finished = true;
            }
        } else {
            self.text
                .push_str(std::str::from_utf8(&END[..self.marker]).unwrap());
            self.marker = 0;
            if ch == '\x1b' {
                self.marker = 1;
            } else {
                self.text.push(ch);
            }
        }
        self.finished
    }

    pub(super) fn finish(mut self) -> String {
        const END: &str = "\x1b[201~";
        self.text.push_str(&END[..self.marker]);
        self.text.replace("\r\n", "\n").replace('\r', "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(text: &str) -> Vec<KeyEvent> {
        let mut decoder = Decoder::default();
        text.chars().filter_map(|ch| decoder.feed(ch)).collect()
    }

    #[test]
    fn unicode_control_keys_escape_and_alt() {
        assert_eq!(
            decode("東京🐟"),
            vec![
                KeyEvent::from('東'),
                KeyEvent::from('京'),
                KeyEvent::from('🐟')
            ]
        );
        assert_eq!(
            decode("\r\t\x7f\x08\x03\n"),
            vec![
                KeyEvent(K::Enter, M::NONE),
                KeyEvent(K::Tab, M::NONE),
                KeyEvent(K::Backspace, M::NONE),
                KeyEvent(K::Backspace, M::NONE),
                KeyEvent::ctrl('C'),
                KeyEvent::ctrl('J'),
            ]
        );
        assert_eq!(
            decode("\x1bf\x1b🐟\x1b\r\x1b\x03"),
            vec![
                KeyEvent::alt('f'),
                KeyEvent::alt('🐟'),
                KeyEvent(K::Enter, M::ALT),
                KeyEvent(K::Char('C'), M::CTRL_ALT),
            ]
        );
        let mut decoder = Decoder::default();
        assert_eq!(decoder.feed('\x1b'), None);
        assert!(decoder.escape_pending());
        assert_eq!(decoder.finish_escape(), KeyEvent(K::Esc, M::NONE));
        assert!(!decoder.escape_pending());
        assert_eq!(decoder.feed('x'), Some(KeyEvent::from('x')));
    }

    #[test]
    fn fragmented_csi_and_ss3_keys_preserve_state_and_modifiers() {
        for (sequence, key) in [
            ("\x1b[A", KeyEvent(K::Up, M::NONE)),
            ("\x1bOB", KeyEvent(K::Down, M::NONE)),
            ("\x1b[1;6C", KeyEvent(K::Right, M::CTRL_SHIFT)),
            ("\x1b[1;3D", KeyEvent(K::Left, M::ALT)),
            ("\x1bOH", KeyEvent(K::Home, M::NONE)),
            ("\x1b[F", KeyEvent(K::End, M::NONE)),
            ("\x1b[2~", KeyEvent(K::Insert, M::NONE)),
            ("\x1b[3;5~", KeyEvent(K::Delete, M::CTRL)),
            ("\x1b[5~", KeyEvent(K::PageUp, M::NONE)),
            ("\x1b[6~", KeyEvent(K::PageDown, M::NONE)),
            ("\x1b[Z", KeyEvent(K::BackTab, M::NONE)),
            ("\x1b[1;6Z", KeyEvent(K::BackTab, M::CTRL)),
            ("\x1b[200~", KeyEvent(K::BracketedPasteStart, M::NONE)),
            ("\x1b[201~", KeyEvent(K::BracketedPasteEnd, M::NONE)),
        ] {
            let mut decoder = Decoder::default();
            let mut chars = sequence.chars().peekable();
            while let Some(ch) = chars.next() {
                let result = decoder.feed(ch);
                assert_eq!(
                    result,
                    chars.peek().is_none().then_some(key),
                    "{sequence:?}"
                );
            }
            assert!(!decoder.escape_pending());
            assert_eq!(decoder.feed('x'), Some(KeyEvent::from('x')));
        }
    }

    #[test]
    fn repeated_escape_cancels_and_interrupt_releases_incomplete_sequences() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.feed('\x1b'), None);
        assert_eq!(decoder.feed('\x1b'), Some(KeyEvent(K::Esc, M::NONE)));
        assert!(decoder.escape_pending());
        assert_eq!(decoder.finish_escape(), KeyEvent(K::Esc, M::NONE));
        assert_eq!(decoder.feed('x'), Some(KeyEvent::from('x')));
        for prefix in ["\x1b[", "\x1b[123;", "\x1bO1;", "\x1b[["] {
            assert_eq!(
                decode(&format!("{prefix}\x03x")),
                vec![KeyEvent::ctrl('C'), KeyEvent::from('x')],
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn xterm_modifiers_and_control_characters_are_normalized() {
        for (value, modifier) in [
            (1, M::NONE),
            (2, M::SHIFT),
            (3, M::ALT),
            (4, M::ALT_SHIFT),
            (5, M::CTRL),
            (6, M::CTRL_SHIFT),
            (7, M::CTRL_ALT),
            (8, M::CTRL_ALT_SHIFT),
        ] {
            assert_eq!(
                decode(&format!("\x1b[1;{value}D")),
                vec![KeyEvent(K::Left, modifier)]
            );
        }
        assert_eq!(decode("\x1b[97;5u"), vec![KeyEvent::ctrl('A')]);
        assert_eq!(
            decode("\x1b[27;7;97~"),
            vec![KeyEvent(K::Char('A'), M::CTRL_ALT)]
        );
        assert_eq!(
            decode("\x1b[1;3u"),
            vec![KeyEvent(K::Char('A'), M::CTRL_ALT)]
        );
        assert_eq!(decode("\x1b[9;2u"), vec![KeyEvent(K::BackTab, M::NONE)]);
        assert_eq!(decode("\x1b[127;5u"), vec![KeyEvent(K::Backspace, M::CTRL)]);
    }

    #[test]
    fn function_keys_and_extended_character_protocols() {
        for (number, sequence) in [
            (1, "\x1bOP"),
            (2, "\x1bOQ"),
            (3, "\x1bOR"),
            (4, "\x1bOS"),
            (5, "\x1b[15~"),
            (6, "\x1b[17~"),
            (7, "\x1b[18~"),
            (8, "\x1b[19~"),
            (9, "\x1b[20~"),
            (10, "\x1b[21~"),
            (11, "\x1b[23~"),
            (12, "\x1b[24~"),
        ] {
            assert_eq!(decode(sequence), vec![KeyEvent(K::F(number), M::NONE)]);
        }
        assert_eq!(decode("\x1bO1;3P"), vec![KeyEvent(K::F(1), M::ALT)]);
        assert_eq!(decode("\x1b[[E"), vec![KeyEvent(K::F(5), M::NONE)]);
        assert_eq!(decode("\x1b[97;5u"), vec![KeyEvent::ctrl('A')]);
        assert_eq!(decode("\x1b[27;3;13~"), vec![KeyEvent(K::Enter, M::ALT)]);
        assert_eq!(decode("\x1b[128031u"), vec![KeyEvent::from('🐟')]);
    }

    #[test]
    fn unknown_and_oversized_sequences_never_leak_parameters_into_input() {
        for sequence in [
            "\x1b[999~",
            "\x1b[1;99A",
            "\x1b[?25h",
            "\x1b[<0;10;20M",
            "\x1b[1:2u",
            "\x1b[1 q",
            "\x1b[200;5~",
            "\x1bO99A",
        ] {
            assert_eq!(
                decode(&format!("{sequence}x")),
                vec![unknown(), KeyEvent::from('x')],
                "{sequence:?}"
            );
        }
        let oversized = format!("\x1b[{}~x", "9".repeat(1000));
        assert_eq!(decode(&oversized), vec![unknown(), KeyEvent::from('x')]);
        assert_eq!(
            decode("\x1b[12\x1b[A"),
            vec![unknown(), KeyEvent(K::Up, M::NONE)]
        );
    }

    #[test]
    fn bracketed_paste_is_literal_and_normalizes_line_endings() {
        let body = "echo 🐟\r\necho 東京\rthird\nfourth\t\x03\x1b[A\x1b[200~";
        let mut paste = Paste::default();
        for ch in body.chars() {
            assert!(!paste.feed(ch));
        }
        for (index, ch) in "\x1b[201~".chars().enumerate() {
            assert_eq!(paste.feed(ch), index == 5);
        }
        assert_eq!(
            paste.finish(),
            body.replace("\r\n", "\n").replace('\r', "\n")
        );
    }

    #[test]
    fn paste_marker_prefixes_and_overlaps_are_preserved() {
        for body in [
            "x\x1b",
            "x\x1b[201",
            "\x1b[20x",
            "\x1b[201\x1b",
            "\x1b\x1b[20",
        ] {
            let mut paste = Paste::default();
            for ch in body.chars() {
                assert!(!paste.feed(ch));
            }
            assert_eq!(paste.finish(), body);
            let mut paste = Paste::default();
            for ch in format!("{body}\x1b[201~").chars() {
                paste.feed(ch);
            }
            assert_eq!(paste.finish(), body);
        }
    }
}
