//! Fish's shell-facing printf conversions over the shared Rust formatter.
use rash::{Error, Result};
const LIMIT: usize = 16 * 1024 * 1024;

pub(super) fn escapes(input: &str, percent_b: bool) -> Result<(Vec<u8>, bool)> {
    let chars: Vec<char> = input.chars().collect();
    let mut output = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        if c != '\\' {
            output.extend(c.to_string().as_bytes());
            continue;
        }
        let Some(c) = chars.get(index).copied() else {
            output.push(b'\\');
            break;
        };
        index += 1;
        if c == 'c' {
            return Ok((output, true));
        }
        let simple = match c {
            'a' => Some(7),
            'b' => Some(8),
            'e' => Some(27),
            'f' => Some(12),
            'n' => Some(10),
            'r' => Some(13),
            't' => Some(9),
            'v' => Some(11),
            '\\' => Some(b'\\'),
            _ => None,
        };
        if let Some(byte) = simple {
            output.push(byte);
            continue;
        }
        let (radix, limit, mut value, mut consumed, unicode) = match c {
            'x' => (16, 2, 0u32, 0, false),
            'u' => (16, 4, 0, 0, true),
            'U' => (16, 8, 0, 0, true),
            '0' if percent_b => (8, 3, 0, 0, false),
            '0'..='7' if !percent_b => (8, 3, c.to_digit(8).unwrap(), 1, false),
            _ => {
                output.push(b'\\');
                output.extend(c.to_string().as_bytes());
                continue;
            }
        };
        while consumed < limit {
            let Some(digit) = chars.get(index).and_then(|c| c.to_digit(radix)) else {
                break;
            };
            value = value * radix + digit;
            index += 1;
            consumed += 1;
        }
        if consumed == 0 && c != '0' {
            return Err(Error::new("printf: numeric escape requires digits"));
        }
        if unicode {
            let character = char::from_u32(value)
                .ok_or_else(|| Error::new("printf: invalid Unicode escape"))?;
            output.extend(character.to_string().as_bytes());
        } else {
            output.push(value as u8);
        }
    }
    Ok((output, false))
}

fn integer(value: &str) -> Result<i64> {
    if value.starts_with(['\'', '"']) {
        return Ok(value.chars().nth(1).unwrap_or('\0') as i64);
    }
    let value = value.trim();
    if value.is_empty() {
        return Ok(0);
    }
    let (negative, number) = if let Some(value) = value.strip_prefix('-') {
        (true, value)
    } else {
        (false, value.strip_prefix('+').unwrap_or(value))
    };
    let (radix, digits) = if let Some(value) = number
        .strip_prefix("0x")
        .or_else(|| number.strip_prefix("0X"))
    {
        (16, value)
    } else if number.starts_with('0') && number.len() > 1 {
        (8, &number[1..])
    } else {
        (10, number)
    };
    let value = u64::from_str_radix(digits, radix)
        .map_err(|_| Error::new("printf: expected an integer"))?;
    if negative {
        Ok((value as i64).wrapping_neg())
    } else {
        Ok(value as i64)
    }
}

pub(super) fn format(args: &[String]) -> Result<Vec<u8>> {
    let fmt = args
        .first()
        .ok_or_else(|| Error::new("printf: expected a format"))?;
    let chars: Vec<char> = fmt.chars().collect();
    let mut output = Vec::new();
    let mut arg_index = 1;
    loop {
        let before = arg_index;
        let mut index = 0;
        while index < chars.len() {
            if chars[index] != '%' {
                let start = index;
                while index < chars.len() && chars[index] != '%' {
                    if chars[index] == '\\' && index + 1 < chars.len() {
                        index += 1;
                    }
                    index += 1;
                }
                let (bytes, stop) =
                    escapes(&chars[start..index].iter().collect::<String>(), false)?;
                output.extend(bytes);
                if stop {
                    return Ok(output);
                }
                continue;
            }
            index += 1;
            if chars.get(index) == Some(&'%') {
                output.push(b'%');
                index += 1;
                continue;
            }
            if chars.get(index) == Some(&'b') {
                let argument = args.get(arg_index).map(String::as_str).unwrap_or("");
                arg_index += usize::from(arg_index < args.len());
                let (bytes, stop) = escapes(argument, true)?;
                output.extend(bytes);
                index += 1;
                if stop {
                    return Ok(output);
                }
                continue;
            }
            let mut specification = String::from("%");
            let mut values = Vec::new();
            while chars.get(index).is_some_and(|c| "-+ #0'".contains(*c)) {
                specification.push(chars[index]);
                index += 1;
            }
            for precision in [false, true] {
                if precision {
                    if chars.get(index) != Some(&'.') {
                        break;
                    }
                    specification.push('.');
                    index += 1;
                }
                if chars.get(index) == Some(&'*') {
                    let value = integer(args.get(arg_index).map(String::as_str).unwrap_or(""))?;
                    if value.unsigned_abs() > LIMIT as u64 {
                        return Err(Error::new("printf: field width is too large"));
                    }
                    values.push(fish_printf::Arg::SInt(value));
                    arg_index += usize::from(arg_index < args.len());
                    specification.push('*');
                    index += 1;
                } else {
                    let start = index;
                    while chars.get(index).is_some_and(|c| c.is_ascii_digit()) {
                        specification.push(chars[index]);
                        index += 1;
                    }
                    if index > start
                        && chars[start..index]
                            .iter()
                            .collect::<String>()
                            .parse::<usize>()
                            .unwrap_or(usize::MAX)
                            > LIMIT
                    {
                        return Err(Error::new("printf: field width is too large"));
                    }
                }
            }
            while chars.get(index).is_some_and(|c| "hlLjzt".contains(*c)) {
                index += 1;
            }
            let conversion = *chars
                .get(index)
                .ok_or_else(|| Error::new("printf: incomplete conversion"))?;
            index += 1;
            specification.push(conversion);
            let argument = args.get(arg_index).map(String::as_str).unwrap_or("");
            arg_index += usize::from(arg_index < args.len());
            let value = match conversion {
                's' => fish_printf::Arg::Str(argument),
                'c' => fish_printf::Arg::UInt(argument.chars().next().unwrap_or('\0') as u64),
                'd' | 'i' => fish_printf::Arg::SInt(integer(argument)?),
                'u' | 'o' | 'x' | 'X' => fish_printf::Arg::UInt(integer(argument)? as u64),
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
                    let number = if argument.starts_with(['\'', '"']) {
                        integer(argument)? as f64
                    } else if argument.is_empty() {
                        0.0
                    } else {
                        argument
                            .parse::<f64>()
                            .map_err(|_| Error::new("printf: expected a number"))?
                    };
                    fish_printf::Arg::Float(number)
                }
                _ => {
                    return Err(Error::new(format!(
                        "printf: %{conversion}: invalid conversion specification"
                    )));
                }
            };
            values.push(value);
            let mut part = String::new();
            fish_printf::printf_c_locale(&mut part, specification.as_str(), &mut values)
                .map_err(|error| Error::new(format!("printf: {error:?}")))?;
            output.extend(part.as_bytes());
            if output.len() > LIMIT {
                return Err(Error::new("printf: output limit exceeded"));
            }
        }
        if arg_index == before || arg_index >= args.len() {
            return Ok(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fish_formatting_and_raw_bytes() {
        let args = ["[%s] %04d %.2f %c\\n", "two words", "12", "1.25", "文"].map(str::to_owned);
        assert_eq!(
            format(&args).unwrap(),
            "[two words] 0012 1.25 文\n".as_bytes()
        );
        assert_eq!(
            format(&["%s\\n".into(), "one".into(), "two".into()]).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            format(&["%b".into(), "\\xC3\\xA9\\cignored".into()]).unwrap(),
            "é".as_bytes()
        );
        assert!(format(&["%q".into(), "word".into()]).is_err());
    }
}
