//! Bash lexical syntax for the reader; submission selects Fish lowering or Rash evaluation.

use crate::highlight::{HighlightRole, HighlightSpec};
use crate::parse_constants::ParseIssue;
use crate::prelude::*;
use std::ops::Range;

pub struct Syntax {
    text: String,
    tree: tree_sitter::Tree,
    positions: Vec<usize>,
    fish: bool,
}

/// Hide literal backticks from Bash parsers without changing any source offsets.
pub(crate) fn literal_backticks(source: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if source.contains(&b'`') {
        std::borrow::Cow::Owned(
            source
                .iter()
                .map(|&b| if b == b'`' { b'^' } else { b })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(source)
    }
}

impl Syntax {
    pub fn new(source: &wstr) -> Self {
        Self::for_input(source, false)
    }

    pub fn for_input(source: &wstr, fish: bool) -> Self {
        let text = source.to_string();
        let mut positions = vec![0; text.len() + 1];
        for (i, (byte, c)) in text.char_indices().enumerate() {
            positions[byte..byte + c.len_utf8()].fill(i);
            positions[byte + c.len_utf8()] = i + 1;
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .unwrap();
        let input = if fish {
            literal_backticks(text.as_bytes())
        } else {
            std::borrow::Cow::Borrowed(text.as_bytes())
        };
        let tree = parser.parse(input.as_ref(), None).unwrap();
        Self {
            text,
            tree,
            positions,
            fish,
        }
    }

    fn range(&self, node: tree_sitter::Node<'_>) -> Range<usize> {
        self.positions[node.start_byte()]..self.positions[node.end_byte()]
    }

    fn walk(&self, mut f: impl FnMut(tree_sitter::Node<'_>)) {
        let mut cursor = self.tree.walk();
        loop {
            f(cursor.node());
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
            }
        }
    }

    pub fn highlight(&self) -> Vec<HighlightSpec> {
        use HighlightRole::*;
        let mut colors = vec![HighlightSpec::with_fg(Normal); *self.positions.last().unwrap()];
        self.walk(|node| {
            let role = match node.kind() {
                "command_name" => Command,
                "comment" => Comment,
                "string" | "raw_string" | "ansi_c_string" | "heredoc_body" => Quote,
                "variable_name" | "expansion" | "simple_expansion" | "${" | "$" => Operat,
                "if" | "then" | "else" | "elif" | "fi" | "while" | "until" | "do" | "done"
                | "for" | "in" | "case" | "esac" | "select" | "function" => Keyword,
                ";" | "&" | "|" | "&&" | "||" | "|&" => StatementTerminator,
                ">" | ">>" | "<" | "<<" | "<<<" | "<&" | ">&" | "<<-" => Redirection,
                "word" if node.parent().is_some_and(|p| p.kind() == "command_name") => Command,
                "word" if self.text[node.byte_range()].starts_with('-') => Option,
                "word" => Param,
                "ERROR" => Error,
                _ => return,
            };
            colors[self.range(node)].fill(HighlightSpec::with_fg(role));
        });
        colors
    }

    pub fn highlight_paths(
        &self,
        source: &wstr,
        colors: &mut [HighlightSpec],
        state: &crate::bash_session::State,
        vars: &dyn crate::env::Environment,
    ) {
        self.walk(|node| {
            if node.kind() != "command_name" && node.kind() != "word" && node.kind() != "raw_string"
            {
                return;
            }
            if node.parent().is_some_and(|p| p.kind() == "command_name") {
                return;
            }
            let range = self.range(node);
            let raw = &source[range.clone()];
            // Only inspect literal words: highlighting must never trigger an expansion.
            if raw
                .chars()
                .any(|c| "$*?[]~".contains(c) || c == '`' && !self.fish)
            {
                return;
            }
            let Some(word) = unquote_for_input(raw, self.fish) else {
                return;
            };
            if node.kind() == "command_name" {
                if !state.builtins.contains_key(&word)
                    && !state.aliases.contains_key(&word)
                    && !state.functions.contains(&word)
                    && crate::path::path_get_path(&word, vars).is_none()
                {
                    colors[range].fill(HighlightSpec::with_fg(HighlightRole::Error));
                }
            } else if !word.is_empty() && crate::path::path_is_valid(&word, &vars.get_pwd_slash()) {
                for color in &mut colors[range] {
                    color.valid_path = true;
                }
            }
        });
    }

    pub fn tokens(&self) -> Vec<Range<usize>> {
        let mut ranges = vec![];
        self.walk(|node| {
            if matches!(
                node.kind(),
                "word"
                    | "string"
                    | "raw_string"
                    | "ansi_c_string"
                    | "concatenation"
                    | "simple_expansion"
                    | "expansion"
                    | "command_substitution"
            ) {
                let range = self.range(node);
                if !ranges
                    .iter()
                    .any(|r: &Range<usize>| r.start <= range.start && r.end >= range.end)
                {
                    ranges.push(range);
                }
            }
        });
        ranges.sort_by_key(|r| r.start);
        ranges
    }

    pub fn token_extent(&self, pos: usize) -> (Range<usize>, Range<usize>) {
        let mut previous = pos..pos;
        for range in self.tokens() {
            if range.start <= pos && pos <= range.end {
                return (range, previous);
            }
            if range.end < pos {
                previous = range;
            }
        }
        (pos..pos, previous)
    }

    pub fn process_extent(&self, pos: usize, job: bool) -> Range<usize> {
        let mut best = 0..*self.positions.last().unwrap();
        self.walk(|node| {
            if node.kind() != "command" && !(job && node.kind() == "pipeline") {
                return;
            }
            if job && node.parent().is_some_and(|p| p.kind() == "pipeline") {
                return;
            }
            let mut range = self.range(node);
            if range.end < pos {
                let end = self
                    .text
                    .char_indices()
                    .nth(pos)
                    .map_or(self.text.len(), |(b, _)| b);
                if !self.text[node.end_byte()..end].trim().is_empty() {
                    return;
                }
                range.end = pos;
            }
            if range.contains(&pos) || range.end == pos {
                best = range;
            }
        });
        best
    }

    pub fn indents(&self) -> Vec<i32> {
        let mut indents = vec![0; *self.positions.last().unwrap()];
        let mut heredocs = vec![];
        self.walk(|node| {
            if node.kind() == "heredoc_body" {
                heredocs.push(self.range(node));
            }
            if !matches!(
                node.kind(),
                "if_statement"
                    | "for_statement"
                    | "while_statement"
                    | "case_statement"
                    | "compound_statement"
                    | "subshell"
                    | "ERROR"
            ) {
                return;
            }
            let range = node.byte_range();
            let Some(newline) = self.text[range.clone()].find('\n') else {
                return;
            };
            let start = self.positions[range.start + newline];
            let mut end = self.positions[range.end];
            if let Some(last) = node.child(node.child_count().saturating_sub(1))
                && matches!(last.kind(), "fi" | "done" | "esac" | "}" | ")")
            {
                let line_start = self.text[..last.start_byte()]
                    .rfind('\n')
                    .map_or(0, |i| i + 1);
                end = self.positions[line_start];
            }
            for indent in &mut indents[start..end.max(start)] {
                *indent += 1;
            }
        });
        for range in heredocs {
            indents[range].fill(0);
        }
        indents
    }

    pub fn ends_in_comment(&self) -> bool {
        let mut result = false;
        self.walk(|node| {
            if node.kind() == "comment" && node.end_byte() == self.text.len() {
                result = true;
            }
        });
        result
    }

    pub fn valid(&self) -> bool {
        !self.tree.root_node().has_error()
    }
}

/// Quote removal only; expansion spelling remains spelling, never evaluated values.
pub fn unquote(text: &wstr) -> Option<WString> {
    unquote_for_input(text, false)
}

pub fn unquote_for_input(text: &wstr, fish: bool) -> Option<WString> {
    if text.find(L!("$'")).is_some() || text.find(L!("$\"")).is_some() {
        return None;
    }
    let mut quote = None;
    let mut result = WString::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '\\' && quote != Some('\'') {
            let next = chars.next()?;
            if quote == Some('"')
                && (!matches!(next, '$' | '`' | '"' | '\\' | '\n') || fish && next == '`')
            {
                result.push('\\');
            }
            if next != '\n' {
                result.push(next);
            }
        } else {
            result.push(c);
        }
    }
    Some(result)
}

/// Check the Fish dialect or the retained Rash session without evaluating input.
pub fn check(source: &wstr, parser: &mut crate::parser::Parser) -> Result<(), ParseIssue> {
    if !parser.libdata().rash_shell {
        return match crate::bash_to_fish::translate(source) {
            Ok(_) => Ok(()),
            Err(error) if error.incomplete => ParseIssue::INCOMPLETE,
            Err(error) => {
                eprintf!("\nfish: Bash-style input: %s\n", error.message);
                ParseIssue::ERROR
            }
        };
    }
    let Some(session) = parser.libdata().bash_session.clone() else {
        return ParseIssue::ERROR;
    };
    let Ok(mut session) = session.try_borrow_mut() else {
        return ParseIssue::ERROR;
    };
    let result = session.check_syntax(source);
    let (status, error) = match result {
        Ok(result) => result,
        Err(error) => {
            eprintf!("fish: Bash syntax check failed: %s\n", error.to_string());
            if error.kind() != std::io::ErrorKind::InvalidInput {
                session.failed = true;
                parser.set_last_statuses(crate::env::Statuses::just(125));
                parser.libdata_mut().exit_current_script = true;
            }
            return ParseIssue::ERROR;
        }
    };
    let error = error.to_string();
    if error.contains("unexpected EOF")
        || error.contains("unexpected end of file")
        || error.contains("here-document") && !Syntax::new(source).valid()
    {
        ParseIssue::INCOMPLETE
    } else if status != 0 {
        eprintf!("\n%s", error);
        ParseIssue::ERROR
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_backticks_are_literal_for_the_editor() {
        for source in [L!("echo `"), L!("echo \"ü`\" tail"), L!("echo $(echo `)")] {
            let syntax = Syntax::for_input(source, true);
            assert!(syntax.valid(), "{source}");
            assert_eq!(syntax.highlight().len(), source.len());
            assert!(
                syntax
                    .highlight()
                    .iter()
                    .all(|color| color.foreground != HighlightRole::Error)
            );
        }
        let text = L!("echo `word next");
        assert_eq!(
            Syntax::for_input(text, true).tokens(),
            vec![0..4, 5..10, 11..15]
        );
        assert!(!Syntax::new(text).valid());
        assert_eq!(
            unquote_for_input(L!("\"\\`\""), true),
            Some(L!("\\`").to_owned())
        );
        assert_eq!(unquote(L!("\"\\`\"")), Some(L!("`").to_owned()));
    }

    #[test]
    fn bash_reader_syntax() {
        let syntax = Syntax::new(L!("for i in 1 2; do\n  echo \"ü${i}\"\ndone"));
        assert!(syntax.valid());
        assert_eq!(syntax.highlight().len(), 36);
        assert_eq!(syntax.highlight()[0].foreground, HighlightRole::Keyword);
        assert!(syntax.indents().contains(&1));
        let text = L!("git -C \"ü repo\" switch topic");
        let syntax = Syntax::new(text);
        assert_eq!(&text[syntax.token_extent(11).0], L!("\"ü repo\""));
        assert!(!Syntax::new(L!("for x in a; do\n")).valid());
    }
}
