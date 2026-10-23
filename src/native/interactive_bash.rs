//! Non-executing lexical support for Bash-shaped input in the native Fish reader.
//!
//! Submission and completeness use the shared Bash-to-Fish translator. This
//! deliberately tolerant view only supplies colors and indentation while a
//! command is still being edited; it never interprets or evaluates its words.

use super::interactive_highlight::Role;
use fish_widestring::WString;
use std::ops::Range;

pub(super) fn is_incomplete(source: &str) -> bool {
    crate::bash_to_fish::translate(&WString::from(source)).is_err_and(|error| error.incomplete)
}

pub(super) fn newline(prefix: &str) -> String {
    let view = View::new(prefix);
    if view.literal {
        "\n".into()
    } else {
        format!("\n{}", " ".repeat(view.indent * 4))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Block {
    If,
    Loop,
    Group,
}

struct Frame {
    word: Option<usize>,
    quote: Option<(char, usize)>,
    command: bool,
    redirect: bool,
    loop_header: bool,
    continuation: bool,
    blocks: Vec<Block>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            word: None,
            quote: None,
            command: true,
            redirect: false,
            loop_header: false,
            continuation: false,
            blocks: Vec::new(),
        }
    }
}

pub(super) struct View {
    /// Byte-indexed roles; UTF-8 continuation bytes have the same role.
    pub roles: Vec<Role>,
    pub pairs: Vec<(usize, usize)>,
    pub commands: Vec<Range<usize>>,
    indent: usize,
    literal: bool,
}

impl View {
    pub fn new(source: &str) -> Self {
        let mut result = Self {
            roles: vec![Role::Normal; source.len()],
            pairs: Vec::new(),
            commands: Vec::new(),
            indent: 0,
            literal: false,
        };
        let mut frame = Frame::default();
        let mut scopes = Vec::new();
        let mut chars = source.char_indices().peekable();
        while let Some((at, character)) = chars.next() {
            let end = at + character.len_utf8();
            let quote = frame.quote.map(|(quote, _)| quote);
            if quote.is_none() && character == '#' && frame.word.is_none() {
                let mut end = end;
                while let Some(&(next, character)) = chars.peek() {
                    if character == '\n' {
                        break;
                    }
                    chars.next();
                    end = next + character.len_utf8();
                }
                result.paint(at..end, Role::Comment);
                continue;
            }
            if character == '\\'
                && quote != Some('\'')
                && (quote.is_none()
                    || chars
                        .peek()
                        .is_none_or(|(_, next)| matches!(next, '$' | '"' | '\\' | '\n')))
            {
                frame.word.get_or_insert(at);
                if let Some((next, character)) = chars.next() {
                    result.paint(at..next + character.len_utf8(), Role::Escape);
                } else {
                    result.paint(at..end, Role::Escape);
                    result.literal = true;
                }
                continue;
            }
            if Some(character) == quote {
                result.paint(at..end, Role::Quote);
                result.pairs.push((frame.quote.unwrap().1, at));
                frame.quote = None;
                continue;
            }
            if quote.is_none() && matches!(character, '\'' | '"') {
                frame.word.get_or_insert(at);
                frame.quote = Some((character, at));
                result.paint(at..end, Role::Quote);
                continue;
            }
            if character == '$' && quote != Some('\'') {
                frame.word.get_or_insert(at);
                if chars.peek().is_some_and(|(_, next)| *next == '(') {
                    let (open, _) = chars.next().unwrap();
                    result.paint(at..open + 1, Role::Operator);
                    scopes.push((std::mem::take(&mut frame), open));
                    continue;
                }
                let mut end = end;
                if chars.peek().is_some_and(|(_, next)| *next == '{') {
                    chars.next();
                    let mut closed = false;
                    for (next, character) in chars.by_ref() {
                        end = next + character.len_utf8();
                        if character == '}' {
                            closed = true;
                            break;
                        }
                    }
                    result.literal |= !closed;
                } else {
                    while let Some(&(next, character)) = chars.peek() {
                        if !(character.is_ascii_alphanumeric() || character == '_') {
                            break;
                        }
                        chars.next();
                        end = next + character.len_utf8();
                    }
                    if end == at + 1
                        && chars.peek().is_some_and(|(_, next)| {
                            matches!(next, '?' | '@' | '*' | '#' | '$' | '!')
                        })
                    {
                        end = chars.next().unwrap().0 + 1;
                    }
                }
                result.paint(at..end, Role::Variable);
                continue;
            }
            if quote.is_some() {
                result.paint(at..end, Role::Quote);
                continue;
            }
            if character.is_whitespace() {
                result.finish_word(source, at, &mut frame);
                if character == '\n' {
                    frame.command = true;
                    frame.redirect = false;
                }
                continue;
            }
            if character == '(' {
                result.finish_word(source, at, &mut frame);
                result.paint(at..end, Role::Operator);
                scopes.push((std::mem::take(&mut frame), at));
                continue;
            }
            if character == ')' {
                result.finish_word(source, at, &mut frame);
                result.paint(at..end, Role::Operator);
                if let Some((outer, open)) = scopes.pop() {
                    frame = outer;
                    result.pairs.push((open, at));
                }
                continue;
            }
            if matches!(character, ';' | '|' | '&') {
                result.finish_word(source, at, &mut frame);
                let mut end = end;
                if chars.peek().is_some_and(|(_, next)| *next == character) {
                    end = chars.next().unwrap().0 + 1;
                }
                result.paint(at..end, Role::End);
                frame.command = true;
                frame.redirect = false;
                frame.continuation = character == '|' || character == '&' && end > at + 1;
                continue;
            }
            if matches!(character, '<' | '>') {
                if let Some(start) = frame.word
                    && source[start..at].chars().all(|c| c.is_ascii_digit())
                {
                    frame.word = None;
                    result.paint(start..at, Role::Redirection);
                }
                result.finish_word(source, at, &mut frame);
                let mut end = end;
                while let Some(&(next, character)) = chars.peek() {
                    if !matches!(character, '<' | '>' | '&' | '|' | '-') {
                        break;
                    }
                    chars.next();
                    end = next + 1;
                }
                result.paint(at..end, Role::Redirection);
                frame.redirect = true;
                continue;
            }
            frame.word.get_or_insert(at);
        }
        result.finish_word(source, source.len(), &mut frame);
        result.literal |= frame.quote.is_some();
        result.indent = frame.blocks.len()
            + scopes.len()
            + scopes
                .iter()
                .map(|(frame, _)| frame.blocks.len())
                .sum::<usize>()
            + usize::from(frame.continuation);
        result
    }

    fn paint(&mut self, range: Range<usize>, role: Role) {
        self.roles[range].fill(role);
    }

    fn finish_word(&mut self, source: &str, end: usize, frame: &mut Frame) {
        let Some(start) = frame.word.take() else {
            return;
        };
        let word = &source[start..end];
        let role = if frame.redirect {
            frame.redirect = false;
            Role::Param
        } else if frame.loop_header && word == "in" {
            Role::Keyword
        } else if frame.command {
            frame.continuation = false;
            match word {
                "if" => {
                    frame.blocks.push(Block::If);
                    Role::Keyword
                }
                "while" | "until" => {
                    frame.blocks.push(Block::Loop);
                    Role::Keyword
                }
                "for" | "select" => {
                    frame.blocks.push(Block::Loop);
                    frame.loop_header = true;
                    frame.command = false;
                    Role::Keyword
                }
                "then" | "do" | "else" | "elif" => {
                    frame.loop_header = false;
                    Role::Keyword
                }
                "fi" | "done" | "}" => {
                    let expected = match word {
                        "fi" => Block::If,
                        "done" => Block::Loop,
                        _ => Block::Group,
                    };
                    if frame.blocks.last() == Some(&expected) {
                        frame.blocks.pop();
                    }
                    frame.command = false;
                    frame.loop_header = false;
                    Role::Keyword
                }
                "{" => {
                    frame.blocks.push(Block::Group);
                    Role::Operator
                }
                "!" => Role::Operator,
                "command" | "builtin" | "exec" => Role::Command,
                "function" => Role::Keyword,
                _ if word
                    .split_once('=')
                    .is_some_and(|(name, _)| variable_name(name)) =>
                {
                    Role::Param
                }
                _ if source[end..].starts_with("()") => Role::Keyword,
                _ => {
                    frame.command = false;
                    self.commands.push(start..end);
                    Role::Command
                }
            }
        } else if word.starts_with('-') {
            Role::Option
        } else {
            Role::Param
        };
        // Quoting, escapes, variables, and nested command scopes already own
        // their colors. Only fill the otherwise unstyled parts of this word.
        for color in &mut self.roles[start..end] {
            if *color == Role::Normal {
                *color = role;
            }
        }
    }
}

fn variable_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Bash quote removal for literal command lookup, without any expansion.
pub(super) fn literal_word(raw: &str) -> Option<String> {
    let mut result = String::new();
    let mut quote = None;
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        if Some(character) == quote {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '\\' && quote != Some('\'') {
            let next = chars.next()?;
            if quote == Some('"') && !matches!(next, '$' | '"' | '\\' | '\n') {
                result.push('\\');
            }
            if next != '\n' {
                result.push(next);
            }
        } else if quote != Some('\'') && "$*?[]~".contains(character) {
            return None;
        } else {
            result.push(character);
        }
    }
    quote.is_none().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completeness_uses_bash_grammar_and_rejects_unsupported_submissions() {
        for source in [
            "if true; then",
            "for x in a; do",
            "echo '🐟",
            "echo $(echo hi",
            "echo hi |",
        ] {
            assert!(is_incomplete(source), "{source}");
        }
        for source in [
            "if true; then echo hi; fi",
            "echo `",
            "echo ${x:-fallback}",
            "echo hi; (false)",
        ] {
            assert!(!is_incomplete(source), "{source}");
        }
    }

    #[test]
    fn indentation_tracks_bash_blocks_without_counting_quoted_keywords() {
        for source in [
            "if true; then",
            "for x in a; do",
            "while true; do",
            "hello() {",
            "if true; then\n    echo 'fi'",
        ] {
            assert_eq!(newline(source), "\n    ", "{source}");
        }
        assert_eq!(newline("if true; then\n    for x in a; do"), "\n        ");
        assert_eq!(
            newline("if true; then\n    for x in a; do echo hi; done"),
            "\n    "
        );
        assert_eq!(newline("if true; then echo hi; fi"), "\n");
        assert_eq!(newline("if true; then\n    echo fi # done ) '"), "\n    ");
        assert_eq!(newline("echo $(if true; then"), "\n        ");
        assert_eq!(newline("echo \"$(if true; then"), "\n        ");
        assert_eq!(newline("if true; then\n    echo hi |"), "\n        ");
    }

    #[test]
    fn indentation_does_not_change_literal_or_escaped_contents() {
        for source in [
            "if true; then\n    echo '🐟",
            "echo \"$(echo hi)🐟",
            "if true; then\n    echo hi\\",
            "echo '${x",
            "echo ${x",
        ] {
            assert_eq!(newline(source), "\n", "{source}");
        }
        assert_eq!(newline("echo \"$(echo hi)\""), "\n");
        assert_eq!(newline("echo 'a\\'"), "\n");
        assert_eq!(newline("echo `"), "\n");
    }

    #[test]
    fn lexical_roles_preserve_bash_quotes_expansions_and_nested_commands() {
        let source = "if true; then echo \"🐟${value} $(cargo build)\"; fi # done";
        let view = View::new(source);
        for (word, role) in [
            ("if", Role::Keyword),
            ("true", Role::Command),
            ("then", Role::Keyword),
            ("🐟", Role::Quote),
            ("${value}", Role::Variable),
            ("cargo", Role::Command),
            ("build", Role::Param),
            ("fi #", Role::Keyword),
            ("# done", Role::Comment),
        ] {
            assert_eq!(view.roles[source.find(word).unwrap()], role, "{word}");
        }
        assert_eq!(literal_word("'a\\b'"), Some("a\\b".into()));
        assert_eq!(literal_word("\"a\\`b\""), Some("a\\`b".into()));
        assert_eq!(literal_word("literal`name"), Some("literal`name".into()));
        assert_eq!(literal_word("$(echo bad)"), None);
    }
}
