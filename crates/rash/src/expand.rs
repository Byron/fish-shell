// Copyright (C) 1987-2005 Free Software Foundation, Inc.
// Rust adaptation: Copyright (C) 2026 fish-shell contributors.
// Source-guided from GNU Bash 3.2 subst.c; GPL-2.0-or-later. See ../COPYING.

//! Execution-time word expansion. Syntax checking and completion must not call this module.

use crate::{Error, Result, arithmetic, os};
use std::ffi::CString;
use std::path::PathBuf;

pub trait ExpansionContext: arithmetic::Variables {
    fn positional(&self) -> Vec<Vec<u8>>;
    fn arg0(&self) -> Vec<u8>;
    fn status(&self) -> i32;
    fn pid(&self) -> u32;
    fn last_pid(&self) -> Option<u32>;
    fn cwd(&self) -> Vec<u8>;
    fn command_substitute(&mut self, source: &[u8]) -> Result<Vec<u8>>;
    fn interactive(&self) -> bool {
        false
    }
    fn option_flags(&self) -> Vec<u8> {
        Vec::new()
    }
    fn nounset(&self) -> bool {
        false
    }
    fn noglob(&self) -> bool {
        false
    }
    fn nullglob(&self) -> bool {
        false
    }
    fn failglob(&self) -> bool {
        false
    }
    fn extglob(&self) -> bool {
        false
    }
    fn home_for_user(&mut self, _name: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(Error::new(
            "named-user tilde expansion is not available in this context",
        ))
    }
    fn array(&mut self, _name: &[u8]) -> Result<Option<Vec<Vec<u8>>>> {
        Err(Error::new(
            "array expansion is not available in this context",
        ))
    }
}

pub use ExpansionContext as Context;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Word,
    Assignment,
    Redirect,
}

fn expansion_error(mut error: Error) -> Error {
    if error.status == 2 {
        error.status = 1;
    }
    error
}

fn parameter_error(message: String, context: &(impl Context + ?Sized)) -> Error {
    Error {
        message,
        status: if context.interactive() { 1 } else { 127 },
    }
}

#[derive(Clone, Debug, Default)]
struct Field {
    bytes: Vec<u8>,
    quoted: Vec<bool>,
    split: Vec<bool>,
    force: bool,
}

impl Field {
    fn append(&mut self, bytes: &[u8], quoted: bool, split: bool) {
        self.bytes.extend_from_slice(bytes);
        self.quoted.resize(self.bytes.len(), quoted);
        self.split.resize(self.bytes.len(), split && !quoted);
        self.force |= quoted;
    }
    fn push_from(&mut self, other: &Self, at: usize) {
        self.bytes.push(other.bytes[at]);
        self.quoted.push(other.quoted[at]);
        self.split.push(false);
    }
}

fn ifs(context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    Ok(context.get(b"IFS")?.unwrap_or_else(|| b" \t\n".to_vec()))
}

fn join(values: &[Vec<u8>], separator: Option<u8>) -> Vec<u8> {
    let mut result = Vec::new();
    for (i, value) in values.iter().enumerate() {
        if i != 0
            && let Some(c) = separator
        {
            result.push(c);
        }
        result.extend_from_slice(value);
    }
    result
}

fn parameter(name: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Option<Vec<u8>>> {
    Ok(match name {
        b"?" => Some(context.status().to_string().into_bytes()),
        b"$" => Some(context.pid().to_string().into_bytes()),
        b"!" => context.last_pid().map(|pid| pid.to_string().into_bytes()),
        b"#" => Some(context.positional().len().to_string().into_bytes()),
        b"0" => Some(context.arg0()),
        b"-" => Some(context.option_flags()),
        b"*" | b"@" => Some(join(&context.positional(), ifs(context)?.first().copied())),
        _ if !name.is_empty() && name.iter().all(u8::is_ascii_digit) => {
            let index = std::str::from_utf8(name)
                .ok()
                .and_then(|s| s.parse::<usize>().ok());
            index
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| context.positional().get(n).cloned())
        }
        _ => context.get(name)?,
    })
}

fn name_end(input: &[u8]) -> usize {
    if input.first().is_some_and(|c| b"@*#?$!-".contains(c)) {
        return 1;
    }
    let mut at = 0;
    while input
        .get(at)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    {
        at += 1;
    }
    if input.get(at) == Some(&b'[') {
        let mut depth = 1;
        at += 1;
        while at < input.len() && depth != 0 {
            match input[at] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            at += 1;
        }
    }
    at
}

// The parser's public scanner handles nested command grammar without executing it.
fn expansion_end(input: &[u8], start: usize) -> Result<usize> {
    crate::syntax::expansion_end(input, start).map_err(|error| Error::new(error.message))
}

fn backtick_end(input: &[u8], start: usize) -> Result<(usize, Vec<u8>)> {
    let mut at = start + 1;
    let mut source = Vec::new();
    while at < input.len() {
        if input[at] == b'`' {
            return Ok((at + 1, source));
        }
        if input[at] == b'\\' && input.get(at + 1).is_some_and(|c| b"\\$`".contains(c)) {
            at += 1;
        }
        source.push(input[at]);
        at += 1;
    }
    Err(Error::new("unterminated command substitution"))
}

fn command_output(source: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    let mut output = context.command_substitute(source)?;
    output.retain(|&c| c != 0);
    while output.last() == Some(&b'\n') {
        output.pop();
    }
    Ok(output)
}

fn scalar(input: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    let fields = expand_raw(input, context, false, false)?;
    Ok(join(
        &fields.into_iter().map(|f| f.bytes).collect::<Vec<_>>(),
        ifs(context)?.first().copied(),
    ))
}

fn braced(input: &[u8], context: &mut (impl Context + ?Sized), quoted: bool) -> Result<Vec<Field>> {
    if input.is_empty() {
        return Err(Error::new("bad substitution"));
    }
    if input[0] == b'#' && input.len() > 1 {
        let name = &input[1..];
        let size = if name == b"@" || name == b"*" {
            context.positional().len()
        } else if name.ends_with(b"[@]") || name.ends_with(b"[*]") {
            context
                .array(&name[..name.len() - 3])?
                .unwrap_or_default()
                .len()
        } else {
            let value = parameter(name, context)?.unwrap_or_default();
            if unicode_locale(context)? {
                boundaries(&value, true).len() - 1
            } else {
                value.len()
            }
        };
        return Ok(expansion_fields(
            vec![size.to_string().into_bytes()],
            quoted,
        ));
    }
    let indirect = input[0] == b'!';
    let input = if indirect { &input[1..] } else { input };
    let end = name_end(input);
    if end == 0 {
        return Err(Error::new("bad substitution"));
    }
    let original = &input[..end];
    let name = if indirect {
        parameter(original, context)?.ok_or_else(|| Error::new("invalid indirect expansion"))?
    } else {
        original.to_vec()
    };
    let mut tail = &input[end..];
    if tail.is_empty() && (name == b"@" || name == b"*") {
        let values = context.positional();
        return Ok(expansion_fields(
            if name == b"@" {
                values
            } else {
                vec![join(&values, ifs(context)?.first().copied())]
            },
            quoted,
        ));
    }
    if tail.is_empty() && (name.ends_with(b"[@]") || name.ends_with(b"[*]")) {
        let values = context.array(&name[..name.len() - 3])?.unwrap_or_default();
        return Ok(expansion_fields(
            if name.ends_with(b"[@]") {
                values
            } else {
                vec![join(&values, ifs(context)?.first().copied())]
            },
            quoted,
        ));
    }
    let value = parameter(&name, context)?;
    if tail.is_empty() {
        if value.is_none() && context.nounset() {
            return Err(parameter_error(
                format!("{}: unbound variable", String::from_utf8_lossy(&name)),
                context,
            ));
        }
        return Ok(expansion_fields(vec![value.unwrap_or_default()], quoted));
    }
    let colon = tail.starts_with(b":");
    if colon && tail.get(1).is_some_and(|c| b"-+=?".contains(c)) {
        tail = &tail[1..];
    }
    if tail.first().is_some_and(|c| b"-+=?".contains(c)) {
        let absent = value.is_none() || colon && value.as_ref().is_some_and(Vec::is_empty);
        let operand = &tail[1..];
        let use_operand =
            tail[0] == b'-' && absent || tail[0] == b'+' && !absent || tail[0] == b'=' && absent;
        if tail[0] == b'?' && absent {
            let message = if operand.is_empty() {
                b"parameter null or not set".to_vec()
            } else {
                scalar(operand, context)?
            };
            return Err(parameter_error(
                format!(
                    "{}: {}",
                    String::from_utf8_lossy(&name),
                    String::from_utf8_lossy(&message)
                ),
                context,
            ));
        }
        if use_operand {
            let mut fields = expand_raw(operand, context, false, false)?;
            for field in &mut fields {
                for at in 0..field.bytes.len() {
                    field.quoted[at] |= quoted;
                    field.split[at] = !field.quoted[at];
                }
                field.force |= quoted;
            }
            if tail[0] == b'=' {
                let values: Vec<_> = fields.iter().map(|f| f.bytes.clone()).collect();
                let value = join(&values, ifs(context)?.first().copied());
                context.set(&name, value)?;
            }
            return Ok(fields);
        }
        return Ok(expansion_fields(
            vec![if tail[0] == b'+' {
                Vec::new()
            } else {
                value.unwrap_or_default()
            }],
            quoted,
        ));
    }
    let value = value.unwrap_or_default();
    if tail.starts_with(b":") {
        let rest = &tail[1..];
        let split = rest.iter().position(|&c| c == b':');
        let offset_text = scalar(split.map_or(rest, |n| &rest[..n]), context)?;
        let offset = arithmetic::eval(&offset_text, context)?;
        let positions = if name == b"@" || name == b"*" {
            let mut args = vec![context.arg0()];
            args.extend(context.positional());
            Some(args)
        } else {
            None
        };
        let boundaries = boundaries(&value, unicode_locale(context)?);
        let count = positions.as_ref().map_or(boundaries.len() - 1, Vec::len) as i64;
        let start = if offset < 0 {
            count.saturating_add(offset)
        } else {
            offset
        };
        if start < 0 || start > count {
            return Ok(expansion_fields(vec![Vec::new()], quoted));
        }
        let length = if let Some(n) = split {
            let text = scalar(&rest[n + 1..], context)?;
            arithmetic::eval(&text, context)?
        } else {
            count - start
        };
        if length < 0 {
            return Err(Error::new("substring expression < 0"));
        }
        let finish = start.saturating_add(length).min(count);
        if let Some(positions) = positions {
            let values = positions[start as usize..finish as usize].to_vec();
            let values = if name == b"*" {
                vec![join(&values, ifs(context)?.first().copied())]
            } else {
                values
            };
            return Ok(expansion_fields(values, quoted));
        }
        return Ok(expansion_fields(
            vec![value[boundaries[start as usize]..boundaries[finish as usize]].to_vec()],
            quoted,
        ));
    }
    if tail[0] == b'#' || tail[0] == b'%' {
        let prefix = tail[0] == b'#';
        let longest = tail.get(1) == tail.first();
        let pattern = pattern_scalar(&tail[1 + usize::from(longest)..], context)?;
        let unicode = unicode_locale(context)?;
        let boundaries = boundaries(&value, unicode);
        let mut lengths: Vec<usize> = boundaries
            .into_iter()
            .map(|n| if prefix { n } else { value.len() - n })
            .collect();
        lengths.sort_unstable();
        if longest {
            lengths.reverse();
        }
        for n in lengths {
            let part = if prefix {
                &value[..n]
            } else {
                &value[value.len() - n..]
            };
            if pattern_matches_mode(&pattern, part, unicode, context.extglob()) {
                return Ok(expansion_fields(
                    vec![if prefix {
                        value[n..].to_vec()
                    } else {
                        value[..value.len() - n].to_vec()
                    }],
                    quoted,
                ));
            }
        }
        return Ok(expansion_fields(vec![value], quoted));
    }
    if tail[0] == b'/' {
        let global = tail.get(1) == Some(&b'/');
        let rest = &tail[1 + usize::from(global)..];
        let at = unquoted_slash(rest).unwrap_or(rest.len());
        let pattern = pattern_scalar(&rest[..at], context)?;
        let replacement = if at < rest.len() {
            scalar(&rest[at + 1..], context)?
        } else {
            Vec::new()
        };
        return Ok(expansion_fields(
            vec![replace_pattern(
                &value,
                &pattern,
                &replacement,
                global,
                unicode_locale(context)?,
                context.extglob(),
            )],
            quoted,
        ));
    }
    Err(Error::new("unsupported parameter expansion"))
}

fn unquoted_slash(input: &[u8]) -> Option<usize> {
    let (mut quote, mut at) = (0, 0);
    while at < input.len() {
        let c = input[at];
        if c == b'\\' && quote != b'\'' {
            at += 2;
            continue;
        }
        if quote == c {
            quote = 0;
        } else if quote == 0 && (c == b'\'' || c == b'"') {
            quote = c;
        } else if quote == 0 && c == b'/' {
            return Some(at);
        }
        at += 1;
    }
    None
}

fn pattern_scalar(input: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    for field in expand_raw(input, context, false, false)? {
        for (at, &c) in field.bytes.iter().enumerate() {
            if field.quoted[at] || c == b'\\' {
                result.push(b'\\');
            }
            result.push(c);
        }
    }
    Ok(result)
}

pub fn expand_pattern(input: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    pattern_scalar(input, context).map_err(expansion_error)
}

pub fn pattern_matches_context(
    pattern: &[u8],
    value: &[u8],
    context: &mut (impl Context + ?Sized),
) -> Result<bool> {
    Ok(pattern_matches_mode(
        pattern,
        value,
        unicode_locale(context)?,
        context.extglob(),
    ))
}

fn expansion_fields(values: Vec<Vec<u8>>, quoted: bool) -> Vec<Field> {
    values
        .into_iter()
        .map(|value| {
            let mut field = Field::default();
            field.append(&value, quoted, true);
            field
        })
        .collect()
}

fn append_fields(fields: &mut Vec<Field>, values: Vec<Field>) {
    for (i, value) in values.into_iter().enumerate() {
        if i != 0 {
            fields.push(Field::default());
        }
        let current = fields.last_mut().unwrap();
        current.bytes.extend(value.bytes);
        current.quoted.extend(value.quoted);
        current.split.extend(value.split);
        current.force |= value.force;
    }
}

fn append_values(fields: &mut Vec<Field>, values: Vec<Vec<u8>>, quoted: bool) {
    for (i, value) in values.into_iter().enumerate() {
        if i != 0 {
            fields.push(Field::default());
        }
        fields.last_mut().unwrap().append(&value, quoted, true);
    }
}

fn expand_raw(
    input: &[u8],
    context: &mut (impl Context + ?Sized),
    inherited_quote: bool,
    assignment_tildes: bool,
) -> Result<Vec<Field>> {
    let mut fields = vec![Field::default()];
    let mut at = 0;
    let mut double = inherited_quote;
    let mut quote_start = None;
    let mut quote_had_content = false;
    let mut after_colon = false;
    if input.first() == Some(&b'~') && !inherited_quote {
        let end = input
            .iter()
            .position(|&c| c == b'/' || assignment_tildes && c == b':')
            .unwrap_or(input.len());
        let home = match &input[1..end] {
            b"" => context.get(b"HOME")?,
            b"+" => Some(context.cwd()),
            b"-" => context.get(b"OLDPWD")?,
            _ => context.home_for_user(&input[1..end])?,
        };
        if let Some(home) = home {
            fields[0].append(&home, true, false);
            at = end;
        }
    }
    while at < input.len() {
        let c = input[at];
        let tilde_after_colon = after_colon;
        after_colon = false;
        if !double && assignment_tildes && tilde_after_colon && c == b'~' {
            let end = input[at + 1..]
                .iter()
                .position(|&c| c == b'/' || c == b':')
                .map_or(input.len(), |n| at + 1 + n);
            let home = match &input[at + 1..end] {
                b"" => context.get(b"HOME")?,
                b"+" => Some(context.cwd()),
                b"-" => context.get(b"OLDPWD")?,
                user => context.home_for_user(user)?,
            };
            if let Some(home) = home {
                fields.last_mut().unwrap().append(&home, true, false);
                at = end;
                continue;
            }
        }
        if !double
            && !context.extglob()
            && b"?*+@!".contains(&c)
            && input.get(at + 1) == Some(&b'(')
        {
            return Err(Error::new("extended glob syntax requires shopt -s extglob"));
        }
        if c == b'"' {
            if double && !inherited_quote {
                if !quote_had_content {
                    fields.last_mut().unwrap().force = true;
                }
                double = false;
                quote_start = None;
            } else if !double {
                double = true;
                quote_start = Some(at);
                quote_had_content = false;
            } else {
                fields.last_mut().unwrap().append(&[c], true, false);
            }
            at += 1;
            continue;
        }
        if c == b'\'' && !double {
            let end = input[at + 1..]
                .iter()
                .position(|&c| c == b'\'')
                .map(|n| at + 1 + n)
                .ok_or_else(|| Error::new("unterminated single quote"))?;
            fields
                .last_mut()
                .unwrap()
                .append(&input[at + 1..end], true, false);
            at = end + 1;
            continue;
        }
        if c == b'\\' {
            let next = input
                .get(at + 1)
                .copied()
                .ok_or_else(|| Error::new("trailing backslash"))?;
            if next == b'\n' {
                at += 2;
                continue;
            }
            if !double || b"$`\"\\".contains(&next) {
                fields.last_mut().unwrap().append(&[next], true, false);
                quote_had_content = true;
                at += 2;
                continue;
            }
        }
        if c == b'`' {
            let (end, source) = backtick_end(input, at)?;
            let value = command_output(&source, context)?;
            fields.last_mut().unwrap().append(&value, double, true);
            quote_had_content = true;
            at = end;
            continue;
        }
        if c == b'$' {
            if input.get(at + 1) == Some(&b'"') && !double {
                at += 1;
                continue;
            }
            if input.get(at + 1) == Some(&b'[') {
                let mut end = at + 2;
                let mut nesting = 1;
                while end < input.len() && nesting > 0 {
                    match input[end] {
                        b'[' => nesting += 1,
                        b']' => nesting -= 1,
                        _ => {}
                    }
                    end += 1;
                }
                if nesting != 0 {
                    return Err(Error::new("unterminated arithmetic expansion"));
                }
                let expression = scalar(&input[at + 2..end - 1], context)?;
                let value = arithmetic::eval(&expression, context)?
                    .to_string()
                    .into_bytes();
                fields.last_mut().unwrap().append(&value, double, true);
                quote_had_content = true;
                at = end;
                continue;
            }
            if input.get(at + 1) == Some(&b'\'') && !double {
                let mut end = at + 2;
                while end < input.len() {
                    if input[end] == b'\\' {
                        end += 2;
                    } else if input[end] == b'\'' {
                        break;
                    } else {
                        end += 1;
                    }
                }
                if end >= input.len() {
                    return Err(Error::new("unterminated ANSI-C quote"));
                }
                let raw = CString::new(&input[at + 2..end])
                    .map_err(|_| Error::new("NUL in shell word"))?;
                let mut value = crate::ansi_c::decode(&raw, 0, false, |c| c.to_ascii_uppercase())
                    .map_or(Vec::new(), |s| s.bytes);
                if let Some(nul) = value.iter().position(|&c| c == 0) {
                    value.truncate(nul);
                }
                fields.last_mut().unwrap().append(&value, true, false);
                at = end + 1;
                continue;
            }
            if input.get(at + 1) == Some(&b'(') {
                let end = expansion_end(input, at)?;
                let value = if input.get(at + 2) == Some(&b'(') {
                    let expression = scalar(&input[at + 3..end - 2], context)?;
                    arithmetic::eval(&expression, context)?
                        .to_string()
                        .into_bytes()
                } else {
                    command_output(&input[at + 2..end - 1], context)?
                };
                fields.last_mut().unwrap().append(&value, double, true);
                quote_had_content = true;
                at = end;
                continue;
            }
            if input.get(at + 1) == Some(&b'{') {
                let end = expansion_end(input, at)?;
                let values = braced(&input[at + 2..end - 1], context, double)?;
                append_fields(&mut fields, values);
                quote_had_content = true;
                at = end;
                continue;
            }
            let rest = &input[at + 1..];
            let length = if rest
                .first()
                .is_some_and(|c| c.is_ascii_digit() || b"@*#?$!-".contains(c))
            {
                1
            } else {
                rest.iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                    .count()
            };
            if length != 0 {
                let name = &rest[..length];
                if name == b"@" {
                    append_values(&mut fields, context.positional(), double);
                } else {
                    let value = parameter(name, context)?;
                    if value.is_none() && context.nounset() {
                        return Err(parameter_error(
                            format!("{}: unbound variable", String::from_utf8_lossy(name)),
                            context,
                        ));
                    }
                    fields
                        .last_mut()
                        .unwrap()
                        .append(&value.unwrap_or_default(), double, true);
                }
                quote_had_content = true;
                at += 1 + length;
                continue;
            }
        }
        fields.last_mut().unwrap().append(&[c], double, false);
        after_colon = c == b':' && !double;
        quote_had_content = true;
        at += 1;
    }
    if quote_start.is_some() {
        return Err(Error::new("unterminated double quote"));
    }
    Ok(fields)
}

fn split_field(field: Field, ifs: &[u8]) -> Vec<Field> {
    if ifs.is_empty() {
        return if field.force || !field.bytes.is_empty() {
            vec![field]
        } else {
            Vec::new()
        };
    }
    let whitespace = |at: usize| {
        field.split[at] && ifs.contains(&field.bytes[at]) && b" \t\n".contains(&field.bytes[at])
    };
    let delimiter = |at: usize| field.split[at] && ifs.contains(&field.bytes[at]);
    let mut at = 0;
    while at < field.bytes.len() && whitespace(at) {
        at += 1;
    }
    let mut result = Vec::new();
    let mut current = Field {
        force: field.force,
        ..Field::default()
    };
    while at < field.bytes.len() {
        if !delimiter(at) {
            current.push_from(&field, at);
            at += 1;
            continue;
        }
        if whitespace(at) {
            while at < field.bytes.len() && whitespace(at) {
                at += 1;
            }
            if at < field.bytes.len() && delimiter(at) {
                at += 1;
            }
        } else {
            at += 1;
        }
        result.push(current);
        current = Field::default();
        while at < field.bytes.len() && whitespace(at) {
            at += 1;
        }
    }
    if current.force || !current.bytes.is_empty() {
        result.push(current);
    }
    result
}

pub fn expand_word(
    raw: &[u8],
    context: &mut (impl Context + ?Sized),
    mode: Mode,
) -> Result<Vec<Vec<u8>>> {
    expand_word_inner(raw, context, mode).map_err(expansion_error)
}

fn expand_word_inner(
    raw: &[u8],
    context: &mut (impl Context + ?Sized),
    mode: Mode,
) -> Result<Vec<Vec<u8>>> {
    let raw_words = if mode == Mode::Word {
        brace_expand(raw)?
    } else {
        vec![raw.to_vec()]
    };
    let mut result = Vec::new();
    for raw in raw_words {
        let fields = expand_raw(&raw, context, false, mode == Mode::Assignment)?;
        if mode == Mode::Assignment {
            result.push(join(
                &fields.into_iter().map(|f| f.bytes).collect::<Vec<_>>(),
                ifs(context)?.first().copied(),
            ));
            continue;
        }
        let separators = ifs(context)?;
        for field in fields.into_iter().flat_map(|f| split_field(f, &separators)) {
            result.extend(glob(field, context)?);
        }
    }
    Ok(result)
}

pub fn expand_scalar(raw: &[u8], context: &mut (impl Context + ?Sized)) -> Result<Vec<u8>> {
    scalar(raw, context).map_err(expansion_error)
}

pub fn pattern_matches(pattern: &[u8], value: &[u8]) -> bool {
    pattern_matches_mode(pattern, value, true, true)
}

fn pattern_matches_mode(pattern: &[u8], value: &[u8], unicode: bool, extended: bool) -> bool {
    match_pattern(pattern, value, unicode, extended, 0)
}

fn match_pattern(
    pattern: &[u8],
    value: &[u8],
    unicode: bool,
    extended: bool,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let mut pending = vec![(0, 0)];
    let mut visited = std::collections::HashSet::new();
    while let Some((p, v)) = pending.pop() {
        if !visited.insert((p, v)) {
            continue;
        }
        let Some(&token) = pattern.get(p) else {
            if v == value.len() {
                return true;
            }
            continue;
        };
        if extended
            && b"?*+@!".contains(&token)
            && pattern.get(p + 1) == Some(&b'(')
            && let Some((end, alternatives)) = pattern_group(pattern, p)
        {
            let starts = if token == b'*' || token == b'+' {
                let mut reached = std::collections::BTreeSet::new();
                let mut todo = vec![v];
                if token == b'*' {
                    reached.insert(v);
                }
                let mut explored = std::collections::HashSet::new();
                while let Some(start) = todo.pop() {
                    if !explored.insert(start) {
                        continue;
                    }
                    for count in boundaries(&value[start..], unicode) {
                        let finish = start + count;
                        if alternatives.iter().any(|alt| {
                            match_pattern(alt, &value[start..finish], unicode, extended, depth + 1)
                        }) {
                            reached.insert(finish);
                            if finish > start {
                                todo.push(finish);
                            }
                        }
                    }
                }
                reached.into_iter().collect::<Vec<_>>()
            } else {
                boundaries(&value[v..], unicode)
                    .into_iter()
                    .filter_map(|count| {
                        let finish = v + count;
                        let matched = alternatives.iter().any(|alt| {
                            match_pattern(alt, &value[v..finish], unicode, extended, depth + 1)
                        });
                        ((token == b'?' && count == 0)
                            || if token == b'!' { !matched } else { matched })
                        .then_some(finish)
                    })
                    .collect()
            };
            pending.extend(starts.into_iter().map(|finish| (end, finish)));
            continue;
        }
        match token {
            b'*' => {
                pending.push((p + 1, v));
                if v < value.len() {
                    pending.push((p, v + character(&value[v..], unicode).1));
                }
            }
            b'?' if v < value.len() => pending.push((p + 1, v + character(&value[v..], unicode).1)),
            b'\\' if p + 1 < pattern.len() => {
                if value.get(v) == pattern.get(p + 1) {
                    pending.push((p + 2, v + 1));
                }
            }
            b'[' => {
                let current = value.get(v).map(|_| character(&value[v..], unicode).0);
                if let Some((end, matched)) = bracket(&pattern[p..], current, unicode) {
                    if matched && v < value.len() {
                        pending.push((p + end, v + character(&value[v..], unicode).1));
                    }
                } else if value.get(v) == Some(&b'[') {
                    pending.push((p + 1, v + 1));
                }
            }
            c => {
                if value.get(v) == Some(&c) {
                    pending.push((p + 1, v + 1));
                }
            }
        }
    }
    false
}

fn pattern_group(pattern: &[u8], start: usize) -> Option<(usize, Vec<&[u8]>)> {
    let mut at = start + 2;
    let mut begin = at;
    let mut depth = 1;
    let mut alternatives = Vec::new();
    while at < pattern.len() {
        match pattern[at] {
            b'\\' => {
                at += 2;
                continue;
            }
            b'[' => {
                if let Some((end, _)) = bracket(&pattern[at..], None, false) {
                    at += end;
                    continue;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    alternatives.push(&pattern[begin..at]);
                    return Some((at + 1, alternatives));
                }
            }
            b'|' if depth == 1 => {
                alternatives.push(&pattern[begin..at]);
                begin = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

fn character(input: &[u8], unicode: bool) -> (u32, usize) {
    if !unicode {
        return (u32::from(input[0]), 1);
    }
    let size = match input[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    if let Some(bytes) = input.get(..size)
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return (text.chars().next().unwrap() as u32, size);
    }
    (u32::from(input[0]), 1)
}

fn bracket(pattern: &[u8], value: Option<u32>, unicode: bool) -> Option<(usize, bool)> {
    let mut at = 1;
    let invert = pattern.get(at).is_some_and(|c| *c == b'!' || *c == b'^');
    at += usize::from(invert);
    let first = at;
    let mut matched = false;
    while at < pattern.len() {
        if pattern[at] == b']' && at != first {
            return Some((at + 1, matched != invert));
        }
        if pattern[at..].starts_with(b"[:")
            && let Some(end) = pattern[at + 2..].windows(2).position(|s| s == b":]")
        {
            let class = &pattern[at + 2..at + 2 + end];
            matched |= value.and_then(char::from_u32).is_some_and(|c| match class {
                b"alnum" => c.is_alphanumeric(),
                b"alpha" => c.is_alphabetic(),
                b"ascii" => c.is_ascii(),
                b"blank" => c == ' ' || c == '\t',
                b"cntrl" => c.is_control(),
                b"digit" => c.is_ascii_digit(),
                b"graph" => !c.is_control() && !c.is_whitespace(),
                b"lower" => c.is_lowercase(),
                b"print" => !c.is_control(),
                b"punct" => c.is_ascii_punctuation(),
                b"space" => c.is_whitespace(),
                b"upper" => c.is_uppercase(),
                b"xdigit" => c.is_ascii_hexdigit(),
                _ => false,
            });
            at += end + 4;
            continue;
        }
        if pattern[at] == b'\\' && at + 1 < pattern.len() {
            at += 1;
        }
        let (c, length) = character(&pattern[at..], unicode);
        at += length;
        if pattern.get(at) == Some(&b'-') && pattern.get(at + 1).is_some_and(|c| *c != b']') {
            let (last, length) = character(&pattern[at + 1..], unicode);
            matched |= value.is_some_and(|v| c <= v && v <= last);
            at += 1 + length;
        } else {
            matched |= value == Some(c);
        }
    }
    None
}

fn boundaries(value: &[u8], unicode: bool) -> Vec<usize> {
    let mut result = vec![0];
    let mut at = 0;
    while at < value.len() {
        at += character(&value[at..], unicode).1;
        result.push(at);
    }
    result
}

fn replace_pattern(
    value: &[u8],
    pattern: &[u8],
    replacement: &[u8],
    global: bool,
    unicode: bool,
    extended: bool,
) -> Vec<u8> {
    let (anchor_start, anchor_end, pattern) = match pattern.first() {
        Some(b'#') => (true, false, &pattern[1..]),
        Some(b'%') => (false, true, &pattern[1..]),
        _ => (false, false, pattern),
    };
    if pattern.is_empty() {
        return value.to_vec();
    }
    let mut result = Vec::new();
    let mut at = 0;
    let positions = boundaries(value, unicode);
    while at < value.len() {
        let end = if anchor_start && at != 0 {
            None
        } else {
            positions
                .iter()
                .copied()
                .rev()
                .filter(|&end| end > at)
                .find(|&end| {
                    (!anchor_end || end == value.len())
                        && pattern_matches_mode(pattern, &value[at..end], unicode, extended)
                })
        };
        if let Some(end) = end {
            result.extend_from_slice(replacement);
            at = end;
            if !global {
                break;
            }
        } else {
            let length = character(&value[at..], unicode).1;
            result.extend_from_slice(&value[at..at + length]);
            at += length;
        }
    }
    result.extend_from_slice(&value[at..]);
    result
}

fn unicode_locale(context: &mut (impl Context + ?Sized)) -> Result<bool> {
    for name in [b"LC_ALL".as_slice(), b"LC_CTYPE", b"LANG"] {
        if let Some(locale) = context.get(name)?
            && !locale.is_empty()
        {
            let locale = locale.to_ascii_lowercase();
            return Ok(
                locale.windows(5).any(|s| s == b"utf-8") || locale.windows(4).any(|s| s == b"utf8")
            );
        }
    }
    Ok(false)
}

fn glob(field: Field, context: &mut (impl Context + ?Sized)) -> Result<Vec<Vec<u8>>> {
    if context.noglob()
        || !field.bytes.iter().enumerate().any(|(n, c)| {
            !field.quoted[n]
                && (b"*?[".contains(c)
                    || context.extglob()
                        && b"+@!".contains(c)
                        && field.bytes.get(n + 1) == Some(&b'('))
        })
    {
        return Ok(vec![field.bytes]);
    }
    let mut pattern = Vec::new();
    for (n, &c) in field.bytes.iter().enumerate() {
        if c == b'\\' || field.quoted[n] && b"*?[]()|@+!".contains(&c) {
            pattern.push(b'\\');
        }
        pattern.push(c);
    }
    let unicode = unicode_locale(context)?;
    let root_len = if cfg!(windows)
        && pattern.len() >= 3
        && pattern[0].is_ascii_alphabetic()
        && pattern[1..3] == *b":/"
    {
        3
    } else if cfg!(windows) && pattern.starts_with(b"//") {
        // A UNC path starts at its server/share pair, not at the current drive.
        pattern[2..]
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == b'/')
            .nth(1)
            .map_or(pattern.len(), |(at, _)| at + 3)
    } else {
        usize::from(pattern.starts_with(b"/"))
    };
    let base = if root_len > 0 {
        PathBuf::from(os::from_bytes(&pattern[..root_len])?)
    } else {
        PathBuf::from(os::from_bytes(&context.cwd())?)
    };
    let mut paths = vec![(base, pattern[..root_len].to_vec())];
    let components: Vec<_> = pattern[root_len..].split(|&c| c == b'/').collect();
    for (n, component) in components.iter().enumerate() {
        if component.is_empty() {
            continue;
        }
        let mut next = Vec::new();
        let wildcard = component.iter().any(|c| b"*?[".contains(c))
            || context.extglob() && extended_pattern(component);
        for (path, display) in paths {
            let names: Vec<Vec<u8>> = if wildcard {
                let Ok(entries) = std::fs::read_dir(&path) else {
                    continue;
                };
                entries
                    .filter_map(|entry| entry.ok().map(|entry| os::to_bytes(&entry.file_name())))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .chain([b".".to_vec(), b"..".to_vec()])
                    .filter(|name| {
                        (!name.starts_with(b".")
                            || component.starts_with(b".")
                            || component.starts_with(b"\\."))
                            && pattern_matches_mode(component, name, unicode, context.extglob())
                    })
                    .collect()
            } else {
                vec![unescape_pattern(component)]
            };
            for name in names {
                let actual = path.join(os::from_bytes(&name)?);
                if n + 1 < components.len() && !actual.is_dir() {
                    continue;
                }
                if n + 1 == components.len()
                    && !actual.exists()
                    && actual.symlink_metadata().is_err()
                {
                    continue;
                }
                let mut shown = display.clone();
                if !shown.is_empty() && !shown.ends_with(b"/") {
                    shown.push(b'/');
                }
                shown.extend_from_slice(&name);
                next.push((actual, shown));
            }
        }
        paths = next;
    }
    let mut matches: Vec<_> = paths
        .into_iter()
        .map(|(_, mut display)| {
            if pattern.ends_with(b"/") && !display.ends_with(b"/") {
                display.push(b'/');
            }
            display
        })
        .collect();
    matches.sort();
    if matches.is_empty() {
        if context.failglob() {
            return Err(Error::new(format!(
                "no match: {}",
                String::from_utf8_lossy(&field.bytes)
            )));
        }
        if !context.nullglob() {
            matches.push(field.bytes);
        }
    }
    Ok(matches)
}

fn extended_pattern(pattern: &[u8]) -> bool {
    let mut at = 0;
    while at < pattern.len() {
        if pattern[at] == b'\\' {
            at += 2;
            continue;
        }
        if b"?*+@!".contains(&pattern[at]) && pattern.get(at + 1) == Some(&b'(') {
            return true;
        }
        at += 1;
    }
    false
}

fn unescape_pattern(input: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < input.len() {
        if input[at] == b'\\' && at + 1 < input.len() {
            at += 1;
        }
        result.push(input[at]);
        at += 1;
    }
    result
}

fn brace_expand(input: &[u8]) -> Result<Vec<Vec<u8>>> {
    fn recurse(input: &[u8], output: &mut Vec<Vec<u8>>, depth: usize) -> Result<()> {
        if depth > 64 || output.len() >= 10000 {
            return Err(Error::new("brace expansion limit exceeded"));
        }
        let (mut at, mut quote) = (0, 0);
        while at < input.len() {
            if input[at] == b'\\' {
                at += 2;
                continue;
            }
            if quote == input[at] {
                quote = 0;
                at += 1;
                continue;
            }
            if quote == 0 && (input[at] == b'\'' || input[at] == b'"') {
                quote = input[at];
                at += 1;
                continue;
            }
            if quote != 0 {
                at += 1;
                continue;
            }
            if input[at] == b'$' && input.get(at + 1).is_some_and(|c| *c == b'{' || *c == b'(') {
                at = expansion_end(input, at)?;
                continue;
            }
            if input[at] != b'{' {
                at += 1;
                continue;
            }
            let start = at;
            let (mut end, mut nesting, mut commas, mut inner_quote) = (at + 1, 1, Vec::new(), 0);
            while end < input.len() && nesting != 0 {
                let c = input[end];
                if c == b'\\' {
                    end += 2;
                    continue;
                }
                if inner_quote == c {
                    inner_quote = 0;
                } else if inner_quote == 0 && (c == b'\'' || c == b'"') {
                    inner_quote = c;
                } else if inner_quote == 0 {
                    match c {
                        b'{' => nesting += 1,
                        b'}' => nesting -= 1,
                        b',' if nesting == 1 => commas.push(end),
                        _ => {}
                    }
                }
                end += 1;
            }
            if nesting != 0 {
                break;
            }
            let close = end - 1;
            let alternatives = if !commas.is_empty() {
                let mut begin = start + 1;
                commas.push(close);
                commas
                    .into_iter()
                    .map(|n| {
                        let part = input[begin..n].to_vec();
                        begin = n + 1;
                        part
                    })
                    .collect::<Vec<_>>()
            } else {
                brace_range(&input[start + 1..close])
            };
            if alternatives.is_empty() {
                at = end;
                continue;
            }
            for part in alternatives {
                let mut word = input[..start].to_vec();
                word.extend(part);
                word.extend_from_slice(&input[end..]);
                recurse(&word, output, depth + 1)?;
            }
            return Ok(());
        }
        output.push(input.to_vec());
        Ok(())
    }
    let mut output = Vec::new();
    recurse(input, &mut output, 0)?;
    Ok(output)
}

fn brace_range(input: &[u8]) -> Vec<Vec<u8>> {
    let Some(n) = input.windows(2).position(|s| s == b"..") else {
        return Vec::new();
    };
    let (a, b) = (&input[..n], &input[n + 2..]);
    let numbers = std::str::from_utf8(a)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .zip(
            std::str::from_utf8(b)
                .ok()
                .and_then(|s| s.parse::<i64>().ok()),
        );
    if let Some((a, b)) = numbers {
        if a.abs_diff(b) >= 10000 {
            return Vec::new();
        }
        return (0..=a.abs_diff(b))
            .map(|n| {
                (if a <= b { a + n as i64 } else { a - n as i64 })
                    .to_string()
                    .into_bytes()
            })
            .collect();
    }
    if a.len() == 1 && b.len() == 1 && a[0].is_ascii_alphabetic() && b[0].is_ascii_alphabetic() {
        return (0..=a[0].abs_diff(b[0]))
            .map(|n| vec![if a[0] <= b[0] { a[0] + n } else { a[0] - n }])
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Context {
        vars: BTreeMap<Vec<u8>, Vec<u8>>,
        args: Vec<Vec<u8>>,
        substitutions: Vec<Vec<u8>>,
    }
    impl arithmetic::Variables for Context {
        fn get(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>> {
            Ok(self.vars.get(name).cloned())
        }
        fn set(&mut self, name: &[u8], value: Vec<u8>) -> Result<()> {
            self.vars.insert(name.to_vec(), value);
            Ok(())
        }
    }
    impl ExpansionContext for Context {
        fn positional(&self) -> Vec<Vec<u8>> {
            self.args.clone()
        }
        fn arg0(&self) -> Vec<u8> {
            b"rash".to_vec()
        }
        fn status(&self) -> i32 {
            7
        }
        fn pid(&self) -> u32 {
            42
        }
        fn last_pid(&self) -> Option<u32> {
            None
        }
        fn cwd(&self) -> Vec<u8> {
            b"/".to_vec()
        }
        fn command_substitute(&mut self, source: &[u8]) -> Result<Vec<u8>> {
            self.substitutions.push(source.to_vec());
            Ok(b"a b\n\n".to_vec())
        }
    }
    fn words(word: &[u8], ctx: &mut Context) -> Vec<Vec<u8>> {
        expand_word(word, ctx, Mode::Word).unwrap()
    }
    fn expected(values: &[&[u8]]) -> Vec<Vec<u8>> {
        values.iter().map(|v| v.to_vec()).collect()
    }

    #[test]
    fn quoted_parameters_and_positional_boundaries() {
        let mut ctx = Context::default();
        ctx.vars.insert(b"x".to_vec(), b"a b".to_vec());
        assert_eq!(
            words(b"pre${x}post", &mut ctx),
            expected(&[b"prea", b"bpost"])
        );
        assert_eq!(
            words(b"\"pre${x}post\"", &mut ctx),
            expected(&[b"prea bpost"])
        );
        assert_eq!(words(b"${unset:-\"a b\"}", &mut ctx), expected(&[b"a b"]));
        assert_eq!(words(b"${unset:-a b}", &mut ctx), expected(&[b"a", b"b"]));
        assert!(words(b"\"$@\"", &mut ctx).is_empty());
        ctx.args = expected(&[b"a b", b"", b"c"]);
        assert_eq!(
            words(b"\"pre$@post\"", &mut ctx),
            expected(&[b"prea b", b"", b"cpost"])
        );
        assert_eq!(words(b"$x[0]", &mut ctx), expected(&[b"a", b"b[0]"]));
    }

    #[test]
    fn defaults_are_lazy_and_preserve_state() {
        let mut ctx = Context::default();
        assert_eq!(words(b"${x:=value}", &mut ctx), expected(&[b"value"]));
        assert_eq!(words(b"${x:-$(unused)}", &mut ctx), expected(&[b"value"]));
        assert!(ctx.substitutions.is_empty());
        assert_eq!(words(b"\"$(printf x)\"", &mut ctx), expected(&[b"a b"]));
        assert_eq!(ctx.substitutions, expected(&[b"printf x"]));
        assert_eq!(words(b"$((n=2,n++))", &mut ctx), expected(&[b"2"]));
        assert_eq!(ctx.vars.get(b"n".as_slice()).unwrap(), b"3");
        ctx.vars.insert(b"HOME".to_vec(), b"/a b".to_vec());
        assert_eq!(
            expand_word(b"~:~/bin:'~'", &mut ctx, Mode::Assignment).unwrap(),
            expected(&[b"/a b:/a b/bin:~"])
        );
    }

    #[test]
    fn ifs_empties_patterns_braces_and_raw_bytes() {
        let mut ctx = Context::default();
        ctx.vars.insert(b"IFS".to_vec(), b":".to_vec());
        ctx.vars.insert(b"x".to_vec(), b":a::b:".to_vec());
        assert_eq!(words(b"$x", &mut ctx), expected(&[b"", b"a", b"", b"b"]));
        assert_eq!(
            words(b"x{a,b}{2..1}", &mut ctx),
            expected(&[b"xa2", b"xa1", b"xb2", b"xb1"])
        );
        assert_eq!(words(b"$'a\\xff'", &mut ctx), expected(&[b"a\xff"]));
        assert!(pattern_matches(
            "?[[:digit:]][é]".as_bytes(),
            "🐟2é".as_bytes()
        ));
        assert!(pattern_matches(b"a[!0-9]*", b"abc"));
        assert!(!pattern_matches(b"a[!0-9]*", b"a2c"));
        assert!(pattern_matches(b"+(a|b|c)", b"abc"));
        assert!(pattern_matches(b"@(item|other)", b"item"));
        assert!(!pattern_matches(b"!(item|other)", b"item"));
        assert!(pattern_matches(b"*(@(a|b)c)", b"acbc"));
        assert!(!pattern_matches(b"\\@\\(item\\|other\\)", b"item"));
    }
}
