//! Locate Fish command substitutions, jobs, processes and tokens without evaluating input.
use crate::tokenizer::{
    Tok, TokFlags, TokenType, Tokenizer, comment_end, is_token_delimiter, quote_end,
};
use fish_widestring::prelude::*;
use std::ops::{self, Range};

#[derive(Debug, Default, PartialEq)]
pub struct CommandSubstitution {
    range: Range<usize>,
    num_closing: usize,
}

impl CommandSubstitution {
    pub fn opening_paren_offset(&self) -> usize {
        self.range.start
    }
    pub fn end(&self) -> usize {
        self.range.end
    }
    #[cfg_attr(windows, allow(dead_code))]
    pub fn opening_paren_range(&self) -> Range<usize> {
        self.range.start..self.range.start + 1
    }
    #[cfg_attr(windows, allow(dead_code))]
    pub fn closing_paren_range(&self) -> Range<usize> {
        self.range.end - self.num_closing..self.range.end
    }
    pub fn command_range(&self) -> Range<usize> {
        self.range.start + 1..self.range.end - self.num_closing
    }
}

/// Alternative API. Iterate over command substitutions.
///
/// \param str the string to search for subshells
/// \param inout_cursor_offset On input, the location to begin the search. On output, either the end
/// of the string, or just after the closed-paren.
/// \param out_contents On output, the contents of the command substitution
/// \param out_start On output, the offset of the start of the command substitution (open paren)
/// \param out_end On output, the offset of the end of the command substitution (close paren), or
/// the end of the string if it was incomplete
/// \param accept_incomplete whether to permit missing closing parenthesis
/// \param inout_is_quoted whether the cursor is in a double-quoted context.
/// \param out_has_dollar whether the command substitution has the optional leading $.
/// Return -1 on syntax error, 0 if no subshells exist and 1 on success
#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_unit_err)]
pub fn locate_cmdsubst_range(
    s: &wstr,
    inout_cursor_offset: &mut usize,
    accept_incomplete: bool,
    inout_is_quoted: Option<&mut bool>,
    out_has_dollar: Option<&mut bool>,
) -> Result<Option<CommandSubstitution>, ()> {
    // Nothing to do if the offset is at or past the end of the string.
    if *inout_cursor_offset >= s.len() {
        return Ok(None);
    }

    // Defer to the wonky version.
    let ret = locate_cmdsub(
        s,
        *inout_cursor_offset,
        accept_incomplete,
        inout_is_quoted,
        out_has_dollar,
    );
    match &ret {
        Err(()) | Ok(None) => (),
        Ok(Some(cmdsub)) => {
            *inout_cursor_offset = cmdsub.end();
        }
    }
    ret
}

/// Find the beginning and end of the command substitution under the cursor. If no subshell is
/// found, the entire string is returned. If the current command substitution is not ended, i.e. the
/// closing parenthesis is missing, then the string from the beginning of the substitution to the
/// end of the string is returned.
///
/// \param buff the string to search for subshells
/// \param cursor_pos the position of the cursor
/// \param a the start of the searched string
/// \param b the end of the searched string
pub fn get_cmdsubst_extent(buff: &wstr, cursor: usize) -> ops::Range<usize> {
    // The tightest command substitution found so far.
    let mut result = 0..buff.len();
    let mut pos = 0;
    loop {
        let cmdsub = match locate_cmdsub(buff, pos, true, None, None) {
            // No subshell found, all done.
            Err(()) | Ok(None) => break,
            Ok(Some(cmdsub)) => cmdsub,
        };

        let command = cmdsub.command_range();
        if command.start <= cursor && command.end >= cursor {
            // This command substitution surrounds the cursor, so it's a tighter fit.
            result = command;
            // pos is where to begin looking for the next one. But if we reached the end there's no
            // next one.
            if result.start >= result.end {
                break;
            }
            pos = result.start + 1;
        } else if cursor < command.start {
            // This command substitution starts at or after the cursor. Since it was the first
            // command substitution in the string, we're done.
            break;
        } else {
            // This command substitution ends before the cursor. Skip it.
            assert!(command.end < cursor);
            pos = cmdsub.end();
            assert!(pos <= buff.len());
        }
    }
    result
}

fn locate_cmdsub(
    input: &wstr,
    cursor: usize,
    allow_incomplete: bool,
    mut inout_is_quoted: Option<&mut bool>,
    mut out_has_dollar: Option<&mut bool>,
) -> Result<Option<CommandSubstitution>, ()> {
    let input = input.as_char_slice();

    let mut escaped = false;
    let mut is_token_begin = true;
    let mut syntax_error = false;
    let mut paren_count = 0;
    let mut quoted_cmdsubs = vec![];

    let mut pos = cursor;
    let mut last_dollar = None;
    let mut paren_begin = None;
    let mut paren_end = None;
    enum Quote {
        Real(char),
        VirtualDouble,
    }
    fn process_opening_quote(
        input: &[char],
        inout_is_quoted: &mut Option<&mut bool>,
        paren_count: i32,
        quoted_cmdsubs: &mut Vec<i32>,
        mut pos: usize,
        last_dollar: &mut Option<usize>,
        quote: Quote,
    ) -> Option<usize> {
        let quote = match quote {
            Quote::Real(q) => q,
            Quote::VirtualDouble => {
                pos = pos.saturating_sub(1);
                '"'
            }
        };
        let q_end = quote_end(input.into(), pos, quote)?;
        // Found a valid closing quote.
        if input[q_end] == '$' {
            // The closing quote is another quoted command substitution.
            *last_dollar = Some(q_end);
            quoted_cmdsubs.push(paren_count);
        }
        // We want to report whether the outermost command substitution between
        // paren_begin..paren_end is quoted.
        if paren_count == 0 {
            inout_is_quoted
                .as_mut()
                .map(|is_quoted| **is_quoted = input[q_end] == '$');
        }
        Some(q_end)
    }

    if inout_is_quoted
        .as_ref()
        .is_some_and(|is_quoted| **is_quoted)
        && !input.is_empty()
    {
        pos = process_opening_quote(
            input,
            &mut inout_is_quoted,
            paren_count,
            &mut quoted_cmdsubs,
            pos,
            &mut last_dollar,
            Quote::VirtualDouble,
        )
        .map_or(input.len(), |pos| pos + 1);
    }

    while pos < input.len() {
        if escaped {
            escaped = false;
            is_token_begin = false;
        } else {
            let c = input[pos];
            if ['\'', '"'].contains(&c) {
                match process_opening_quote(
                    input,
                    &mut inout_is_quoted,
                    paren_count,
                    &mut quoted_cmdsubs,
                    pos,
                    &mut last_dollar,
                    Quote::Real(c),
                ) {
                    Some(q_end) => pos = q_end,
                    None => break,
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == '#' && is_token_begin {
                pos = comment_end(input.into(), pos) - 1;
            } else if c == '$' {
                last_dollar = Some(pos);
            } else if c == '(' {
                if paren_count == 0 && paren_begin.is_none() {
                    paren_begin = Some(pos);
                    out_has_dollar
                        .as_mut()
                        .map(|has_dollar| **has_dollar = last_dollar == Some(pos.wrapping_sub(1)));
                }

                paren_count += 1;
            } else if c == ')' {
                paren_count -= 1;

                if paren_count == 0 {
                    assert!(paren_end.is_none());
                    paren_end = Some(pos);
                    break;
                }

                if paren_count < 0 {
                    syntax_error = true;
                    break;
                }

                // Check if the ) did complete a quoted command substitution.
                if quoted_cmdsubs.last() == Some(&paren_count) {
                    quoted_cmdsubs.pop();
                    // Quoted command substitutions temporarily close double quotes.
                    // In "foo$(bar)baz$(qux)", after the ), we need to act as if there was a double quote.
                    match process_opening_quote(
                        input,
                        &mut inout_is_quoted,
                        paren_count,
                        &mut quoted_cmdsubs,
                        pos,
                        &mut last_dollar,
                        Quote::VirtualDouble,
                    ) {
                        Some(q_end) => pos = q_end,
                        None => break,
                    }
                }
            }
            is_token_begin = is_token_delimiter(c, input.get(pos + 1).copied());
        }
        pos += 1;
    }

    syntax_error |= paren_count < 0;
    syntax_error |= paren_count > 0 && !allow_incomplete;

    if syntax_error {
        return Err(());
    }

    let Some(paren_begin) = paren_begin else {
        return Ok(None);
    };

    let end = if paren_count != 0 {
        input.len()
    } else {
        paren_end.unwrap() + 1
    };

    let cmdsub = CommandSubstitution {
        range: paren_begin..end,
        num_closing: if paren_count == 0 { 1 } else { 0 },
    };

    Ok(Some(cmdsub))
}

/// Find the beginning and end of the process definition under the cursor
///
/// \param buff the string to search for subshells
/// \param cursor_pos the position of the cursor
/// \param tokens the tokens in the process
pub fn get_process_extent(
    buff: &wstr,
    cursor_pos: usize,
    out_tokens: Option<&mut Vec<Tok>>,
) -> ops::Range<usize> {
    get_job_or_process_extent(true, buff, cursor_pos, out_tokens)
}

#[cfg_attr(windows, allow(dead_code))]
pub fn get_process_first_token_offset(buff: &wstr, cursor_pos: usize) -> Option<usize> {
    let mut tokens = vec![];
    get_process_extent(buff, cursor_pos, Some(&mut tokens));
    tokens.first().map(|tok| tok.offset())
}

/// Find the beginning and end of the process definition under the cursor
///
/// \param buff the string to search for subshells
/// \param cursor_pos the position of the cursor
/// \param out_tokens the tokens in the process
pub fn get_job_extent(
    buff: &wstr,
    cursor_pos: usize,
    out_tokens: Option<&mut Vec<Tok>>,
) -> ops::Range<usize> {
    get_job_or_process_extent(false, buff, cursor_pos, out_tokens)
}

/// Get the beginning and end of the job or process definition under the cursor.
fn get_job_or_process_extent(
    process: bool,
    buff: &wstr,
    cursor_pos: usize,
    mut out_tokens: Option<&mut Vec<Tok>>,
) -> ops::Range<usize> {
    let mut finished = false;

    let cmdsub_range = get_cmdsubst_extent(buff, cursor_pos);
    assert!(cursor_pos >= cmdsub_range.start);
    let pos = cursor_pos - cmdsub_range.start;

    let mut result = cmdsub_range.clone();
    for token in Tokenizer::new(
        &buff[cmdsub_range.clone()],
        TokFlags {
            accept_unfinished: true,
            show_comments: true,
            ..Default::default()
        },
    ) {
        let tok_begin = token.offset();
        if finished {
            break;
        }
        match token.type_ {
            TokenType::Pipe
            | TokenType::End
            | TokenType::Background
            | TokenType::AndAnd
            | TokenType::OrOr
            | TokenType::LeftBrace
            | TokenType::RightBrace
                if (token.type_ != TokenType::Pipe || process) =>
            {
                if tok_begin >= pos {
                    finished = true;
                    result.end = cmdsub_range.start + tok_begin;
                } else if token.end() <= pos {
                    // Statement at cursor might start after this token.
                    result.start = cmdsub_range.start + tok_begin + token.length();
                    out_tokens.as_mut().map(|tokens| tokens.clear());
                } else {
                    finished = true;
                    result = cursor_pos..cursor_pos;
                    out_tokens.as_mut().map(|tokens| tokens.clear());
                }
                continue; // Do not add this to tokens
            }
            _ => (),
        }
        out_tokens.as_mut().map(|tokens| tokens.push(token));
    }
    result
}

/// Find the beginning and end of the token under the cursor and the token before the current token.
///
/// \param buff the string to search for subshells
/// \param cursor_pos the position of the cursor
pub fn get_token_extent(buff: &wstr, cursor_pos: usize) -> (Range<usize>, Range<usize>) {
    let cmdsubst_range = get_cmdsubst_extent(buff, cursor_pos);
    let cmdsubst_begin = cmdsubst_range.start;

    // pos is equivalent to cursor_pos within the range of the command substitution {begin, end}.
    let offset_within_cmdsubst = cursor_pos - cmdsubst_range.start;

    let mut cur_begin = cmdsubst_begin + offset_within_cmdsubst;
    let mut cur_end = cur_begin;
    let mut prev_begin = cur_begin;
    let mut prev_end = cur_begin;

    assert!(cmdsubst_begin <= buff.len());
    assert!(cmdsubst_range.end <= buff.len());

    for token in Tokenizer::new(
        &buff[cmdsubst_range],
        TokFlags {
            accept_unfinished: true,
            ..Default::default()
        },
    ) {
        let tok_begin = token.offset();
        let mut tok_end = tok_begin;

        // Calculate end of token.
        if token.type_ == TokenType::String {
            tok_end += token.length();
        }

        // Cursor was before beginning of this token, means that the cursor is between two tokens,
        // so we set it to a zero element string and break.
        if tok_begin > offset_within_cmdsubst {
            cur_begin = cmdsubst_begin + offset_within_cmdsubst;
            cur_end = cur_begin;
            break;
        }

        // If cursor is inside the token, this is the token we are looking for. If so, set
        // cur_begin and cur_end and break.
        if token.type_ == TokenType::String && tok_end >= offset_within_cmdsubst {
            cur_begin = cmdsubst_begin + token.offset();
            cur_end = cur_begin + token.length();
            break;
        }

        // Remember previous string token.
        if token.type_ == TokenType::String {
            prev_begin = cmdsubst_begin + token.offset();
            prev_end = prev_begin + token.length();
        }
    }

    assert!(prev_begin <= buff.len());
    assert!(prev_end >= prev_begin);
    assert!(prev_end <= buff.len());
    (cur_begin..cur_end, prev_begin..prev_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutions_report_quoted_dollars_and_leave_slices_outside_the_range() {
        let source = L!(r#"prefix "left$(echo one)right$(echo two)" tail (echo three)[1]"#);
        let mut cursor = 0;
        let mut quoted = false;
        let mut dollar = false;
        for (expected, expected_quoted, expected_dollar) in [
            (L!("echo one"), true, true),
            (L!("echo two"), true, true),
            (L!("echo three"), false, false),
        ] {
            let substitution = locate_cmdsubst_range(
                source,
                &mut cursor,
                true,
                Some(&mut quoted),
                Some(&mut dollar),
            )
            .unwrap()
            .unwrap();
            assert_eq!(&source[substitution.command_range()], expected);
            assert_eq!(quoted, expected_quoted);
            assert_eq!(dollar, expected_dollar);
            assert_eq!(substitution.closing_paren_range().len(), 1);
            assert_eq!(source.char_at(substitution.opening_paren_offset()), '(');
            assert_eq!(&source[substitution.opening_paren_range()], L!("("));
        }
        assert_eq!(&source[cursor..], L!("[1]"));
        assert!(
            locate_cmdsubst_range(
                source,
                &mut cursor,
                true,
                Some(&mut quoted),
                Some(&mut dollar)
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn nested_substitutions_comments_and_incomplete_input_keep_cursor_extents() {
        let source = L!(r#"echo "$(first (second 文))" | tail"#);
        let cursor = source.find(L!("文")).unwrap();
        assert_eq!(
            &source[get_cmdsubst_extent(source, cursor)],
            L!("second 文")
        );
        assert_eq!(
            &source[get_process_extent(source, cursor, None)],
            L!("second 文")
        );
        let (current, previous) = get_token_extent(source, cursor);
        assert_eq!(&source[current], L!("文"));
        assert_eq!(&source[previous], L!("second"));
        let commented = L!("echo # (ignored)\nnext \\(literal\\)");
        assert_eq!(
            get_cmdsubst_extent(commented, commented.len()),
            0..commented.len()
        );
        let incomplete = L!("echo $(unfinished 文");
        assert_eq!(
            &incomplete[get_cmdsubst_extent(incomplete, incomplete.len())],
            L!("unfinished 文")
        );
        assert!(locate_cmdsubst_range(incomplete, &mut 0, false, None, None).is_err());
    }

    #[test]
    fn process_job_and_token_ranges_use_character_offsets_through_unicode() {
        let source = L!("one α | two β; three");
        let cursor = source.find(L!("β")).unwrap();
        let mut tokens = Vec::new();
        assert_eq!(
            &source[get_process_extent(source, cursor, Some(&mut tokens))],
            L!(" two β")
        );
        assert_eq!(
            &source[get_job_extent(source, cursor, None)],
            L!("one α | two β")
        );
        assert_eq!(tokens.len(), 2);
        assert_eq!(
            get_process_first_token_offset(source, cursor),
            source.find(L!("two"))
        );
        let (current, previous) = get_token_extent(source, cursor);
        assert_eq!(&source[current], L!("β"));
        assert_eq!(&source[previous], L!("two"));
    }

    #[test]
    fn test_get_process_extent() {
        macro_rules! validate {
            ($commandline:literal, $cursor:expr, $expected_range:expr) => {
                assert_eq!(
                    get_process_extent(L!($commandline), $cursor, None),
                    $expected_range
                );
            };
        }
        validate!("for file in (path base\necho", 22, 13..22);
        validate!("begin\n\n\nec", 10, 6..10);
        validate!("begin; echo; end", 12, 12..16);
        validate!("echo hi &| less", 9, 9..9);
    }

    #[test]
    fn test_get_cmdsubst_extent() {
        let a = L!("echo (echo (echo hi");
        assert_eq!(get_cmdsubst_extent(a, 0), 0..a.len());
        assert_eq!(get_cmdsubst_extent(a, 1), 0..a.len());
        assert_eq!(get_cmdsubst_extent(a, 2), 0..a.len());
        assert_eq!(get_cmdsubst_extent(a, 3), 0..a.len());
        assert_eq!(get_cmdsubst_extent(a, 8), "echo (".chars().count()..a.len());
        assert_eq!(
            get_cmdsubst_extent(a, 17),
            "echo (echo (".chars().count()..a.len()
        );
    }
}
