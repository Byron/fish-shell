//! Multiline editing for the native Windows reader.
//!
//! Only the newline requested by the user is inserted. Existing source remains
//! editable in the same buffer; indentation is never added inside a quoted
//! string or after a line-continuation backslash, where it would change data.

use crate::ast::{self, Leaf as _, Node as _, NodeVisitor as _};
use crate::parse_constants::{ParseTokenType, ParseTreeFlags};
use crate::tokenizer::{is_token_delimiter, quote_end};
use fish_widestring::{WExt as _, WString, wstr};
use rustyline::{Cmd, ConditionalEventHandler, Event, EventContext, RepeatCount};

const SPACES_PER_INDENT: usize = 4;

pub(super) struct EnterKey {
    /// Alt-Enter adds a line even when the command is ready to execute.
    pub force_newline: bool,
}

impl ConditionalEventHandler for EnterKey {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext<'_>) -> Option<Cmd> {
        Some(enter(ctx.line(), ctx.pos(), self.force_newline))
    }
}

/// Decide whether Enter executes the complete source or continues editing it.
/// The cursor is a UTF-8 byte offset, matching the line editor's cursor API.
pub(super) fn enter(line: &str, cursor: usize, force_newline: bool) -> Cmd {
    if !force_newline && !super::fish::is_incomplete(line) {
        return Cmd::AcceptLine;
    }
    let Some(prefix) = line.get(..cursor) else {
        // The editor supplies character boundaries. Preserve the input if a
        // future caller accidentally passes an invalid position.
        return Cmd::Noop;
    };
    Cmd::Insert(1, newline(prefix))
}

fn newline(prefix: &str) -> String {
    let source = WString::from(prefix);
    let context = context(&source);
    if context.quoted || context.escaped {
        return "\n".into();
    }

    // A substitution's contents have their own AST. Parse each still-open
    // command scope separately so nested blocks inside $(...) and (...) get
    // the same indentation rules as an ordinary interactive command.
    let mut start = 0;
    let mut indent = 0;
    for open in context.substitutions {
        indent += trailing_indent(&source[start..open]) + 1;
        start = open + 1;
    }
    indent += trailing_indent(&source[start..]);
    format!("\n{}", " ".repeat(indent * SPACES_PER_INDENT))
}

#[derive(Default, Debug)]
struct Context {
    /// Unicode scalar offsets of substitutions enclosing the cursor.
    substitutions: Vec<usize>,
    quoted: bool,
    escaped: bool,
}

/// Find the current lexical scope without expanding or executing the source.
/// The shared tokenizer's quote scanner handles Fish's quote escapes and its
/// special transition from a double-quoted string into a $(...) substitution.
fn context(source: &wstr) -> Context {
    let chars = source.as_char_slice();
    let mut result = Context::default();
    let mut quoted_substitutions: Vec<bool> = Vec::new();
    let mut token_begin = true;
    let mut cursor = 0;
    while cursor < chars.len() {
        let c = chars[cursor];
        if c == '\\' {
            if cursor + 1 == chars.len() {
                result.escaped = true;
                break;
            }
            cursor += 2;
            token_begin = false;
            continue;
        }
        if c == '#' && token_begin {
            // Comments within substitutions are significant here: a quoted
            // parenthesis in a comment must never affect the indentation.
            while cursor < chars.len() && chars[cursor] != '\n' {
                cursor += 1;
            }
            token_begin = true;
            continue;
        }
        let quote = if matches!(c, '\'' | '"') {
            Some(c)
        } else if c == ')' {
            result.substitutions.pop();
            quoted_substitutions
                .pop()
                .and_then(|quoted| quoted.then_some('"'))
        } else {
            None
        };
        if let Some(quote) = quote {
            let Some(end) = quote_end(source, cursor, quote) else {
                result.quoted = true;
                break;
            };
            if chars[end] == '$' {
                // quote_end stops on the dollar of $(. The quote resumes when
                // this substitution closes, using ')' as an implicit opener.
                result.substitutions.push(end + 1);
                quoted_substitutions.push(true);
                cursor = end + 2;
                token_begin = true;
            } else {
                cursor = end + 1;
                token_begin = false;
            }
            continue;
        }
        if c == '(' {
            result.substitutions.push(cursor);
            quoted_substitutions.push(false);
        }
        token_begin = is_token_delimiter(c, chars.get(cursor + 1).copied());
        cursor += 1;
    }
    result
}

/// Use the shared Fish AST to distinguish block keywords from arguments,
/// quoted words, and command decorations. The newline lets an unfinished
/// header enter its body before we inspect its indentation.
fn trailing_indent(source: &wstr) -> usize {
    let mut source = source.to_owned();
    source.push('\n');
    let tree = ast::parse(
        &source,
        ParseTreeFlags {
            continue_after_error: true,
            include_comments: true,
            accept_incomplete_tokens: true,
            leave_unterminated: true,
            ..Default::default()
        },
        None,
    );
    let mut visitor = IndentVisitor {
        source: &source,
        parent: None,
        // The top-level job list adds one indentation level.
        indent: -1,
        trailing: 0,
    };
    visitor.visit(tree.top());
    visitor.trailing.max(0) as usize
}

/// A small native reader visitor following Fish's job-list indentation rules.
/// Unlike the POSIX renderer's per-character indent map, only the requested
/// newline needs a level here; source formatting and terminal drawing remain
/// separate from the grammar.
struct IndentVisitor<'a> {
    source: &'a wstr,
    parent: Option<&'a dyn ast::Node>,
    indent: i32,
    trailing: i32,
}

impl<'a> ast::NodeVisitor<'a> for IndentVisitor<'a> {
    fn visit(&mut self, node: &'a dyn ast::Node) {
        use ast::Kind;
        let mut increase = 0;
        let mut decrease = 0;
        match node.kind() {
            Kind::JobList(_) | Kind::AndorJobList(_) => {
                increase = 1;
                decrease = 1;
            }
            Kind::JobConjunction(_)
                if self.parent.is_some_and(|parent| {
                    matches!(parent.kind(), Kind::IfClause(_) | Kind::WhileHeader(_))
                }) =>
            {
                increase = 1;
                decrease = 1;
            }
            Kind::JobContinuation(job) if job.newlines.source(self.source).contains('\n') => {
                increase = 1;
                decrease = 1;
            }
            Kind::JobConjunctionContinuation(job)
                if job.newlines.source(self.source).contains('\n') =>
            {
                increase = 1;
                decrease = 1;
            }
            Kind::CaseItemList(_) => {
                increase = 1;
                // Keep a partial case keyword at switch-body depth even though
                // error recovery attaches it to the surrounding job list.
                decrease = i32::from(self.parent.is_some_and(|parent| {
                    matches!(parent.kind(), Kind::SwitchStatement(switch) if switch.end.has_source())
                }));
            }
            Kind::Token(token)
                if token.token_type() == ParseTokenType::End
                    && self
                        .parent
                        .is_some_and(|parent| matches!(parent.kind(), Kind::BeginHeader(_)))
                    && token.source(self.source) == "\n" =>
            {
                increase = 1;
                decrease = 1;
            }
            _ => (),
        }
        self.indent += increase;
        if increase != 0 || (node.as_leaf().is_some() && node.source_range().length() != 0) {
            self.trailing = self.indent;
        }
        let saved = self.parent.replace(node);
        node.accept(self);
        self.parent = saved;
        self.indent -= decrease;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indent(source: &str, levels: usize) {
        assert_eq!(
            newline(source),
            format!("\n{}", " ".repeat(levels * 4)),
            "{source}"
        );
    }

    #[test]
    fn blocks_branches_and_case_bodies_follow_the_fish_grammar() {
        for source in [
            "begin",
            "if true",
            "for item in one two",
            "function hello",
            "while true",
            "{",
        ] {
            indent(source, 1);
        }
        indent("begin\n    if true", 2);
        indent("begin\n    if true\n        echo hi\n    else", 2);
        indent("begin\n    if true\n        echo hi\n    else if false", 2);
        indent("begin\n    if true\n        echo hi\n    end", 1);
        indent("begin\n    if true\n    end\nend", 0);
        indent("switch value", 1);
        indent("switch value\n    case first", 2);
        indent(
            "switch value\n    case first\n        echo yes\n    case second",
            2,
        );
        indent("switch value\n    case first\n        echo yes\nend", 0);
        indent("begin\n    {\n        echo hi\n    }", 1);
    }

    #[test]
    fn keywords_in_arguments_and_comments_do_not_open_blocks() {
        for source in [
            "echo begin",
            "echo 'if'",
            "command if",
            "echo hello # if begin",
            "echo 🐟 'end'",
        ] {
            indent(source, 0);
        }
        indent("begin\n    # end ' (", 1);
        indent("begin\n    echo 🐟 end", 1);
    }

    #[test]
    fn continuations_and_nested_substitutions_have_their_own_scope() {
        indent("echo hello |", 1);
        indent("true &&", 1);
        indent("false ||", 1);
        indent("begin\n    echo hello |", 2);
        indent("echo (", 1);
        indent("echo (begin", 2);
        indent("echo (begin; end", 1);
        indent("begin\n    echo (for 🐟 in sea", 3);
        indent("echo $(echo (begin", 3);
        indent("begin\n    echo \"$(echo (begin", 4);
        indent("echo (echo 🐟 # ) '", 1);
        indent("echo (echo '🐟)'; begin", 2);
    }

    #[test]
    fn quoted_content_and_escaped_newlines_are_never_indented() {
        for source in [
            "begin\n    echo '",
            "begin\n    echo \"🐟",
            "begin\n    echo 'first\nsecond",
            "begin\n    echo \"$(echo hi)text",
            "begin\n    echo (echo '🐟",
            "begin\n    echo word\\",
            "begin\n    echo \\\\\\",
            "begin\n    echo \"backslash\\",
        ] {
            indent(source, 0);
        }
        indent("begin\n    echo \\\\", 1);
        indent("begin\n    echo \"done\"", 1);
        indent("begin\n    echo \"$(echo hi)done\"", 1);
        indent("begin\n    echo '\\\\'", 1);
        indent("begin\n    echo \\#", 1);
    }

    #[test]
    fn enter_keeps_the_entire_buffer_editable_at_unicode_boundaries() {
        assert_eq!(enter("echo 🐟", "echo 🐟".len(), false), Cmd::AcceptLine);
        let buffer = "begin\n    echo 🐟\nend";
        let cursor = "begin\n    echo 🐟".len();
        let Cmd::Insert(1, text) = enter(buffer, cursor, true) else {
            panic!("expected newline")
        };
        let mut edited = buffer.to_owned();
        edited.insert_str(cursor, &text);
        assert_eq!(edited, "begin\n    echo 🐟\n    \nend");
        assert_eq!(enter("begin", 5, false), Cmd::Insert(1, "\n    ".into()));
        assert_eq!(
            enter("echo '🐟", "echo '🐟".len(), false),
            Cmd::Insert(1, "\n".into())
        );
        assert_eq!(enter("🐟", 1, true), Cmd::Noop);
    }
}
