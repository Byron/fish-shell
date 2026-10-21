// Copyright (C) 1997-2005 Free Software Foundation, Inc.
// Rust adaptation: Copyright (C) 2026 fish-shell contributors.
// Source-guided from GNU Bash 3.2 builtins/printf.def; GPL-2.0-or-later.

//! Bash printf argument handling around Fish's existing Rust numeric formatter.

use crate::{Error, Result, arithmetic::Variables};
use fish_printf::Arg;
use std::ffi::CString;

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct Output {
    pub bytes: Vec<u8>,
    /// `-v` retains embedded NUL until the variable binding truncates it.
    pub value: Vec<u8>,
    pub diagnostics: Vec<String>,
    pub status: i32,
}

impl Output {
    fn error(&mut self, message: impl Into<String>) {
        self.diagnostics.push(message.into());
        self.status = 1;
    }
    fn append(&mut self, value: &[u8]) -> Result<()> {
        if self.value.len().saturating_add(value.len()) > OUTPUT_LIMIT {
            return Err(Error::new("printf: output limit exceeded"));
        }
        self.bytes.extend_from_slice(value);
        self.value.extend_from_slice(value);
        Ok(())
    }
    fn character(&mut self, value: &[u8]) -> Result<()> {
        let before = self.bytes.len();
        self.append(value)?;
        // 3.2's PF macro writes asprintf's %c result through fputs.
        if let Some(nul) = value.iter().position(|&c| c == 0) {
            self.bytes.truncate(before + nul);
        }
        Ok(())
    }
}

fn argument<'a>(args: &'a [Vec<u8>], next: &mut usize) -> &'a [u8] {
    if let Some(value) = args.get(*next) {
        *next += 1;
        value
    } else {
        b""
    }
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn integer(input: &[u8], unsigned: bool, output: &mut Output) -> u64 {
    if input.first().is_some_and(|c| b"'\"".contains(c)) {
        return input.get(1).copied().unwrap_or(0) as std::ffi::c_char as i64 as u64;
    }
    let mut at = input
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(input.len());
    if at == input.len() {
        if !input.is_empty() {
            output.error(format!(
                "{}: invalid number",
                String::from_utf8_lossy(input)
            ));
        }
        return 0;
    }
    let negative = input[at] == b'-';
    if input[at] == b'-' || input[at] == b'+' {
        at += 1;
    }
    let base = if input.get(at) == Some(&b'0') {
        if input.get(at + 1).is_some_and(|c| *c == b'x' || *c == b'X') {
            at += 2;
            16
        } else {
            8
        }
    } else {
        10
    };
    let start = at;
    let mut value = 0u64;
    let mut overflow = false;
    while let Some(digit) = input.get(at).copied().and_then(digit).filter(|&n| n < base) {
        value = value
            .checked_mul(u64::from(base))
            .and_then(|n| n.checked_add(u64::from(digit)))
            .unwrap_or_else(|| {
                overflow = true;
                u64::MAX
            });
        at += 1;
    }
    if at == start || at != input.len() {
        output.error(format!(
            "{}: invalid number",
            String::from_utf8_lossy(input)
        ));
        return 0;
    }
    let maximum = if unsigned {
        u64::MAX
    } else {
        i64::MAX as u64 + u64::from(negative)
    };
    if overflow || value > maximum {
        output.diagnostics.push(format!(
            "warning: {}: numerical result out of range",
            String::from_utf8_lossy(input)
        ));
        value = maximum;
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

fn floating(input: &[u8], output: &mut Output) -> f64 {
    if input.first().is_some_and(|c| b"'\"".contains(c)) {
        return f64::from(input.get(1).copied().unwrap_or(0) as std::ffi::c_char);
    }
    if input.is_empty() {
        return 0.0;
    }
    let text = String::from_utf8_lossy(input);
    let text = text.trim_start();
    if let Ok(value) = text.parse::<f64>() {
        return value;
    }
    // strtod accepts C hexadecimal floating point input; keep it independent of locale.
    let (negative, text) = if let Some(text) = text.strip_prefix('-') {
        (true, text)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        let (mantissa, exponent) = hex
            .find(['p', 'P'])
            .map_or((hex, "0"), |at| (&hex[..at], &hex[at + 1..]));
        if let Ok(exponent) = exponent.parse::<i32>() {
            let (mut value, mut fraction, mut valid) = (0.0, 0.0, false);
            for c in mantissa.bytes() {
                if c == b'.' && fraction == 0.0 {
                    fraction = 1.0;
                    continue;
                }
                let Some(n) = digit(c) else {
                    valid = false;
                    break;
                };
                valid = true;
                if fraction == 0.0 {
                    value = value * 16.0 + f64::from(n);
                } else {
                    fraction /= 16.0;
                    value += f64::from(n) * fraction;
                }
            }
            if valid {
                return if negative { -value } else { value } * 2.0f64.powi(exponent);
            }
        }
    }
    output.error(format!(
        "{}: invalid number",
        String::from_utf8_lossy(input)
    ));
    0.0
}

/// Decode one printf escape, retaining the distinct format and %b octal rules.
fn escape(input: &[u8], at: &mut usize, argument: bool, output: &mut Output) -> (Vec<u8>, bool) {
    *at += 1;
    let Some(&c) = input.get(*at) else {
        return (vec![b'\\'], false);
    };
    *at += 1;
    let byte = match c {
        b'a' => 7,
        b'b' => 8,
        b'e' | b'E' => 27,
        b'f' => 12,
        b'n' => 10,
        b'r' => 13,
        b't' => 9,
        b'v' => 11,
        b'\\' => b'\\',
        b'c' if argument => return (Vec::new(), true),
        b'\'' | b'"' | b'?' if !argument => c,
        b'0'..=b'7' => {
            let mut value = c - b'0';
            for _ in 0..(2 + usize::from(argument && c == b'0')) {
                let Some(next @ b'0'..=b'7') = input.get(*at).copied() else {
                    break;
                };
                value = value.wrapping_mul(8).wrapping_add(next - b'0');
                *at += 1;
            }
            value
        }
        b'x' => {
            let start = *at;
            let mut value = 0u8;
            for _ in 0..2 {
                let Some(next) = input.get(*at).copied().and_then(digit) else {
                    break;
                };
                value = value.wrapping_mul(16).wrapping_add(next);
                *at += 1;
            }
            if *at == start {
                output.diagnostics.push("missing hex digit for \\x".into());
                return (b"\\x".to_vec(), false);
            }
            value
        }
        _ => return (vec![b'\\', c], false),
    };
    (vec![byte], false)
}

fn expanded(input: &[u8], output: &mut Output) -> (Vec<u8>, bool) {
    let mut result = Vec::new();
    let mut at = 0;
    while at < input.len() {
        if input[at] == b'\\' {
            let (bytes, stop) = escape(input, &mut at, true, output);
            result.extend(bytes);
            if stop {
                return (result, true);
            }
        } else {
            result.push(input[at]);
            at += 1;
        }
    }
    (result, false)
}

fn number_field(input: &[u8], at: &mut usize) -> Result<usize> {
    let mut value = 0usize;
    while let Some(c @ b'0'..=b'9') = input.get(*at).copied() {
        value = value
            .saturating_mul(10)
            .saturating_add(usize::from(c - b'0'));
        *at += 1;
        if value > OUTPUT_LIMIT {
            return Err(Error::new(
                "printf: field width or precision exceeds output limit",
            ));
        }
    }
    Ok(value)
}

pub fn format(
    format: &[u8],
    args: &[Vec<u8>],
    variables: &mut (impl Variables + ?Sized),
) -> Result<Output> {
    let mut output = Output::default();
    let mut next = 0;
    loop {
        let initial = next;
        let pass_start = output.value.len();
        let mut at = 0;
        while at < format.len() {
            if format[at] == b'\\' {
                let (bytes, _) = escape(format, &mut at, false, &mut output);
                output.append(&bytes)?;
                continue;
            }
            if format[at] != b'%' {
                output.append(&format[at..at + 1])?;
                at += 1;
                continue;
            }
            at += 1;
            if format.get(at) == Some(&b'%') {
                output.append(b"%")?;
                at += 1;
                continue;
            }
            let flags_start = at;
            while format.get(at).is_some_and(|c| b"-+ #0'".contains(c)) {
                at += 1;
            }
            let mut flags = format[flags_start..at].to_vec();
            let width = if format.get(at) == Some(&b'*') {
                at += 1;
                let value = integer(argument(args, &mut next), false, &mut output) as i64;
                if value < 0 {
                    flags.push(b'-');
                }
                usize::try_from(value.unsigned_abs()).unwrap_or(usize::MAX)
            } else {
                number_field(format, &mut at)?
            };
            let precision = if format.get(at) == Some(&b'.') {
                at += 1;
                if format.get(at) == Some(&b'*') {
                    at += 1;
                    let value = integer(argument(args, &mut next), false, &mut output) as i64;
                    usize::try_from(value).ok()
                } else if format.get(at) == Some(&b'-') {
                    at += 1;
                    number_field(format, &mut at)?;
                    None
                } else {
                    Some(number_field(format, &mut at)?)
                }
            } else {
                None
            };
            if width > OUTPUT_LIMIT || precision.is_some_and(|p| p > OUTPUT_LIMIT) {
                return Err(Error::new(
                    "printf: field width or precision exceeds output limit",
                ));
            }
            while format.get(at).is_some_and(|c| b"hjlLtz".contains(c)) {
                at += 1;
            }
            let Some(&conversion) = format.get(at) else {
                output.error("missing format character");
                return Ok(output);
            };
            at += 1;
            let value = argument(args, &mut next);
            if conversion == b'n' {
                if !value.is_empty() {
                    if !value
                        .first()
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                        || !value
                            .iter()
                            .all(|c| c.is_ascii_alphanumeric() || *c == b'_')
                    {
                        output.error(format!(
                            "{}: not a valid identifier",
                            String::from_utf8_lossy(value)
                        ));
                        return Ok(output);
                    }
                    variables.set(
                        value,
                        (output.value.len() - pass_start).to_string().into_bytes(),
                    )?;
                }
                continue;
            }
            let mut stop = false;
            let mut text = match conversion {
                b's' => value.to_vec(),
                b'c' => vec![value.first().copied().unwrap_or(0)],
                b'b' => {
                    let result = expanded(value, &mut output);
                    stop = result.1;
                    result.0
                }
                b'q' => {
                    let value =
                        CString::new(value).map_err(|_| Error::new("NUL in printf argument"))?;
                    if value.is_empty() {
                        b"''".to_vec()
                    } else if crate::ansi_c::should_quote(&value, |c| {
                        c.is_ascii_graphic() || c == b' '
                    }) {
                        crate::ansi_c::quote(&value, |c| c.is_ascii_graphic() || c == b' ')
                            .unwrap()
                            .to_bytes()
                            .to_vec()
                    } else {
                        crate::quote::backslash_quote(&value).to_bytes().to_vec()
                    }
                }
                b'd' | b'i' | b'o' | b'u' | b'x' | b'X' | b'e' | b'E' | b'f' | b'F' | b'g'
                | b'G' | b'a' | b'A' => {
                    let arg = match conversion {
                        b'd' | b'i' => Arg::SInt(integer(value, false, &mut output) as i64),
                        b'o' | b'u' | b'x' | b'X' => Arg::UInt(integer(value, true, &mut output)),
                        _ => Arg::Float(floating(value, &mut output)),
                    };
                    let mut spec = String::from("%");
                    spec.push_str(std::str::from_utf8(&flags).unwrap());
                    if width != 0 {
                        spec.push_str(&width.to_string());
                    }
                    if let Some(precision) = precision {
                        spec.push('.');
                        spec.push_str(&precision.to_string());
                    }
                    spec.push(char::from(conversion));
                    let mut result = String::new();
                    if let Err(error) =
                        fish_printf::printf_c_locale(&mut result, spec.as_str(), &mut [arg])
                    {
                        output.error(format!("invalid format: {error:?}"));
                        return Ok(output);
                    }
                    result.into_bytes()
                }
                _ => {
                    output.error(format!(
                        "{}: invalid format character",
                        char::from(conversion)
                    ));
                    return Ok(output);
                }
            };
            if b"sbqc".contains(&conversion) {
                if conversion != b'c' {
                    if let Some(precision) = precision {
                        text.truncate(precision);
                    }
                }
                // Bash 3.2's printstr returns before padding an empty %b result.
                let padding = if conversion == b'b' && text.is_empty() {
                    0
                } else {
                    width.saturating_sub(text.len())
                };
                let mut field = Vec::new();
                if !flags.contains(&b'-') {
                    field.resize(padding, b' ');
                }
                field.extend(text);
                if flags.contains(&b'-') {
                    field.resize(field.len() + padding, b' ');
                }
                if conversion == b'c' {
                    output.character(&field)?;
                } else {
                    output.append(&field)?;
                }
            } else {
                output.append(&text)?;
            }
            if stop {
                return Ok(output);
            }
        }
        if next == initial || next >= args.len() {
            return Ok(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Vars;
    impl Variables for Vars {
        fn get(&mut self, _: &[u8]) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn set(&mut self, _: &[u8], _: Vec<u8>) -> Result<()> {
            Ok(())
        }
    }
    fn print(format: &[u8], args: &[&[u8]]) -> Output {
        super::format(
            format,
            &args.iter().map(|a| a.to_vec()).collect::<Vec<_>>(),
            &mut Vars,
        )
        .unwrap()
    }
    #[test]
    fn byte_strings_and_stop_escape() {
        assert_eq!(
            print(b"%b%s", &[b"before\\cafter", b"ignored"]).bytes,
            b"before"
        );
        assert_eq!(print(b"%s:%b", &[b"\xff", b"a\\000b"]).bytes, b"\xff:a\0b");
        assert_eq!(print(b"<%5.2s>", &[b"abc"]).bytes, b"<   ab>");
        assert!(print(b"%c", &[]).bytes.is_empty());
        assert_eq!(print(b"%c", &[]).value, b"\0");
    }
    #[test]
    fn numbers_width_and_repeated_format() {
        assert_eq!(print(b"%#08x", &[b"255"]).bytes, b"0x0000ff");
        assert_eq!(print(b"%d ", &[b"010", b"-2", b"0xff"]).bytes, b"8 -2 255 ");
        assert_eq!(print(b"%*.*f", &[b"8", b"2", b"1.25"]).bytes, b"    1.25");
        assert_eq!(print(b"%g", &[b"1000000"]).bytes, b"1e+06");
        assert_eq!(print(b"%d", &[b"123bad"]).bytes, b"0");
        assert_eq!(print(b"%d", &[b"123bad"]).status, 1);
    }
}
