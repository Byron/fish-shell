// Copyright (C) 2000 Free Software Foundation, Inc.
// Rust translation: Copyright (C) 2026 fish-shell contributors.
//
// This file is derived from GNU Bash 3.2.57, lib/sh/strtrans.c.
// You may redistribute and/or modify it under the GNU General Public License,
// version 2 or (at your option) any later version. It comes WITHOUT ANY WARRANTY;
// without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See ../COPYING for the full license.

//! Bash 3.2's ANSI-C quoting and escape decoding, operating on bytes.
//!
//! Decoded output can contain NUL and is therefore not a C string. Character
//! classification is supplied by the caller to preserve Bash's locale behavior.
//! These helpers do not implement the Unicode escapes introduced in later Bash.

use std::ffi::{CStr, CString};
use std::ops::Range;

/// Decode for `echo -e`: octal escapes require a leading zero.
pub const ECHO: u8 = 1;
/// Decode for the parser: protect decoded CTLESC/CTLNUL and allow `\x{...}`.
pub const PARSER: u8 = 2;
/// Remove the backslash from unrecognized escape sequences.
pub const STRIP_UNKNOWN: u8 = 4;

const CTLESC: u8 = 1;
const CTLNUL: u8 = 127;

#[derive(Debug, Eq, PartialEq)]
pub struct Decoded {
    pub bytes: Vec<u8>,
    /// A recognized `\c` terminated decoding before the end of the input.
    pub saw_c: bool,
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `ansicstr`: decode escapes, optionally treating `\c` as an early terminator.
///
/// `flags` combines [`ECHO`], [`PARSER`], and [`STRIP_UNKNOWN`]. `to_upper`
/// supplies the locale's byte uppercase mapping for `\cX`. Empty input returns
/// `None`, matching the source helper; decoded NUL bytes remain in the result.
pub fn decode(
    input: &CStr,
    flags: u8,
    stop_at_c: bool,
    to_upper: impl Fn(u8) -> u8,
) -> Option<Decoded> {
    let input = input.to_bytes();
    if input.is_empty() {
        return None;
    }
    let mut result = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let mut c = input[i];
        i += 1;
        if c != b'\\' || i == input.len() {
            result.push(c);
            continue;
        }
        c = input[i];
        i += 1;
        match c {
            b'a' => c = 7,
            b'v' => c = 11,
            b'b' => c = 8,
            b'e' | b'E' => c = 27,
            b'f' => c = 12,
            b'n' => c = b'\n',
            b'r' => c = b'\r',
            b't' => c = b'\t',
            b'1'..=b'7' if flags & ECHO != 0 => result.push(b'\\'),
            b'0'..=b'7' => {
                let limit = 2 + usize::from(flags & ECHO != 0 && c == b'0');
                c -= b'0';
                for _ in 0..limit {
                    let Some(digit @ b'0'..=b'7') = input.get(i).copied() else {
                        break;
                    };
                    c = c.wrapping_mul(8).wrapping_add(digit - b'0');
                    i += 1;
                }
            }
            b'x' => {
                let braced = flags & PARSER != 0 && input.get(i) == Some(&b'{');
                if braced {
                    i += 1;
                }
                let start = i;
                c = 0;
                while let Some(digit) = input.get(i).copied().and_then(hex_value) {
                    if !braced && i - start == 2 {
                        break;
                    }
                    c = c.wrapping_mul(16).wrapping_add(digit);
                    i += 1;
                }
                if braced {
                    if input.get(i) == Some(&b'}') {
                        i += 1;
                    }
                } else if i == start {
                    result.push(b'\\');
                    c = b'x';
                }
            }
            b'\\' => {}
            b'\'' | b'"' | b'?' => {
                if flags & ECHO != 0 {
                    result.push(b'\\');
                }
            }
            b'c' => {
                if stop_at_c {
                    return Some(Decoded {
                        bytes: result,
                        saw_c: true,
                    });
                }
                if flags & ECHO == 0 {
                    c = input.get(i).copied().unwrap_or(0);
                    if c != 0 {
                        i += 1;
                        c = to_upper(c) & 0x1f;
                    } else if flags & STRIP_UNKNOWN == 0 {
                        result.push(b'\\');
                    }
                } else if flags & STRIP_UNKNOWN == 0 {
                    result.push(b'\\');
                }
            }
            _ => {
                if flags & STRIP_UNKNOWN == 0 {
                    result.push(b'\\');
                }
            }
        }
        if flags & PARSER != 0 && matches!(c, CTLESC | CTLNUL) {
            result.push(CTLESC);
        }
        result.push(c);
    }
    Some(Decoded {
        bytes: result,
        saw_c: false,
    })
}

/// `ansic_quote`: form a `$'...'` word using the locale's byte print predicate.
/// Empty input returns `None`, matching Bash's helper.
pub fn quote(input: &CStr, is_print: impl Fn(u8) -> bool) -> Option<CString> {
    if input.is_empty() {
        return None;
    }
    let mut result = b"$'".to_vec();
    for &c in input.to_bytes() {
        let escaped = match c {
            27 => Some(b'E'),
            7 => Some(b'a'),
            11 => Some(b'v'),
            8 => Some(b'b'),
            12 => Some(b'f'),
            b'\n' => Some(b'n'),
            b'\r' => Some(b'r'),
            b'\t' => Some(b't'),
            b'\\' | b'\'' => Some(c),
            _ => None,
        };
        if let Some(escaped) = escaped {
            result.extend_from_slice(&[b'\\', escaped]);
        } else if !is_print(c) {
            result.extend_from_slice(&[
                b'\\',
                b'0' + ((c >> 6) & 7),
                b'0' + ((c >> 3) & 7),
                b'0' + (c & 7),
            ]);
        } else {
            result.push(c);
        }
    }
    result.push(b'\'');
    Some(CString::new(result).expect("quoting cannot introduce NUL"))
}

/// `ansic_shouldquote`: whether any byte is nonprinting in the supplied locale.
pub fn should_quote(input: &CStr, is_print: impl Fn(u8) -> bool) -> bool {
    input.to_bytes().iter().any(|&c| !is_print(c))
}

/// `ansiexpand`: decode an input range for the parser, including an empty range.
/// Invalid bounds return `None`; an empty valid range returns an empty vector.
pub fn expand_range(
    input: &CStr,
    range: Range<usize>,
    to_upper: impl Fn(u8) -> u8,
) -> Option<Vec<u8>> {
    let bytes = input.to_bytes().get(range)?;
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let input = CString::new(bytes).expect("a C string range cannot contain NUL");
    Some(decode(&input, PARSER, false, to_upper).unwrap().bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(byte: u8) -> u8 {
        byte.to_ascii_uppercase()
    }

    fn printable(byte: u8) -> bool {
        (b' '..=b'~').contains(&byte)
    }

    #[test]
    fn decoding_preserves_nul_and_echo_octal_rules() {
        assert_eq!(decode(c"", 0, false, upper), None);
        assert_eq!(
            decode(c"\\0a\\x00b", 0, false, upper).unwrap().bytes,
            b"\0a\0b"
        );
        assert_eq!(decode(c"\\1234", 0, false, upper).unwrap().bytes, b"S4");
        assert_eq!(
            decode(c"\\1234", ECHO, false, upper).unwrap().bytes,
            b"\\1234"
        );
        assert_eq!(decode(c"\\01234", ECHO, false, upper).unwrap().bytes, b"S4");
        assert_eq!(decode(c"\\777", 0, false, upper).unwrap().bytes, [255]);
    }

    #[test]
    fn parser_flags_protect_only_decoded_markers() {
        assert_eq!(
            decode(c"\x01\\001\\177", PARSER, false, upper)
                .unwrap()
                .bytes,
            [1, 1, 1, 1, 127]
        );
        assert_eq!(
            decode(c"\\x{1234}\\x{}", PARSER, false, upper)
                .unwrap()
                .bytes,
            [0x34, 0]
        );
        assert_eq!(
            decode(c"\\x{12}", 0, false, upper).unwrap().bytes,
            b"\\x{12}"
        );
        assert_eq!(
            decode(c"\\q\\x!\\u0041", STRIP_UNKNOWN, false, upper)
                .unwrap()
                .bytes,
            b"q\\x!u0041"
        );
    }

    #[test]
    fn control_escape_can_stop_or_decode() {
        assert_eq!(
            decode(c"before\\cafter", ECHO, true, upper).unwrap(),
            Decoded {
                bytes: b"before".to_vec(),
                saw_c: true
            }
        );
        assert_eq!(decode(c"\\ca\\c?", 0, false, upper).unwrap().bytes, [1, 31]);
        assert_eq!(decode(c"\\c", 0, false, upper).unwrap().bytes, [b'\\', 0]);
        assert_eq!(decode(c"\\c", ECHO, false, upper).unwrap().bytes, b"\\c");
        assert_eq!(decode(c"\\c!", 0, false, |_| b'Z').unwrap().bytes, [26]);
    }

    #[test]
    fn quoting_uses_the_supplied_locale() {
        assert_eq!(quote(c"", printable), None);
        assert!(!should_quote(c"", printable));
        assert_eq!(
            quote(c"a\n\x1b'\\\xff", printable).unwrap().as_c_str(),
            c"$'a\\n\\E\\'\\\\\\377'"
        );
        assert!(should_quote(c"\xff", printable));
        assert!(!should_quote(c"\xff", |_| true));
        assert_eq!(quote(c"\xff", |_| true).unwrap().as_c_str(), c"$'\xff'");
    }

    #[test]
    fn parser_range_checks_bounds_and_keeps_empty_input() {
        assert_eq!(expand_range(c"pre\\001post", 3..7, upper), Some(vec![1, 1]));
        assert_eq!(expand_range(c"", 0..0, upper), Some(vec![]));
        assert_eq!(expand_range(c"abc", 3..3, upper), Some(vec![]));
        assert_eq!(expand_range(c"abc", 2..4, upper), None);
        let reversed = Range { start: 2, end: 1 };
        assert_eq!(expand_range(c"abc", reversed, upper), None);
    }
}
