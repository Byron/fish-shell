//! Various mostly unrelated utility functions related to parsing, loading and evaluating fish code.
use crate::{
    ast::{self, Ast, Keyword as _, Kind, Leaf as _, Node, Token as _, Traversal, is_same_node},
    builtins::builtin_exists,
    common::{valid_var_name, valid_var_name_char},
    expand::{ExpandFlags, expand_one, expand_to_command_and_args},
    operation_context::OperationContext,
    parse_constants::{
        ERROR_BAD_VAR_CHAR1, ERROR_BRACKETED_VARIABLE_QUOTED1, ERROR_BRACKETED_VARIABLE1,
        ERROR_NO_VAR_NAME, ERROR_NOT_ARGV_AT, ERROR_NOT_ARGV_COUNT, ERROR_NOT_ARGV_STAR,
        ERROR_NOT_PID, ERROR_NOT_STATUS, INVALID_BREAK_ERR_MSG, INVALID_CONTINUE_ERR_MSG,
        INVALID_PIPELINE_CMD_ERR_MSG, ParseError, ParseErrorCode, ParseErrorList, ParseIssue,
        ParseKeyword, ParseTokenType, ParseTreeFlags, PipelinePosition, StatementDecoration,
        UNKNOWN_BUILTIN_ERR_MSG, parse_error_offset_source_start,
    },
    prelude::*,
    tokenizer::quote_end,
};
use fish_common::{UnescapeFlags, UnescapeStringStyle, help_section, unescape_string};
use fish_feature_flags::{FeatureFlag, feature_test};
use fish_wcstringutil::{count_newlines, truncate};
use fish_widestring::{
    ANY_CHAR, ANY_STRING, ANY_STRING_RECURSIVE, BRACE_BEGIN, BRACE_END, BRACE_SEP,
    INTERNAL_SEPARATOR, VARIABLE_EXPAND, VARIABLE_EXPAND_EMPTY, VARIABLE_EXPAND_SINGLE,
};
use std::iter;

pub use crate::commandline_extent::{
    CommandSubstitution, get_cmdsubst_extent, get_job_extent, get_process_extent,
    get_process_first_token_offset, get_token_extent, locate_cmdsubst_range,
};

/// Handles slices: the square brackets in an expression like $foo[5..4]
/// Return the length of the slice starting at `in`, or 0 if there is no slice, or None on error.
/// This never accepts incomplete slices.
pub fn slice_length(input: &wstr) -> Option<usize> {
    const OPENC: char = '[';
    const CLOSEC: char = ']';

    // Check for initial opening [
    let mut chars = input.chars().enumerate();
    if chars.next() != Some((0, OPENC)) {
        return Some(0);
    }
    let mut bracket_count = 1;

    while let Some((pos, c)) = chars.next() {
        match c {
            '\'' | '"' => {
                let q_pos = quote_end(input, pos, c)?;
                // We need to advance the iterator as well.
                // Subtract 1 because `nth(0)` advances by 1.
                chars.nth(q_pos - pos - 1);
            }
            OPENC => {
                bracket_count += 1;
            }
            CLOSEC => {
                bracket_count -= 1;
                if bracket_count == 0 {
                    // pos points at the closing ], so add 1.
                    return Some(pos + 1);
                }
            }
            '\\' => {
                // Escape sequence, skip the next char
                _ = chars.next();
            }
            _ => {}
        }
    }
    assert!(bracket_count > 0, "Should have unclosed brackets");

    None
}

/// Get the line number at the specified character offset.
pub fn lineno(s: &wstr, offset: usize) -> usize {
    // Return the line number of position offset, starting with 1.
    if s.is_empty() {
        return 1;
    }

    let end = offset.min(s.len());
    count_newlines(&s[..end]) + 1
}

/// Calculate the line number of the specified cursor position.
pub fn get_line_from_offset(s: &wstr, pos: usize) -> isize {
    // Return the line pos is on, or -1 if it's after the end.
    if pos > s.len() {
        return -1;
    }
    count_newlines(&s[..pos]).try_into().unwrap()
}

/// Get the offset of the first character on the specified line.
pub fn get_offset_from_line(s: &wstr, line: i32) -> Option<usize> {
    // Return the first position on line X, counting from 0.
    if line < 0 {
        return None;
    }
    if line == 0 {
        return Some(0);
    }

    let mut count = 0;
    for (pos, _) in s.chars().enumerate().filter(|(_, c)| *c == '\n') {
        count += 1;
        if count == line {
            return Some(pos + 1);
        }
    }
    None
}

/// Return the total offset of the buffer for the cursor position nearest to the specified position.
pub fn get_offset(s: &wstr, line: i32, line_offset: isize) -> Option<usize> {
    let off = get_offset_from_line(s, line)?;
    let off2 = get_offset_from_line(s, line + 1).unwrap_or(s.len() + 1);

    let mut line_offset = line_offset as usize;
    if line_offset >= off2 - off - 1 {
        line_offset = off2 - off - 1;
    }

    Some(off + line_offset)
}

/// Return the given string, unescaping wildcard characters but not performing any other character
/// transformation.
pub fn unescape_wildcards(s: &wstr) -> WString {
    fish_wcstringutil::unescape_wildcards(s, !feature_test(FeatureFlag::QuestionMarkNoGlob))
}

/// Return if the given string contains wildcard characters.
pub fn contains_wildcards(s: &wstr) -> bool {
    let unesc_qmark = !feature_test(FeatureFlag::QuestionMarkNoGlob);

    let mut i = 0;
    while i < s.len() {
        let c = s.as_char_slice()[i];
        #[allow(clippy::if_same_then_else)]
        if c == '*' {
            return true;
        } else if unesc_qmark && c == '?' {
            return true;
        } else if c == '\\' {
            if s.char_at(i + 1) == '*' {
                i += 1;
            } else if unesc_qmark && s.char_at(i + 1) == '?' {
                i += 1;
            } else if s.char_at(i + 1) == '\\' {
                // Not a wildcard, but ensure the next iteration doesn't see this escaped backslash.
                i += 1;
            }
        }
        i += 1;
    }
    false
}

/// Escape any wildcard characters in the given string. e.g. convert
/// "a*b" to "a\*b".
pub fn escape_wildcards(s: &wstr) -> WString {
    let mut result = WString::with_capacity(s.len());
    let unesc_qmark = !feature_test(FeatureFlag::QuestionMarkNoGlob);

    for c in s.chars() {
        if c == '*' {
            result.push_str("\\*");
        } else if unesc_qmark && c == '?' {
            result.push_str("\\?");
        } else if c == '\\' {
            result.push_str("\\\\");
        } else {
            result.push(c);
        }
    }
    result
}

/// Checks if the specified string is a help option.
pub fn argument_is_help(s: &wstr) -> bool {
    [L!("-h"), L!("--help")].contains(&s)
}

/// Returns true if the specified command is a builtin that may not be used in a pipeline.
fn parser_is_pipe_forbidden(word: &wstr) -> bool {
    [
        L!("exec"),
        L!("case"),
        L!("break"),
        L!("return"),
        L!("continue"),
    ]
    .contains(&word)
}

// Return a pointer to the first argument node of an argument_or_redirection_list_t, or nullptr if
// there are no arguments.
fn get_first_arg(list: &ast::ArgumentOrRedirectionList) -> Option<&ast::Argument> {
    for v in list.iter() {
        if v.is_argument() {
            return Some(v.argument());
        }
    }
    None
}

/// Given a wide character immediately after a dollar sign, return the appropriate error message.
/// For example, if wc is @, then the variable name was $@ and we suggest $argv.
fn error_for_character(c: char) -> WString {
    match c {
        '?' => wgettext!(ERROR_NOT_STATUS).to_owned(),
        '#' => wgettext!(ERROR_NOT_ARGV_COUNT).to_owned(),
        '@' => wgettext!(ERROR_NOT_ARGV_AT).to_owned(),
        '*' => wgettext!(ERROR_NOT_ARGV_STAR).to_owned(),
        _ if [
            '$',
            VARIABLE_EXPAND,
            VARIABLE_EXPAND_SINGLE,
            VARIABLE_EXPAND_EMPTY,
        ]
        .contains(&c) =>
        {
            wgettext!(ERROR_NOT_PID).to_owned()
        }
        _ if [BRACE_END, '}', ',', BRACE_SEP].contains(&c) => {
            wgettext!(ERROR_NO_VAR_NAME).to_owned()
        }
        _ => wgettext_fmt!(ERROR_BAD_VAR_CHAR1, c),
    }
}

pub use crate::indent::{SPACES_PER_INDENT, apply_indents, compute_indents};

/// Given a string, detect parse errors in it. If allow_incomplete is set, then if the string is
/// incomplete (e.g. an unclosed quote), an error is not returned and `ParseIssue::incomplete`
/// is set in the return value. If allow_incomplete is not set, then incomplete strings result in an
/// error.
pub fn detect_parse_errors(
    buff_src: &wstr,
    mut out_errors: Option<&mut ParseErrorList>,
    allow_incomplete: bool, /*=false*/
) -> Result<(), ParseIssue> {
    // Whether there's an unclosed quote or subshell, and therefore unfinished. This is only set if
    // allow_incomplete is set.
    let mut has_unclosed_quote_or_subshell = false;

    let parse_flags = ParseTreeFlags {
        leave_unterminated: allow_incomplete,
        ..Default::default()
    };

    // Parse the input string into an ast. Some errors are detected here.
    let mut parse_errors = ParseErrorList::new();
    let ast = ast::parse(buff_src, parse_flags, Some(&mut parse_errors));
    if allow_incomplete {
        // Issue #1238: If the only error was unterminated quote, then consider this to have parsed
        // successfully.
        parse_errors.retain(|parse_error| {
            if [
                ParseErrorCode::TokenizerUnterminatedQuote,
                ParseErrorCode::TokenizerUnterminatedSubshell,
            ]
            .contains(&parse_error.code)
            {
                // Remove this error, since we don't consider it a real error.
                has_unclosed_quote_or_subshell = true;
                false
            } else {
                true
            }
        });
    }

    // has_unclosed_quote_or_subshell may only be set if allow_incomplete is true.
    assert!(!has_unclosed_quote_or_subshell || allow_incomplete);
    if has_unclosed_quote_or_subshell {
        // We do not bother to validate the rest of the tree in this case.
        return ParseIssue::INCOMPLETE;
    }

    // Early parse error, stop here.
    if !parse_errors.is_empty() {
        if let Some(errors) = out_errors.as_mut() {
            errors.extend(parse_errors);
        }
        return ParseIssue::ERROR;
    }

    // Defer to the tree-walking version.
    detect_parse_errors_in_ast(&ast, buff_src, out_errors)
}

/// Like detect_parse_errors but accepts an already-parsed ast.
/// The top of the ast is assumed to be a job list.
pub fn detect_parse_errors_in_ast(
    ast: &Ast,
    buff_src: &wstr,
    mut out_errors: Option<&mut ParseErrorList>,
) -> Result<(), ParseIssue> {
    // The issue to return.
    // We break out various reasons for incompleteness to be explicit.
    let mut issue = ParseIssue::default();

    // Whether we encountered an unclosed block. We detect this via an 'end_command' block without
    // source.
    let mut has_unclosed_block = false;

    // Whether we encounter a missing statement, i.e. a newline after a pipe. This is found by
    // detecting job_continuations that have source for pipes but not the statement.
    let mut has_unclosed_pipe = false;

    // Whether we encounter a missing job, i.e. a newline after && or ||. This is found by
    // detecting job_conjunction_continuations that have source for && or || but not the job.
    let mut has_unclosed_conjunction = false;

    // Expand all commands.
    // Verify 'or' and 'and' not used inside pipelines.
    // Verify return only within a function.
    // Verify no variable expansions.

    let mut traversal = ast::Traversal::new(ast.top());
    while let Some(node) = traversal.next() {
        match node.kind() {
            Kind::JobContinuation(jc)
                // Somewhat clumsy way of checking for a statement without source in a pipeline.
                // See if our pipe has source but our statement does not.
                if jc.pipe.has_source() && jc.statement.try_source_range().is_none() => {
                    has_unclosed_pipe = true;
                }
            Kind::JobConjunction(job_conjunction) => {
                issue.error |= detect_errors_in_job_conjunction(job_conjunction, &mut out_errors);
            }
            Kind::JobConjunctionContinuation(jcc)
                // Somewhat clumsy way of checking for a job without source in a conjunction.
                // See if our conjunction operator (&& or ||) has source but our job does not.
                if jcc.conjunction.has_source() && jcc.job.try_source_range().is_none() => {
                    has_unclosed_conjunction = true;
                }
            Kind::Argument(arg) => {
                let arg_src = arg.source(buff_src);
                if let Err(e) = detect_errors_in_argument(arg, arg_src, &mut out_errors) {
                    issue.error |= e.error;
                    issue.incomplete |= e.incomplete;
                }
            }
            Kind::JobPipeline(job)
                // Disallow background in the following cases:
                //
                // foo & ; and bar
                // foo & ; or bar
                // if foo & ; end
                // while foo & ; end
                // If it's not a background job, nothing to do.
                if job.bg.is_some() => {
                    issue.error |=
                        detect_errors_in_backgrounded_job(&traversal, job, &mut out_errors);
                }
            Kind::DecoratedStatement(stmt) => {
                issue.error |= detect_errors_in_decorated_statement(
                    buff_src,
                    &traversal,
                    stmt,
                    &mut out_errors,
                );
            }
            Kind::BlockStatement(block) => {
                // If our 'end' had no source, we are unsourced.
                if !block.end.has_source() {
                    has_unclosed_block = true;
                }
                issue.error |= detect_errors_in_block_redirection_list(
                    node,
                    &block.args_or_redirs,
                    &mut out_errors,
                );
            }
            Kind::BraceStatement(brace_statement) => {
                // If our closing brace had no source, we are unsourced.
                if !brace_statement.right_brace.has_source() {
                    has_unclosed_block = true;
                }
                issue.error |= detect_errors_in_block_redirection_list(
                    node,
                    &brace_statement.args_or_redirs,
                    &mut out_errors,
                );
            }
            Kind::IfStatement(ifs) => {
                // If our 'end' had no source, we are unsourced.
                if !ifs.end.has_source() {
                    has_unclosed_block = true;
                }
                issue.error |= detect_errors_in_block_redirection_list(
                    node,
                    &ifs.args_or_redirs,
                    &mut out_errors,
                );
            }
            Kind::SwitchStatement(switchs) => {
                // If our 'end' had no source, we are unsourced.
                if !switchs.end.has_source() {
                    has_unclosed_block = true;
                }
                issue.error |= detect_errors_in_block_redirection_list(
                    node,
                    &switchs.args_or_redirs,
                    &mut out_errors,
                );
            }
            _ => {}
        }
    }

    issue.incomplete |= has_unclosed_block || has_unclosed_pipe || has_unclosed_conjunction;
    if issue.error || issue.incomplete {
        Err(issue)
    } else {
        Ok(())
    }
}

/// Detect errors in the specified string when parsed as an argument list. Returns the text of an
/// error, or none if no error occurred.
pub fn detect_errors_in_argument_list(arg_list_src: &wstr, prefix: &wstr) -> Result<(), WString> {
    // Helper to return a description of the first error.
    let get_error_text = |errors: &ParseErrorList| {
        assert!(!errors.is_empty(), "Expected an error");
        Err(errors[0].describe_with_prefix(
            arg_list_src,
            prefix,
            false, /* not interactive */
            false, /* don't skip caret */
        ))
    };

    // Parse the string as a freestanding argument list.
    let mut errors = ParseErrorList::new();
    let ast = ast::parse_argument_list(arg_list_src, ParseTreeFlags::default(), Some(&mut errors));
    if !errors.is_empty() {
        return get_error_text(&errors);
    }

    // Get the root argument list and extract arguments from it.
    // Test each of these.
    let arg_list: &ast::FreestandingArgumentList = ast.top();
    let args = &arg_list.arguments;
    for arg in args.iter() {
        let arg_src = arg.source(arg_list_src);
        if detect_errors_in_argument(arg, arg_src, &mut Some(&mut errors)).is_err() {
            return get_error_text(&errors);
        }
    }
    Ok(())
}

/// Append a syntax error to the given error list.
macro_rules! append_syntax_error {
    (
        $errors:expr, $source_location:expr,
        $source_length:expr, $fmt:expr
        $(, $arg:expr)* $(,)?
    ) => {
        {
            append_syntax_error_formatted!(
                $errors, $source_location, $source_length,
                wgettext_fmt!($fmt $(, $arg)*))
        }
    }
}

macro_rules! append_syntax_error_formatted {
    (
        $errors:expr, $source_location:expr,
        $source_length:expr, $text:expr
    ) => {{
        if let Some(ref mut errors) = $errors.as_mut() {
            let mut error = ParseError::default();
            error.source_start = $source_location;
            error.source_length = $source_length;
            error.code = ParseErrorCode::Syntax;
            error.text = $text;
            errors.push(error);
        }
        true
    }};
}

/// Test if this argument contains any errors. Detected errors include syntax errors in command
/// substitutions, improperly escaped characters and improper use of the variable expansion
/// operator.
pub fn detect_errors_in_argument(
    arg: &ast::Argument,
    arg_src: &wstr,
    out_errors: &mut Option<&mut ParseErrorList>,
) -> Result<(), ParseIssue> {
    let Some(source_range) = arg.try_source_range() else {
        return Ok(());
    };

    let source_start = source_range.start();
    let mut issue = ParseIssue::default();

    // Check if a subtoken contains errors. Returns true if there is an error, and appends to out_errors if provided.
    let check_subtoken =
        |begin: usize, end: usize, out_errors: &mut Option<&mut ParseErrorList>| -> bool {
            let Some(unesc) = unescape_string(
                &arg_src[begin..end],
                UnescapeStringStyle::Script(UnescapeFlags {
                    special: true,
                    ..Default::default()
                }),
            ) else {
                if out_errors.is_some() {
                    let src = arg_src.as_char_slice();
                    if src.len() == 2
                        && src[0] == '\\'
                        && (src[1] == 'c'
                            || src[1].eq_ignore_ascii_case(&'u')
                            || src[1].eq_ignore_ascii_case(&'x'))
                    {
                        append_syntax_error!(
                            out_errors,
                            source_start + begin,
                            end - begin,
                            "Incomplete escape sequence '%s'",
                            arg_src
                        );
                        return true;
                    }
                    append_syntax_error!(
                        out_errors,
                        source_start + begin,
                        end - begin,
                        "Invalid token '%s'",
                        arg_src
                    );
                }
                return true;
            };

            let mut errored = false;
            // Check for invalid variable expansions.
            let unesc = unesc.as_char_slice();
            for (idx, c) in unesc.iter().enumerate() {
                if ![VARIABLE_EXPAND, VARIABLE_EXPAND_SINGLE].contains(c) {
                    continue;
                }
                let next_char = unesc.get(idx + 1).copied().unwrap_or('\0');
                if !matches!(next_char, VARIABLE_EXPAND | VARIABLE_EXPAND_SINGLE | '(')
                    && !valid_var_name_char(next_char)
                {
                    errored = true;
                    if let Some(out_errors) = out_errors {
                        let mut first_dollar = idx;
                        while first_dollar > 0
                            && [VARIABLE_EXPAND, VARIABLE_EXPAND_SINGLE]
                                .contains(&unesc[first_dollar - 1])
                        {
                            first_dollar -= 1;
                        }
                        expand_variable_error(unesc.into(), source_start, first_dollar, out_errors);
                    }
                }
            }

            errored
        };

    let mut cursor = 0;
    let mut checked = 0;

    let mut do_loop = true;
    let mut is_quoted = false;
    while do_loop {
        let mut has_dollar = false;
        match locate_cmdsubst_range(
            arg_src,
            &mut cursor,
            false,
            Some(&mut is_quoted),
            Some(&mut has_dollar),
        ) {
            Err(()) => {
                issue.error = true;
                append_syntax_error!(out_errors, source_start, 1, "Mismatched parenthesis");
                return Err(issue);
            }
            Ok(None) => {
                do_loop = false;
            }
            Ok(Some(cmdsub)) => {
                issue.error |= check_subtoken(
                    checked,
                    cmdsub.opening_paren_offset() - if has_dollar { 1 } else { 0 },
                    out_errors,
                );
                let mut subst_errors = ParseErrorList::new();
                if let Err(e) = detect_parse_errors(
                    &arg_src[cmdsub.command_range()],
                    Some(&mut subst_errors),
                    false,
                ) {
                    issue.error |= e.error;
                    issue.incomplete |= e.incomplete;
                }

                // Our command substitution produced error offsets relative to its source. Tweak the
                // offsets of the errors in the command substitution to account for both its offset
                // within the string, and the offset of the node.
                let error_offset = cmdsub.opening_paren_offset() + 1 + source_start;
                parse_error_offset_source_start(&mut subst_errors, error_offset);
                if let Some(out_errors) = out_errors {
                    out_errors.extend(subst_errors);
                }

                checked = cmdsub.end();
            }
        }
    }

    issue.error |= check_subtoken(checked, arg_src.len(), out_errors);
    if issue.error || issue.incomplete {
        Err(issue)
    } else {
        Ok(())
    }
}

fn detect_errors_in_job_conjunction(
    job_conjunction: &ast::JobConjunction,
    parse_errors: &mut Option<&mut ParseErrorList>,
) -> bool {
    // Disallow background immediately before conjunction continuations. For example:
    // foo & && bar
    // foo & || baz
    let continuations = &job_conjunction.continuations;
    let jobs = iter::once(&job_conjunction.job)
        .chain(continuations.iter().map(|continuation| &continuation.job));
    for (job, continuation) in jobs.zip(continuations.iter()) {
        if job.bg.is_some() {
            let conjunction = &continuation.conjunction;
            return append_syntax_error!(
                parse_errors,
                conjunction.source_range().start(),
                conjunction.source_range().length(),
                BOOL_AFTER_BACKGROUND_ERROR_MSG,
                if conjunction.token_type() == ParseTokenType::AndAnd {
                    L!("&&")
                } else {
                    L!("||")
                }
            );
        }
    }

    false
}

/// Given that the job should be backgrounded, return true if we detect any errors.
fn detect_errors_in_backgrounded_job(
    traversal: &Traversal,
    job: &ast::JobPipeline,
    parse_errors: &mut Option<&mut ParseErrorList>,
) -> bool {
    let Some(source_range) = job.try_source_range() else {
        return false;
    };

    let mut errored = false;
    // Disallow background in the following cases:
    // foo & ; and bar
    // foo & ; or bar
    // if foo & ; end
    // while foo & ; end
    let Kind::JobConjunction(job_conj) = traversal.parent(job).kind() else {
        return false;
    };

    let job_conj_parent = traversal.parent(job_conj);
    if matches!(
        job_conj_parent.kind(),
        Kind::IfClause(_) | Kind::WhileHeader(_)
    ) {
        errored = append_syntax_error!(
            parse_errors,
            source_range.start(),
            source_range.length(),
            BACKGROUND_IN_CONDITIONAL_ERROR_MSG
        );
    } else if let Kind::JobList(jlist) = job_conj_parent.kind() {
        // This isn't very complete, e.g. we don't catch 'foo & ; not and bar'.
        // Find the index of ourselves in the job list.
        let index = jlist
            .iter()
            .position(|job| is_same_node(job, job_conj))
            .expect("Should have found the job in the list");

        // Try getting the next job and check its decorator.
        if let Some(next) = jlist.get(index + 1)
            && let Some(deco) = &next.decorator
        {
            assert!(
                [ParseKeyword::And, ParseKeyword::Or].contains(&deco.keyword()),
                "Unexpected decorator keyword"
            );
            let deco_name = if deco.keyword() == ParseKeyword::And {
                L!("and")
            } else {
                L!("or")
            };
            errored = append_syntax_error!(
                parse_errors,
                deco.source_range().start(),
                deco.source_range().length(),
                BOOL_AFTER_BACKGROUND_ERROR_MSG,
                deco_name
            );
        }
    }
    errored
}

/// Given a source buffer `buff_src` and decorated statement `dst` within it, return true if there
/// is an error and false if not.
fn detect_errors_in_decorated_statement(
    buff_src: &wstr,
    traversal: &ast::Traversal,
    dst: &ast::DecoratedStatement,
    parse_errors: &mut Option<&mut ParseErrorList>,
) -> bool {
    let mut errored = false;
    let source_start = dst.source_range().start();
    let source_length = dst.source_range().length();
    let decoration = dst.decoration();

    // Determine if the first argument is help.
    let mut first_arg_is_help = false;
    if let Some(arg) = get_first_arg(&dst.args_or_redirs) {
        let arg_src = arg.source(buff_src);
        first_arg_is_help = argument_is_help(arg_src);
    }

    // Get the statement we are part of.
    let Kind::Statement(st) = traversal.parent(dst).kind() else {
        panic!();
    };

    // Walk up to the job.
    let job = traversal
        .parent_nodes()
        .find_map(|n| match n.kind() {
            Kind::JobPipeline(job) => Some(job),
            _ => None,
        })
        .expect("should have found the job");

    // Check our pipeline position.
    let pipe_pos = if job.continuation.is_empty() {
        PipelinePosition::None
    } else if is_same_node(&job.statement, st) {
        PipelinePosition::First
    } else {
        PipelinePosition::Subsequent
    };

    // Check that we don't try to pipe through exec.
    let is_in_pipeline = pipe_pos != PipelinePosition::None;
    if is_in_pipeline && decoration == StatementDecoration::Exec {
        errored = append_syntax_error!(
            parse_errors,
            source_start,
            source_length,
            INVALID_PIPELINE_CMD_ERR_MSG,
            "exec"
        );
    }

    // This is a somewhat stale check that 'and' and 'or' are not in pipelines, except at the
    // beginning. We can't disallow them as commands entirely because we need to support 'and
    // --help', etc.
    if pipe_pos == PipelinePosition::Subsequent {
        // We only reject it if we have no decoration.
        // `echo foo | command time something`
        // is entirely fair and valid.
        // Other current decorations like "exec"
        // are already forbidden.
        if dst.decoration() == StatementDecoration::None {
            // check if our command is 'and' or 'or'. This is very clumsy; we don't catch e.g. quoted
            // commands.
            let command = dst.command.source(buff_src);
            if [L!("and"), L!("or")].contains(&command) {
                errored = append_syntax_error!(
                    parse_errors,
                    source_start,
                    source_length,
                    INVALID_PIPELINE_CMD_ERR_MSG,
                    command
                );
            }

            // Similarly for time (#8841).
            if command == "time" {
                errored = append_syntax_error!(
                    parse_errors,
                    source_start,
                    source_length,
                    TIME_IN_PIPELINE_ERR_MSG
                );
            }
        }
    }

    // $status specifically is invalid as a command,
    // to avoid people trying `if $status`.
    // We see this surprisingly regularly.
    let com = dst.command.source(buff_src);
    if com == "$status" {
        errored = append_syntax_error!(
            parse_errors,
            source_start,
            source_length,
            "$status is not valid as a command. See `help %s`",
            help_section!("language#conditions")
        );
    }

    let unexp_command = com;
    if !unexp_command.is_empty() {
        // Check that we can expand the command.
        // Make a new error list so we can fix the offset for just those, then append later.
        let mut new_errors = ParseErrorList::new();
        let mut command = WString::new();
        if expand_to_command_and_args(
            unexp_command,
            &mut OperationContext::empty(),
            &mut command,
            None,
            Some(&mut new_errors),
            /*skip_cmdsubs=*/ true,
            /*skip_wildcards=*/ true,
        )
        .failed()
        {
            errored = true;
        }

        // Check that pipes are sound.
        if !errored && parser_is_pipe_forbidden(&command) && is_in_pipeline {
            errored = append_syntax_error!(
                parse_errors,
                source_start,
                source_length,
                INVALID_PIPELINE_CMD_ERR_MSG,
                command
            );
        }

        // Check that we don't break or continue from outside a loop.
        if !errored && (command == "break" || command == "continue") && !first_arg_is_help {
            // Walk up until we hit a 'for' or 'while' loop. If we hit a function first,
            // stop the search; we can't break an outer loop from inside a function.
            // This is a little funny because we can't tell if it's a 'for' or 'while'
            // loop from the ancestor alone; we need the header. That is, we hit a
            // block_statement, and have to check its header.
            let mut found_loop = false;
            for block in traversal.parent_nodes().filter_map(|anc| match anc.kind() {
                Kind::BlockStatement(block) => Some(block),
                _ => None,
            }) {
                match block.header {
                    ast::BlockStatementHeader::For(_) | ast::BlockStatementHeader::While(_) => {
                        // This is a loop header, so we can break or continue.
                        found_loop = true;
                        break;
                    }
                    ast::BlockStatementHeader::Function(_) => {
                        // This is a function header, so we cannot break or
                        // continue. We stop our search here.
                        found_loop = false;
                        break;
                    }
                    _ => {}
                }
            }

            if !found_loop {
                errored = if command == "break" {
                    append_syntax_error!(
                        parse_errors,
                        source_start,
                        source_length,
                        INVALID_BREAK_ERR_MSG
                    )
                } else {
                    append_syntax_error!(
                        parse_errors,
                        source_start,
                        source_length,
                        INVALID_CONTINUE_ERR_MSG
                    )
                }
            }
        }

        // Check that we don't do an invalid builtin (issue #1252).
        if !errored && decoration == StatementDecoration::Builtin {
            let mut command = unexp_command.to_owned();
            if expand_one(
                &mut command,
                ExpandFlags::FAIL_ON_CMDSUBST,
                &mut OperationContext::empty(),
                match parse_errors {
                    Some(pe) => Some(pe),
                    None => None,
                },
            ) && !builtin_exists(unexp_command)
            {
                errored = append_syntax_error!(
                    parse_errors,
                    source_start,
                    source_length,
                    UNKNOWN_BUILTIN_ERR_MSG,
                    unexp_command
                );
            }
        }

        if let Some(parse_errors) = parse_errors {
            // The expansion errors here go from the *command* onwards,
            // so we need to offset them by the *command* offset,
            // excluding the decoration.
            parse_error_offset_source_start(&mut new_errors, dst.command.source_range().start());
            parse_errors.extend(new_errors);
        }
    }
    errored
}

// Given we have a trailing ArgumentOrRedirectionList, like `begin; end > /dev/null`, verify that
// there are no arguments in the list. The parent of the list is provided.
fn detect_errors_in_block_redirection_list(
    parent: &dyn Node,
    args_or_redirs: &ast::ArgumentOrRedirectionList,
    out_errors: &mut Option<&mut ParseErrorList>,
) -> bool {
    let Some(first_arg) = get_first_arg(args_or_redirs) else {
        return false;
    };
    let r = first_arg.source_range();
    if let Kind::BraceStatement(_) = parent.kind() {
        append_syntax_error!(out_errors, r.start(), r.length(), RIGHT_BRACE_ARG_ERR_MSG);
    } else {
        append_syntax_error!(out_errors, r.start(), r.length(), END_ARG_ERR_MSG);
    }
    true
}

/// Given a string containing a variable expansion error, append an appropriate error to the errors
/// list. The global_token_pos is the offset of the token in the larger source, and the dollar_pos
/// is the offset of the offending dollar sign within the token.
pub fn expand_variable_error(
    token: &wstr,
    global_token_pos: usize,
    dollar_pos: usize,
    errors: &mut ParseErrorList,
) {
    let mut errors = Some(errors);
    // Note that dollar_pos is probably VARIABLE_EXPAND or VARIABLE_EXPAND_SINGLE, not a literal
    // dollar sign.
    let token = token.as_char_slice();
    let double_quotes = token[dollar_pos] == VARIABLE_EXPAND_SINGLE;
    let start_error_count = errors.as_ref().unwrap().len();
    let global_dollar_pos = global_token_pos + dollar_pos;
    let global_after_dollar_pos = global_dollar_pos + 1;
    let char_after_dollar = token.get(dollar_pos + 1).copied().unwrap_or('\0');

    match char_after_dollar {
        BRACE_BEGIN | '{' => {
            // The BRACE_BEGIN is for unquoted, the { is for quoted. Anyways we have (possible
            // quoted) ${. See if we have a }, and the stuff in between is variable material. If so,
            // report a bracket error. Otherwise just complain about the ${.
            let mut looks_like_variable = false;
            let closing_bracket = token
                .iter()
                .skip(dollar_pos + 2)
                .position(|c| {
                    *c == if char_after_dollar == '{' {
                        '}'
                    } else {
                        BRACE_END
                    }
                })
                .map(|p| p + dollar_pos + 2);
            let mut var_name = L!("");
            if let Some(var_end) = closing_bracket {
                let var_start = dollar_pos + 2;
                var_name = (&token[var_start..var_end]).into();
                looks_like_variable = valid_var_name(var_name);
            }
            if looks_like_variable {
                if double_quotes {
                    append_syntax_error!(
                        errors,
                        global_after_dollar_pos,
                        1,
                        ERROR_BRACKETED_VARIABLE_QUOTED1,
                        truncate(var_name, VAR_ERR_LEN)
                    );
                } else {
                    append_syntax_error!(
                        errors,
                        global_after_dollar_pos,
                        1,
                        ERROR_BRACKETED_VARIABLE1,
                        truncate(var_name, VAR_ERR_LEN),
                    );
                }
            } else {
                append_syntax_error!(errors, global_after_dollar_pos, 1, ERROR_BAD_VAR_CHAR1, '{');
            }
        }
        INTERNAL_SEPARATOR => {
            // e.g.: echo foo"$"baz
            // These are only ever quotes, not command substitutions. Command substitutions are
            // handled earlier.
            append_syntax_error!(errors, global_dollar_pos, 1, ERROR_NO_VAR_NAME);
        }
        '\0' => {
            append_syntax_error!(errors, global_dollar_pos, 1, ERROR_NO_VAR_NAME);
        }
        _ => {
            let mut token_stop_char = char_after_dollar;
            // Unescape (see issue #50).
            if token_stop_char == ANY_CHAR {
                token_stop_char = '?';
            } else if [ANY_STRING, ANY_STRING_RECURSIVE].contains(&token_stop_char) {
                token_stop_char = '*';
            }

            // Determine which error message to use. The format string may not consume all the
            // arguments we pass but that's harmless.
            append_syntax_error_formatted!(
                errors,
                global_after_dollar_pos,
                1,
                error_for_character(token_stop_char)
            );
        }
    }

    // We should have appended exactly one error.
    assert_eq!(errors.as_ref().unwrap().len(), start_error_count + 1);
}

localizable_consts!(
    /// Error message for use of backgrounded commands before and/or.
    pub(crate) BOOL_AFTER_BACKGROUND_ERROR_MSG
    "The '%s' command can not be used immediately after a backgrounded job"

    /// Error message for backgrounded commands as conditionals.
    BACKGROUND_IN_CONDITIONAL_ERROR_MSG
    "Backgrounded commands can not be used as conditionals"

    /// Error message for arguments to 'end'
    END_ARG_ERR_MSG
    "'end' does not take arguments. Did you forget a ';'?"

    RIGHT_BRACE_ARG_ERR_MSG
    "'}' does not take arguments. Did you forget a ';'?"

    /// Error message when 'time' is in a pipeline.
    TIME_IN_PIPELINE_ERR_MSG
    "The 'time' command may only be at the beginning of a pipeline"
);

/// Maximum length of a variable name to show in error reports before truncation
const VAR_ERR_LEN: usize = 16;

#[cfg(test)]
mod tests {
    use super::{
        BOOL_AFTER_BACKGROUND_ERROR_MSG, compute_indents, detect_parse_errors, slice_length,
    };
    use crate::parse_constants::{
        ERROR_BAD_VAR_CHAR1, ERROR_BRACKETED_VARIABLE_QUOTED1, ERROR_BRACKETED_VARIABLE1,
        ERROR_NO_VAR_NAME, ERROR_NOT_ARGV_AT, ERROR_NOT_ARGV_COUNT, ERROR_NOT_ARGV_STAR,
        ERROR_NOT_PID, ERROR_NOT_STATUS,
    };
    use crate::prelude::*;
    use crate::tests::prelude::*;
    use pcre2::utf32::Regex;

    #[test]
    #[serial]
    fn test_error_messages() {
        test_init();
        // Given a format string, returns a list of non-empty strings separated by format specifiers. The
        // format specifiers themselves are omitted.
        fn separate_by_format_specifiers(format: &wstr) -> Vec<&wstr> {
            let format_specifier_regex = Regex::new(L!(r"%[cds]").as_char_slice()).unwrap();
            let mut result = vec![];
            let mut offset = 0;
            for mtch in format_specifier_regex.find_iter(format.as_char_slice()) {
                let mtch = mtch.unwrap();
                let component = &format[offset..mtch.start()];
                result.push(component);
                offset = mtch.end();
            }
            result.push(&format[offset..]);
            // Avoid mismatch from localized  quotes.
            for component in &mut result {
                *component = component.trim_matches('\'');
            }
            result
        }

        // Given a format string 'format', return true if the string may have been produced by that format
        // string. We do this by splitting the format string around the format specifiers, and then ensuring
        // that each of the remaining chunks is found (in order) in the string.
        fn string_matches_format(s: &wstr, format: &wstr) -> bool {
            let components = separate_by_format_specifiers(format);
            assert!(!components.is_empty());
            let mut idx = 0;
            for component in components {
                let Some(relpos) = s[idx..].find(component) else {
                    return false;
                };
                idx += relpos + component.len();
                assert!(idx <= s.len());
            }
            true
        }

        macro_rules! validate {
            ($src:expr, $error_text_format:expr) => {
                let mut errors = vec![];
                let res = detect_parse_errors(L!($src), Some(&mut errors), false);
                let fmt = wgettext!($error_text_format);
                assert!(res.is_err());
                assert!(
                    string_matches_format(&errors[0].text, fmt),
                    "command '{}' is expected to match error pattern '{}' but is '{}'",
                    $src,
                    $error_text_format.localize(),
                    &errors[0].text
                );
            };
        }

        validate!("echo $^", ERROR_BAD_VAR_CHAR1);
        validate!("echo foo${a}bar", ERROR_BRACKETED_VARIABLE1);
        validate!("echo foo\"${a}\"bar", ERROR_BRACKETED_VARIABLE_QUOTED1);
        validate!("echo foo\"${\"bar", ERROR_BAD_VAR_CHAR1);
        validate!("echo $?", ERROR_NOT_STATUS);
        validate!("echo $$", ERROR_NOT_PID);
        validate!("echo $#", ERROR_NOT_ARGV_COUNT);
        validate!("echo $@", ERROR_NOT_ARGV_AT);
        validate!("echo $*", ERROR_NOT_ARGV_STAR);
        validate!("echo $", ERROR_NO_VAR_NAME);
        validate!("echo foo\"$\"bar", ERROR_NO_VAR_NAME);
        validate!("echo \"foo\"$\"bar\"", ERROR_NO_VAR_NAME);
        validate!("echo foo $ bar", ERROR_NO_VAR_NAME);
        validate!("echo 1 & && echo 2", BOOL_AFTER_BACKGROUND_ERROR_MSG);
        validate!(
            "echo 1 && echo 2 & && echo 3",
            BOOL_AFTER_BACKGROUND_ERROR_MSG
        );
    }

    #[test]
    #[serial]
    fn test_slice_length() {
        test_init();
        assert_eq!(slice_length(L!("")), Some(0));
        assert_eq!(slice_length(L!("foo]bar")), Some(0));
        assert_eq!(slice_length(L!("[2]bar")), Some(3));
        assert_eq!(slice_length(L!("[12]bar")), Some(4));

        assert_eq!(slice_length(L!("[\"foo\"]bar")), Some(7));
        assert_eq!(slice_length(L!("[\"fo]o\"]bar")), Some(8));
        assert_eq!(slice_length(L!("[\"foo\"")), None);

        assert_eq!(slice_length(L!("['foo']bar")), Some(7));
        assert_eq!(slice_length(L!("['fo]o']bar")), Some(8));
        assert_eq!(slice_length(L!("['foo'")), None);

        assert_eq!(slice_length(L!("[fo\\]o]bar")), Some(7));
        assert_eq!(slice_length(L!("[foo\\")), None);
    }

    #[test]
    #[serial]
    fn test_indents() {
        test_init();
        // A struct which is either text or a new indent.
        struct Segment {
            // The indent to set
            indent: i32,
            text: &'static str,
        }
        fn do_validate(segments: &[Segment]) {
            // Compute the indents.
            let mut expected_indents = vec![];
            let mut text = WString::new();
            for segment in segments {
                text.push_str(segment.text);
                for _ in segment.text.chars() {
                    expected_indents.push(segment.indent);
                }
            }
            let indents = compute_indents(&text);
            assert_eq!(indents, expected_indents);
        }
        macro_rules! validate {
            ( $( $(,)? $indent:literal, $text:literal )* $(,)?  ) => {
                let segments = vec![
                    $(
                        Segment{ indent: $indent, text: $text },
                    )*
                ];
                do_validate(&segments);
            };
        }

        // TODO: feature(stmt_expr_attributes): use #[rustfmt::skip]
        #[cfg_attr(any(), rustfmt::skip)]
        {
            validate!(
                0, "if", 1, " foo",
                0, "\nend"
            );
            validate!(
                0, "if", 1, " foo",
                1, "\nfoo",
                0, "\nend"
            );

            validate!(
                0, "if", 1, " foo",
                1, "\nif", 2, " bar",
                1, "\nend",
                0, "\nend"
            );

            validate!(
                0, "if", 1, " foo",
                1, "\nif", 2, " bar",
                2, "\n",
                1, "\nend\n"
            );

            validate!(
                0, "if", 1, " foo",
                1, "\nif", 2, " bar",
                2, "\n"
            );

            validate!(
                0, "begin",
                1, "\nfoo",
                1, "\n"
            );

            validate!(
                0, "begin",
                1, "\n;",
                0, "end",
                0, "\nfoo", 0, "\n"
            );

            validate!(
                0, "begin",
                1, "\n;",
                0, "end",
                0, "\nfoo", 0, "\n"
            );

            validate!(
                0, "if", 1, " foo",
                1, "\nif", 2, " bar",
                2, "\nbaz",
                1, "\nend", 1, "\n"
            );

            validate!(
                0, "switch foo",
                1, "\n"
            );

            validate!(
                0, "switch foo",
                1, "\ncase bar",
                1, "\ncase baz",
                2, "\nquux",
                2, "\nquux"
            );

            validate!(
                0,
                "switch foo",
                1,
                "\ncas" // parse error indentation handling
            );

            validate!(
                0, "while",
                1, " false",
                1, "\n# comment", // comment indentation handling
                1, "\ncommand",
                1, "\n# comment 2"
            );

            validate!(
                0, "begin",
                1, "\n", // "begin" is special because this newline belongs to the block header
                1, "\n"
            );

            // Continuation lines.
            validate!(
                0, "echo 'continuation line' \\",
                1, "\ncont",
                0, "\n"
            );
            validate!(
                0, "echo 'empty continuation line' \\",
                1, "\n"
            );
            validate!(
                0, "begin # continuation line in block",
                1, "\necho \\",
                2, "\ncont"
            );
            validate!(
                0, "begin # empty continuation line in block",
                1, "\necho \\",
                2, "\n",
                0, "\nend"
            );
            validate!(
                0, "echo 'multiple continuation lines' \\",
                1, "\nline1 \\",
                1, "\n# comment",
                1, "\n# more comment",
                1, "\nline2 \\",
                1, "\n"
            );
            validate!(
                0, "echo # inline comment ending in \\",
                0, "\nline"
            );
            validate!(
                0, "# line comment ending in \\",
                0, "\nline"
            );
            validate!(
                0, "echo 'multiple empty continuation lines' \\",
                1, "\n\\",
                1, "\n",
                0, "\n"
            );
            validate!(
                0, "echo 'multiple statements with continuation lines' \\",
                1, "\nline 1",
                0, "\necho \\",
                1, "\n"
            );
            // This is an edge case, probably okay to change the behavior here.
            validate!(
                0, "begin",
                1, " \\",
                2, "\necho 'continuation line in block header' \\",
                2, "\n",
                1, "\n",
                0, "\nend"
            );
            validate!(
                0, "if", 1,  " true",
                1, "\n    begin",
                2,  "\n        echo",
                1,  "\n    end",
                0,  "\nend",
            );

            // Quotes and command substitutions.
            validate!(
                0, "if", 1, " foo \"",
                0, "\nquoted",
            );
            validate!(
                0, "if", 1, " foo \"",
                0, "\n",
            );
            validate!(
                0, "echo (",
                1, "\n", // )
            );
            validate!(
                0, "echo \"$(",
                1, "\n" // )
            );
            validate!(
                0, "echo (", // )
                1, "\necho \"",
                0, "\n"
            );
            validate!(
                0, "echo (", // )
                1, "\necho (", // )
                2, "\necho"
            );
            validate!(
                0, "if", 1, " true",
                1, "\n    echo \"line1",
                0, "\nline2 ", 1, "$(",
                2, "\n    echo line3",
                0, "\n) line4",
                0, "\nline5\"",
            );
            validate!(
                0, r#"echo "$()"'"#,
                0, "\n"
            );
            validate!(
                0, r#"""#,
                0, "\n",
                0, r#"$()"$() ""#
            );
        }
    }
}
