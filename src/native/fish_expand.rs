//! Fish quoting and list expansion for the native evaluator's initial subset.
use super::{Error, Result, unsupported};

#[derive(Clone, Copy, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Case arguments are expanded first, then their remaining stars become match
/// wildcards. A quoted backslash-star pair matches a literal star. Question
/// marks follow Fish's default question-mark-noglob behavior.
pub(super) fn wildcard_match(pattern: &str, value: &str) -> bool {
    let mut items = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => items.push(Item::Star),
            '\\' if chars.peek() == Some(&'*') => {
                chars.next();
                items.push(Item::Literal('*'));
            }
            '\\' if chars.peek() == Some(&'\\') => {
                chars.next();
                items.extend([Item::Literal('\\'), Item::Literal('\\')]);
            }
            c => items.push(Item::Literal(c)),
        }
    }
    glob::matches(&items, value)
}

pub(super) fn validate(input: &str) -> Result<()> {
    syntax(input, 0)
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
    // Start at the already recognized opening parenthesis. Its body has its
    // own quote context, including comments and nested quoted substitutions.
    let input: &fish_widestring::wstr = chars.into();
    let mut cursor = start - 1;
    let substitution =
        crate::commandline_extent::locate_cmdsubst_range(input, &mut cursor, false, None, None)
            .map_err(|()| Error::new("unterminated command substitution"))?
            .ok_or_else(|| Error::new("unterminated command substitution"))?;
    super::check_syntax_depth(&input[substitution.command_range()].to_string(), depth + 1)?;
    Ok(substitution.end())
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

#[path = "fish_glob.rs"]
mod glob;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    Literal(char),
    Star,
}

pub(super) enum Request<'a> {
    Variable(&'a str),
    /// The one-character delimiter used by this variable's quoted expansion.
    VariableSeparator(&'a str),
    Command {
        source: &'a str,
        quoted: bool,
    },
}

pub(super) fn split_substitution(bytes: &[u8], quoted: bool) -> Result<Vec<String>> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Error::new("command substitution output limit exceeded"));
    }
    let value = std::str::from_utf8(bytes)
        .map_err(|_| Error::new("command substitution output is not UTF-8"))?;
    if quoted {
        Ok(vec![value.trim_end_matches('\n').to_owned()])
    } else {
        // Native programs ordinarily write Windows CRLF. Treat each CRLF as a
        // line delimiter while preserving a lone carriage return as content.
        Ok(value
            .split_inclusive('\n')
            .map(|line| {
                line.strip_suffix('\n')
                    .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line))
                    .to_owned()
            })
            .collect())
    }
}

pub(super) fn split_separated_substitution(
    buffer: &crate::separated_buffer::SeparatedBuffer,
    quoted: bool,
) -> Result<Vec<String>> {
    if buffer.discarded() || buffer.len() > 16 * 1024 * 1024 {
        return Err(Error::new("command substitution output limit exceeded"));
    }
    let mut fields = Vec::new();
    for element in buffer.elements() {
        if element.is_explicitly_separated() {
            fields.push(
                std::str::from_utf8(&element.contents)
                    .map_err(|_| Error::new("command substitution output is not UTF-8"))?
                    .to_owned(),
            );
        } else if quoted {
            let contents = std::str::from_utf8(&element.contents)
                .map_err(|_| Error::new("command substitution output is not UTF-8"))?;
            fields.extend(contents.split_terminator('\n').map(str::to_owned));
        } else {
            fields.extend(split_substitution(&element.contents, false)?);
        }
    }
    if quoted {
        Ok(vec![fields.join("\n").trim_end_matches('\n').to_owned()])
    } else {
        Ok(fields)
    }
}

pub(super) fn word(
    input: &str,
    mut variable: impl FnMut(&str) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    word_with(input, |request| match request {
        Request::Variable(name) => variable(name),
        Request::VariableSeparator(name) => {
            Ok(vec![if name.ends_with("PATH") { ";" } else { " " }.into()])
        }
        Request::Command { .. } => Err(unsupported("command substitutions in literal expansion")),
    })
}

pub(super) fn word_with(
    input: &str,
    mut expand: impl FnMut(Request<'_>) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let words = expand_chars(&input.chars().collect::<Vec<_>>(), &mut expand, 0)?;
    glob::expand(words)
}

fn literal(value: &str) -> Vec<Item> {
    value.chars().map(Item::Literal).collect()
}
fn rendered(items: &[Item]) -> String {
    items
        .iter()
        .map(|item| match item {
            Item::Literal(c) => *c,
            Item::Star => '*',
        })
        .collect()
}
fn append(results: &mut Vec<Vec<Item>>, values: Vec<Vec<Item>>) -> Result<()> {
    if results.len().saturating_mul(values.len()) > 100_000 {
        return Err(Error::new("too many Fish expansion results"));
    }
    let mut combined = Vec::new();
    // Fish expands the rightmost list on the outside of the Cartesian product.
    for suffix in &values {
        for prefix in results.iter() {
            let mut value = prefix.clone();
            value.extend(suffix.clone());
            combined.push(value);
        }
    }
    *results = combined;
    Ok(())
}

fn expand_chars(
    chars: &[char],
    expand: &mut impl FnMut(Request<'_>) -> Result<Vec<String>>,
    depth: usize,
) -> Result<Vec<Vec<Item>>> {
    if depth >= 128 {
        return Err(Error::new("maximum Fish expansion nesting exceeded"));
    }
    let mut results = vec![Vec::new()];
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
                if let Some(c) = escape_char(chars, &mut index, quote)? {
                    for value in &mut results {
                        value.push(Item::Literal(c));
                    }
                }
            }
            '$' if quote != Quote::Single && chars.get(index) != Some(&'(') => {
                let mut levels = 1;
                while chars.get(index) == Some(&'$') {
                    index += 1;
                    levels += 1;
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
                if quote == Quote::Double {
                    let value = quoted_variable(&name, levels, chars, &mut index, expand, depth)?;
                    append(&mut results, vec![literal(&value)])?;
                    continue;
                }
                let mut values = expand(Request::Variable(&name))?;
                for level in 0..levels {
                    if level > 0 {
                        let mut indirect = Vec::new();
                        for name in values {
                            indirect.extend(expand(Request::Variable(&name))?);
                        }
                        values = indirect;
                    }
                    if chars.get(index) == Some(&'[') {
                        values = slice(chars, &mut index, values, expand, depth)?;
                    }
                }
                append(
                    &mut results,
                    values.iter().map(|value| literal(value)).collect(),
                )?;
            }
            '$' if quote != Quote::Single && chars.get(index) == Some(&'(') => {
                index += 1;
                let end = substitution(chars, index, depth)?;
                let source: String = chars[index..end - 1].iter().collect();
                let mut values = expand(Request::Command {
                    source: &source,
                    quoted: quote == Quote::Double,
                })?;
                index = end;
                if chars.get(index) == Some(&'[') {
                    values = slice(chars, &mut index, values, expand, depth)?;
                }
                append(
                    &mut results,
                    values.iter().map(|value| literal(value)).collect(),
                )?;
            }
            '(' if quote == Quote::None => {
                let end = substitution(chars, index, depth)?;
                let source: String = chars[index..end - 1].iter().collect();
                let mut values = expand(Request::Command {
                    source: &source,
                    quoted: false,
                })?;
                index = end;
                if chars.get(index) == Some(&'[') {
                    values = slice(chars, &mut index, values, expand, depth)?;
                }
                append(
                    &mut results,
                    values.iter().map(|value| literal(value)).collect(),
                )?;
            }
            '{' if quote == Quote::None => {
                let BraceExpansion {
                    end,
                    parts,
                    expands,
                } = braces(chars, index)?;
                if expands {
                    let mut alternatives = Vec::new();
                    for (start, end) in parts {
                        let mut part = &chars[start..end];
                        while part.first().is_some_and(|c| c.is_whitespace()) {
                            part = &part[1..];
                        }
                        while part.last().is_some_and(|c| c.is_whitespace()) {
                            part = &part[..part.len() - 1];
                        }
                        alternatives.extend(expand_chars(part, expand, depth + 1)?);
                    }
                    append(&mut results, alternatives)?;
                } else {
                    let values = expand_chars(&chars[index..end - 1], expand, depth + 1)?;
                    let values = values
                        .into_iter()
                        .map(|mut value| {
                            value.insert(0, Item::Literal('{'));
                            value.push(Item::Literal('}'));
                            value
                        })
                        .collect();
                    append(&mut results, values)?;
                }
                index = end;
            }
            '~' if quote == Quote::None && index == 1 => {
                if chars.get(index).is_some_and(|c| *c != '/' && *c != '\\') {
                    return Err(unsupported("named-user home expansion"));
                }
                let home = expand(Request::Variable("HOME"))?.join(" ");
                append(&mut results, vec![literal(&home)])?;
            }
            '*' if quote == Quote::None => {
                for value in &mut results {
                    value.push(Item::Star);
                }
            }
            _ => {
                for value in &mut results {
                    value.push(Item::Literal(c));
                }
            }
        }
    }
    if quote != Quote::None {
        return Err(Error::new("unterminated quote"));
    }
    Ok(results)
}

/// Quoted indirection joins each variable before the next dereference. The
/// following name ends at its first non-name character; the remaining joined
/// text stays literal. This also ensures the final variable's own delimiter is
/// used, rather than the delimiter of the variable holding its name.
fn quoted_variable(
    name: &str,
    levels: usize,
    chars: &[char],
    index: &mut usize,
    expand: &mut impl FnMut(Request<'_>) -> Result<Vec<String>>,
    depth: usize,
) -> Result<String> {
    let mut name = name.to_owned();
    let mut suffix = String::new();
    for level in 0..levels {
        let mut values = if name.is_empty() {
            Vec::new()
        } else {
            expand(Request::Variable(&name))?
        };
        if suffix.is_empty() && chars.get(*index) == Some(&'[') {
            values = slice(chars, index, values, expand, depth)?;
        }
        let separators = expand(Request::VariableSeparator(&name))?;
        let separator = separators.first().map_or(" ", String::as_str);
        let mut joined = values.join(separator);
        joined.push_str(&suffix);
        if level + 1 == levels {
            return Ok(joined);
        }
        let end = joined
            .char_indices()
            .find(|(_, character)| !crate::common::valid_var_name_char(*character))
            .map_or(joined.len(), |(index, _)| index);
        // Fish protects an empty first value during recursive quoted expansion;
        // other values that do not begin with a name are invalid dereferences.
        if end == 0 && values.first().is_some_and(|value| !value.is_empty()) {
            return Err(Error::new("expected a Fish variable name after '$'"));
        }
        name = joined[..end].to_owned();
        suffix = joined[end..].to_owned();
    }
    unreachable!("a variable expansion has at least one level")
}

struct BraceExpansion {
    end: usize,
    parts: Vec<(usize, usize)>,
    expands: bool,
}

fn braces(chars: &[char], start: usize) -> Result<BraceExpansion> {
    let mut level = 1;
    let mut quote = Quote::None;
    let mut index = start;
    let mut part = start;
    let mut parts = Vec::new();
    let mut expands = false;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '\\' => index += usize::from(index + 1 < chars.len()),
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
            '{' if quote == Quote::None => level += 1,
            '}' if quote == Quote::None => {
                level -= 1;
                if level == 0 {
                    parts.push((part, index));
                    return Ok(BraceExpansion {
                        end: index + 1,
                        parts,
                        expands,
                    });
                }
            }
            ',' if quote == Quote::None && level == 1 => {
                parts.push((part, index));
                part = index + 1;
                expands = true;
            }
            '$' if quote != Quote::Single => expands = true,
            '(' if quote == Quote::None => {
                index = substitution(chars, index + 1, 0)? - 1;
            }
            _ => (),
        }
        index += 1;
    }
    Err(Error::new("unterminated brace expansion"))
}

fn slice(
    chars: &[char],
    index: &mut usize,
    values: Vec<String>,
    expand: &mut impl FnMut(Request<'_>) -> Result<Vec<String>>,
    depth: usize,
) -> Result<Vec<String>> {
    *index += 1;
    let start = *index;
    while *index < chars.len() && chars[*index] != ']' {
        *index += 1;
    }
    if *index == chars.len() {
        return Err(Error::new("unterminated variable index"));
    }
    let specs = expand_chars(&chars[start..*index], expand, depth + 1)?;
    *index += 1;
    let specs = specs
        .iter()
        .map(|items| rendered(items))
        .collect::<Vec<_>>()
        .join(" ");
    let mut selected = Vec::new();
    let len = values.len() as isize;
    let resolve = |text: &str, default: isize| -> Result<isize> {
        let n = if text.is_empty() {
            default
        } else {
            text.parse()
                .map_err(|_| Error::new("invalid Fish list index"))?
        };
        if n == 0 {
            return Err(Error::new("Fish list indexes start at one"));
        }
        Ok(if n < 0 { len + n } else { n - 1 })
    };
    for spec in specs.split_whitespace() {
        if let Some((left, right)) = spec.split_once("..") {
            let left = resolve(left, 1)?;
            let right = resolve(right, -1)?;
            if left <= right {
                for i in left.max(0)..=right.min(len - 1) {
                    selected.push(values[i as usize].clone());
                }
            } else {
                for i in (right.max(0)..=left.min(len - 1)).rev() {
                    selected.push(values[i as usize].clone());
                }
            }
        } else {
            let i = resolve(spec, 1)?;
            if i >= 0 {
                if let Some(value) = values.get(i as usize) {
                    selected.push(value.clone());
                }
            }
        }
        if selected.len() > 100_000 {
            return Err(Error::new("too many Fish list indices"));
        }
    }
    Ok(selected)
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
    fn quoted_variables_use_path_metadata_after_slicing_and_indirection() {
        let expand = |request: Request<'_>| {
            Ok(match request {
                Request::Variable(name) => match name {
                    "custom" | "UNPATH" => vec!["one".into(), "two".into()],
                    "ref" => vec!["custom".into()],
                    "refs" | "pathrefs" => vec!["custom".into(), "ordinary".into()],
                    "ordinary" => vec!["x".into(), "y".into()],
                    "empty_first" => vec!["".into(), "ordinary".into()],
                    _ => Vec::new(),
                },
                Request::VariableSeparator(name) => vec![
                    if matches!(name, "custom" | "pathrefs") {
                        ";"
                    } else {
                        " "
                    }
                    .into(),
                ],
                Request::Command { .. } => unreachable!(),
            })
        };
        for (source, expected) in [
            ("\"$custom\"", "one;two"),
            ("\"$UNPATH\"", "one two"),
            ("\"$custom[2..1]\"", "two;one"),
            ("\"$$ref\"", "one;two"),
            ("\"$$ref[1][2]\"", "two"),
            ("\"$$refs\"", "one;two ordinary"),
            ("\"$$pathrefs\"", "one;two;ordinary"),
            ("\"$$refs[2]\"", "x y"),
            ("\"$$missing\"", ""),
            ("\"$$empty_first\"", " ordinary"),
        ] {
            assert_eq!(word_with(source, expand).unwrap(), [expected], "{source}");
        }
        assert_eq!(word_with("$custom", expand).unwrap(), ["one", "two"]);
        assert_eq!(
            word_with("$$refs", expand).unwrap(),
            ["one", "two", "x", "y"]
        );
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
    fn structural_validation_does_not_execute_expansions() {
        for input in [
            "(echo hello)",
            "$(echo hello)",
            "*.txt",
            "{one,two}",
            "$items[1..2]",
        ] {
            assert!(validate(input).is_ok(), "{input}");
        }
        assert!(validate("$").is_err());
    }

    #[test]
    fn cartesian_braces_and_ranges_follow_fish_order() {
        let values = |name: &str| {
            Ok(if name == "a" {
                vec!["a1".into(), "a2".into()]
            } else {
                vec!["b1".into(), "b2".into()]
            })
        };
        assert_eq!(
            word("$a$b", values).unwrap(),
            ["a1b1", "a2b1", "a1b2", "a2b2"]
        );
        assert_eq!(
            word("{one,two}{A,B}", |_| Ok(vec![])).unwrap(),
            ["oneA", "twoA", "oneB", "twoB"]
        );
        assert_eq!(word("$a[2..1]", values).unwrap(), ["a2", "a1"]);
        assert_eq!(word("{single}", |_| Ok(vec![])).unwrap(), ["{single}"]);
        assert_eq!(word("{a{b,c}}", |_| Ok(vec![])).unwrap(), ["{ab}", "{ac}"]);
        assert_eq!(word("{a\\ b}", |_| Ok(vec![])).unwrap(), ["{a b}"]);
    }

    #[test]
    fn command_substitution_keeps_fish_line_splitting() {
        assert_eq!(
            split_substitution(b"a\n\nb\n\n", false).unwrap(),
            ["a", "", "b", ""]
        );
        assert_eq!(split_substitution(b"a\n\nb\n\n", true).unwrap(), ["a\n\nb"]);
        assert_eq!(
            split_substitution(b"a\r\n\r\nb\r\nlast\r", false).unwrap(),
            ["a", "", "b", "last\r"]
        );
        assert_eq!(
            split_substitution(b"a\r\nb\r\n", true).unwrap(),
            ["a\r\nb\r"]
        );
        assert_eq!(
            word_with("pre(echo value)", |request| match request {
                Request::Command { source, quoted } => {
                    assert_eq!(source, "echo value");
                    assert!(!quoted);
                    Ok(vec!["one".into(), "two".into()])
                }
                _ => unreachable!(),
            })
            .unwrap(),
            ["preone", "pretwo"]
        );
    }
}
