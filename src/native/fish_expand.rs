//! Fish quoting and list expansion for the native evaluator's initial subset.
use super::{Error, Result, unsupported};

#[derive(Clone, Copy, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

pub(super) fn validate(input: &str) -> Result<()> {
    word(input, |_| Ok(Vec::new())).map(|_| ())
}

/// Syntax-only checking must accept valid features even when this execution
/// backend cannot run them. Catch invalid variable syntax inside those words.
pub(super) fn syntax(input: &str, depth: usize) -> Result<()> {
    let chars: Vec<char> = input.chars().collect();
    let mut quote = Quote::None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        match c {
            '\'' if quote != Quote::Double => {
                quote = if quote == Quote::Single {
                    Quote::None
                } else {
                    Quote::Single
                }
            }
            '"' if quote != Quote::Single => {
                quote = if quote == Quote::Double {
                    Quote::None
                } else {
                    Quote::Double
                }
            }
            '\\' => {
                if chars.get(index).is_some_and(|c| {
                    quote == Quote::None
                        || (quote == Quote::Single && matches!(c, '\\' | '\''))
                        || (quote == Quote::Double && matches!(c, '\\' | '"' | '$' | '\n'))
                }) {
                    index += 1;
                }
            }
            '$' if quote != Quote::Single => {
                while chars.get(index) == Some(&'$') {
                    index += 1;
                }
                if chars.get(index) == Some(&'(') {
                    index = substitution(&chars, index + 1, depth)?;
                } else if !chars
                    .get(index)
                    .is_some_and(|c| crate::common::valid_var_name_char(*c))
                {
                    return Err(Error::new("Expected a variable name after this $."));
                }
            }
            '(' if quote == Quote::None => index = substitution(&chars, index, depth)?,
            _ => (),
        }
    }
    Ok(())
}

fn substitution(chars: &[char], start: usize, depth: usize) -> Result<usize> {
    let mut quote = Quote::None;
    let mut nesting = 1;
    let mut index = start;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        match c {
            '\'' if quote != Quote::Double => {
                quote = if quote == Quote::Single {
                    Quote::None
                } else {
                    Quote::Single
                }
            }
            '"' if quote != Quote::Single => {
                quote = if quote == Quote::Double {
                    Quote::None
                } else {
                    Quote::Double
                }
            }
            '\\' => {
                index += usize::from(index < chars.len());
            }
            '(' if quote == Quote::None => nesting += 1,
            ')' if quote == Quote::None => {
                nesting -= 1;
                if nesting == 0 {
                    let source: String = chars[start..index - 1].iter().collect();
                    super::check_syntax_depth(&source, depth + 1)?;
                    return Ok(index);
                }
            }
            _ => (),
        }
    }
    Err(Error::new("unterminated command substitution"))
}

/// Called by the shared parser after checking that a token can contain a keyword.
pub(crate) fn unescape_literal(input: &str) -> Result<String> {
    let values = word(input, |_| Err(Error::new("not a literal")))?;
    Ok(values.into_iter().next().unwrap_or_default())
}

pub(super) fn echo(input: &str) -> Result<(String, bool)> {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    let mut result = String::new();
    while index < chars.len() {
        if chars[index] == '\\' {
            index += 1;
            if chars.get(index) == Some(&'c') {
                return Ok((result, true));
            }
            if let Some(c) = escape_char(&chars, &mut index, Quote::None)? {
                result.push(c);
            }
        } else {
            result.push(chars[index]);
            index += 1;
        }
    }
    Ok((result, false))
}

fn escape_char(chars: &[char], index: &mut usize, quote: Quote) -> Result<Option<char>> {
    let c = *chars
        .get(*index)
        .ok_or_else(|| Error::new("incomplete escape sequence"))?;
    if quote == Quote::Single && c != '\\' && c != '\'' {
        return Ok(Some('\\'));
    }
    if quote == Quote::Double && !matches!(c, '\\' | '"' | '$' | '\n') {
        return Ok(Some('\\'));
    }
    *index += 1;
    if c == '\n' {
        return Ok(None);
    }
    if quote != Quote::None {
        return Ok(Some(c));
    }
    let simple = match c {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'b' => '\x08',
        'e' => '\x1b',
        'f' => '\x0c',
        'v' => '\x0b',
        'a' => '\x07',
        _ => c,
    };
    let (radix, max, mut value, mut digits) = match c {
        'x' | 'X' => (16, 2, 0, 0),
        'u' => (16, 4, 0, 0),
        'U' => (16, 8, 0, 0),
        '0'..='7' => (8, 3, c.to_digit(8).unwrap(), 1),
        'c' => {
            let control = *chars
                .get(*index)
                .ok_or_else(|| Error::new("incomplete control escape"))?;
            *index += 1;
            if !control.is_ascii() {
                return Err(Error::new("invalid control escape"));
            }
            return Ok(Some(((control as u8) & 0x1f) as char));
        }
        _ => return Ok(Some(simple)),
    };
    while digits < max {
        let Some(digit) = chars.get(*index).and_then(|c| c.to_digit(radix)) else {
            break;
        };
        value = value * radix + digit;
        digits += 1;
        *index += 1;
    }
    if digits == 0 {
        return Err(Error::new("invalid numeric escape"));
    }
    if !matches!(c, 'u' | 'U') && value >= 128 {
        return Err(unsupported(
            "non-ASCII byte escapes; use Unicode text or \\u escapes",
        ));
    }
    char::from_u32(value)
        .map(Some)
        .ok_or_else(|| Error::new("invalid Unicode escape"))
}

pub(super) fn word(
    input: &str,
    mut variable: impl FnMut(&str) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let chars: Vec<char> = input.chars().collect();
    let mut results = vec![String::new()];
    let mut quote = Quote::None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        match c {
            '\'' if quote != Quote::Double => {
                quote = if quote == Quote::Single {
                    Quote::None
                } else {
                    Quote::Single
                };
            }
            '"' if quote != Quote::Single => {
                quote = if quote == Quote::Double {
                    Quote::None
                } else {
                    Quote::Double
                };
            }
            '\\' => {
                if let Some(c) = escape_char(&chars, &mut index, quote)? {
                    for result in &mut results {
                        result.push(c);
                    }
                }
            }
            '$' if quote != Quote::Single => {
                if chars.get(index) == Some(&'(') {
                    return Err(unsupported("command substitutions"));
                }
                let start = index;
                while chars
                    .get(index)
                    .is_some_and(|c| crate::common::valid_var_name_char(*c))
                {
                    index += 1;
                }
                if index == start {
                    return Err(Error::new("expected a Fish variable name after '$'"));
                }
                let name: String = chars[start..index].iter().collect();
                let mut values = variable(&name)?;
                if chars.get(index) == Some(&'[') {
                    index += 1;
                    let start = index;
                    while index < chars.len() && chars[index] != ']' {
                        index += 1;
                    }
                    if index == chars.len() {
                        return Err(Error::new("unterminated variable index"));
                    }
                    let slices: String = chars[start..index].iter().collect();
                    index += 1;
                    let mut selected = Vec::new();
                    for item in slices.split_whitespace() {
                        let n = item
                            .parse::<isize>()
                            .map_err(|_| unsupported("variable index ranges and expansions"))?;
                        let offset = if n > 0 {
                            n - 1
                        } else {
                            values.len() as isize + n
                        };
                        if n != 0 && offset >= 0 {
                            if let Some(value) = values.get(offset as usize) {
                                selected.push(value.clone());
                            }
                        }
                    }
                    values = selected;
                }
                if quote == Quote::Double {
                    values = vec![values.join(if name.ends_with("PATH") { ";" } else { " " })];
                }
                let mut expanded = Vec::new();
                for prefix in &results {
                    for value in &values {
                        if expanded.len() >= 100_000 {
                            return Err(Error::new("too many Fish expansion results"));
                        }
                        expanded.push(format!("{prefix}{value}"));
                    }
                }
                results = expanded;
            }
            '(' if quote == Quote::None => return Err(unsupported("command substitutions")),
            '~' if quote == Quote::None && index == 1 => {
                if chars.get(index).is_some_and(|c| *c != '/' && *c != '\\') {
                    return Err(unsupported("named-user home expansion"));
                }
                let home = variable("HOME")?.join(" ");
                for result in &mut results {
                    result.push_str(&home);
                }
            }
            '*' | '{' | '}' if quote == Quote::None => {
                return Err(unsupported("wildcards and brace expansion"));
            }
            _ => {
                for result in &mut results {
                    result.push(c);
                }
            }
        }
    }
    if quote != Quote::None {
        return Err(Error::new("unterminated quote"));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_lists_and_quotes() {
        let values = || |_: &str| Ok(vec!["one".into(), "two words".into()]);
        assert_eq!(
            word("pre$items", values()).unwrap(),
            ["preone", "pretwo words"]
        );
        assert_eq!(word("\"$items\"", values()).unwrap(), ["one two words"]);
        assert_eq!(word("'$items'", values()).unwrap(), ["$items"]);
        assert_eq!(word("$items[-1]", values()).unwrap(), ["two words"]);
        assert_eq!(
            word("$missing", |_| Ok(vec![])).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(word("\"$missing\"", |_| Ok(vec![])).unwrap(), [""]);
    }

    #[test]
    fn fish_escaped_keywords() {
        for input in ["i'f'", "\"if\"", "\\if", "\\x69f", "\\u0069f", "i\\\nf"] {
            assert_eq!(unescape_literal(input).unwrap(), "if", "{input}");
        }
        assert_eq!(
            unescape_literal("'C:\\tools\\bin'").unwrap(),
            "C:\\tools\\bin"
        );
        assert_eq!(
            unescape_literal("\"C:\\tools\\bin\"").unwrap(),
            "C:\\tools\\bin"
        );
        assert_eq!(unescape_literal("\\u6587").unwrap(), "文");
    }

    #[test]
    fn unsupported_expansions_are_detected_without_values() {
        for input in [
            "(echo hello)",
            "$(echo hello)",
            "*.txt",
            "{one,two}",
            "$items[1..2]",
        ] {
            assert!(validate(input).is_err(), "{input}");
        }
        assert!(validate("'*.txt'").is_ok());
        assert!(validate("'$(literal)'").is_ok());
    }
}
