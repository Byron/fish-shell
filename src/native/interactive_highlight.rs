//! Syntax colors for the native editor, using the shared Fish grammar.
//!
//! The editor never expands variables, runs substitutions, or traverses globs
//! while painting input. The caller resolves literal commands against its
//! current builtin/function/PATH snapshot.

use super::interactive_colors::Palette;
use crate::ast::{self, Leaf as _};
use crate::parse_constants::{ParseTokenType, ParseTreeFlags};
use crate::tokenizer::{TokFlags, TokenType, Tokenizer};
use fish_widestring::{WString, wstr};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Role {
    #[default]
    Normal,
    Command,
    Keyword,
    Quote,
    Escape,
    Variable,
    Operator,
    Comment,
    Error,
    Param,
    Option,
    Redirection,
    End,
}

impl Role {
    pub(super) fn variable(self) -> &'static str {
        match self {
            Self::Normal => "fish_color_normal",
            Self::Command => "fish_color_command",
            Self::Keyword => "fish_color_keyword",
            Self::Quote => "fish_color_quote",
            Self::Escape => "fish_color_escape",
            Self::Variable | Self::Operator => "fish_color_operator",
            Self::Comment => "fish_color_comment",
            Self::Error => "fish_color_error",
            Self::Param => "fish_color_param",
            Self::Option => "fish_color_option",
            Self::Redirection => "fish_color_redirection",
            Self::End => "fish_color_end",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

struct Colors {
    roles: Vec<Role>,
    pairs: Vec<(usize, usize)>,
}

impl Colors {
    fn paint(&mut self, range: Range<usize>, role: Role) {
        let end = range.end.min(self.roles.len());
        if range.start < end {
            self.roles[range.start..end].fill(role);
        }
    }
}

/// Syntax roles for formatter output, with no command execution or PATH probes.
pub(super) fn highlight_roles(source: &wstr) -> Vec<Role> {
    let mut colors = Colors {
        roles: vec![Role::Normal; source.len()],
        pairs: vec![],
    };
    highlight_source(source, 0, &mut colors, &|_| true, 0);
    colors.roles
}

/// Return terminal styling without changing any of the input's characters.
/// `cursor` is a UTF-8 byte offset, as used by the native line editor.
#[cfg(test)]
pub(super) fn highlight(
    line: &str,
    cursor: usize,
    command_exists: impl Fn(&str) -> bool,
) -> String {
    highlight_with_palette(line, cursor, command_exists, &Palette::default())
}

pub(super) fn highlight_with_palette(
    line: &str,
    cursor: usize,
    command_exists: impl Fn(&str) -> bool,
    palette: &Palette,
) -> String {
    let source = WString::from(line);
    let mut colors = Colors {
        roles: vec![Role::Normal; source.len()],
        pairs: vec![],
    };
    highlight_source(&source, 0, &mut colors, &command_exists, 0);
    render(line, cursor, colors, palette)
}

pub(super) fn highlight_for_language(
    line: &str,
    cursor: usize,
    command_exists: impl Fn(&str) -> bool,
    palette: &Palette,
    language: super::InputLanguage,
) -> String {
    if language == super::InputLanguage::Fish {
        return highlight_with_palette(line, cursor, command_exists, palette);
    }
    let mut view = super::interactive_bash::View::new(line);
    for range in view.commands {
        if super::interactive_bash::literal_word(&line[range.clone()])
            .is_some_and(|name| !command_exists(&name))
        {
            view.roles[range].fill(Role::Error);
        }
    }
    let colors = Colors {
        roles: line.char_indices().map(|(at, _)| view.roles[at]).collect(),
        pairs: view
            .pairs
            .into_iter()
            .map(|(open, close)| (line[..open].chars().count(), line[..close].chars().count()))
            .collect(),
    };
    render(line, cursor, colors, palette)
}

fn render(line: &str, cursor: usize, colors: Colors, palette: &Palette) -> String {
    let cursor = line
        .char_indices()
        .take_while(|(offset, _)| *offset < cursor)
        .count();
    let active_pair = colors
        .pairs
        .iter()
        .copied()
        .find(|&(open, close)| cursor == open || cursor == close)
        .or_else(|| {
            colors
                .pairs
                .iter()
                .copied()
                .find(|&(_, close)| cursor == close + 1)
        });
    let mut result = String::with_capacity(line.len() + 32);
    result.push_str(&palette.sequence("fish_color_normal", false));
    let mut previous = (Role::Normal, false);
    for (index, (character, role)) in line.chars().zip(colors.roles).enumerate() {
        let emphasize = active_pair.is_some_and(|(a, b)| index == a || index == b);
        let current = (role, emphasize);
        if current != previous {
            // Reset attributes before every color change; a quote's underline
            // must not leak into the following argument or the next prompt.
            result.push_str(&palette.sequence(role.variable(), emphasize));
            previous = current;
        }
        result.push(character);
    }
    result.push_str("\x1b[0m");
    result
}

fn highlight_source(
    source: &wstr,
    offset: usize,
    colors: &mut Colors,
    command_exists: &impl Fn(&str) -> bool,
    depth: usize,
) {
    // A substitution used as a command is initially inside a command-colored
    // word; its arguments still need their own, ordinary argument color.
    colors.paint(offset..offset + source.len(), Role::Normal);
    // Typing unfinished source is normal. Shared parser flags preserve as much
    // structure as possible without painting unclosed blocks as errors.
    let tree = ast::parse(
        source,
        ParseTreeFlags {
            continue_after_error: true,
            include_comments: true,
            accept_incomplete_tokens: true,
            leave_unterminated: true,
            ..Default::default()
        },
        None,
    );
    let mut unknown_commands = vec![];
    for node in tree.walk() {
        match node.kind() {
            ast::Kind::Argument(argument) => {
                let range = ast::Node::source_range(argument).as_usize();
                let role = if source[range.clone()].to_string().starts_with('-') {
                    Role::Option
                } else {
                    Role::Param
                };
                colors.paint(offset + range.start..offset + range.end, role);
            }
            ast::Kind::DecoratedStatement(statement) => {
                if let Some(range) = statement.command.range() {
                    let range = range.as_usize();
                    let word = source[range.clone()].to_string();
                    colors.paint(offset + range.start..offset + range.end, Role::Command);
                    if literal_command(&word).is_some_and(|name| !command_exists(&name)) {
                        unknown_commands.push(range);
                    }
                }
            }
            ast::Kind::Keyword(keyword) => {
                if let Some(range) = keyword.range() {
                    let range = range.as_usize();
                    colors.paint(offset + range.start..offset + range.end, Role::Keyword);
                }
            }
            ast::Kind::Token(token) if token.token_type() != ParseTokenType::String => {
                if let Some(range) = token.range() {
                    let range = range.as_usize();
                    colors.paint(offset + range.start..offset + range.end, Role::Operator);
                }
            }
            ast::Kind::VariableAssignment(assignment) => {
                if let Some(range) = assignment.range() {
                    let range = range.as_usize();
                    if let Some(equals) = source[range.clone()].chars().position(|c| c == '=') {
                        colors.paint(
                            offset + range.start..offset + range.start + equals,
                            Role::Variable,
                        );
                    }
                }
            }
            _ => (),
        }
    }
    // Tokenization also covers incomplete words which the AST could not attach
    // to a statement. Token offsets, like AST ranges, count Unicode scalars.
    for token in Tokenizer::new(
        source,
        TokFlags {
            accept_unfinished: true,
            show_comments: true,
            continue_after_error: true,
            ..Default::default()
        },
    ) {
        let range = token.range();
        match token.type_ {
            TokenType::String => highlight_word(
                &source[range.clone()],
                offset + range.start,
                colors,
                command_exists,
                depth,
            ),
            TokenType::Comment => {
                colors.paint(offset + range.start..offset + range.end, Role::Comment);
            }
            TokenType::Error => {
                colors.paint(offset + range.start..offset + range.end, Role::Error);
            }
            TokenType::Redirect => {
                colors.paint(offset + range.start..offset + range.end, Role::Redirection);
            }
            TokenType::End => colors.paint(offset + range.start..offset + range.end, Role::End),
            _ => colors.paint(offset + range.start..offset + range.end, Role::Operator),
        }
    }
    for range in unknown_commands {
        colors.paint(offset + range.start..offset + range.end, Role::Error);
    }
    for range in &tree.extras.errors {
        let range = range.as_usize();
        colors.paint(offset + range.start..offset + range.end, Role::Error);
    }
}

/// The native unescaper is shared with execution, but it also implements glob
/// expansion. Exclude all expansion markers before calling it, even when they
/// appear quoted: a conservative unresolved color is preferable to filesystem
/// traversal or a guessed command while someone is typing.
fn literal_command(word: &str) -> Option<String> {
    if word.contains(['$', '(', ')', '*', '{', '}', '~']) {
        return None;
    }
    super::fish::unescape_literal(word).ok()
}

fn highlight_word(
    source: &wstr,
    offset: usize,
    colors: &mut Colors,
    command_exists: &impl Fn(&str) -> bool,
    depth: usize,
) {
    let chars = source.as_char_slice();
    let mut quote = Quote::None;
    let mut quote_start = 0;
    let mut brackets = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if quote != Quote::None {
            colors.roles[offset + index] = Role::Quote;
        }
        if character == '\\'
            && (quote == Quote::None
                || chars.get(index + 1).is_some_and(|c| match quote {
                    Quote::Single => matches!(c, '\\' | '\''),
                    Quote::Double => matches!(c, '\\' | '"' | '$' | '\n'),
                    Quote::None => true,
                }))
        {
            let end = escape_end(chars, index, quote);
            let escape: String = chars[index..end].iter().collect();
            let role = if end == index + 1 || super::fish::unescape_literal(&escape).is_err() {
                Role::Error
            } else {
                Role::Escape
            };
            colors.paint(offset + index..offset + end, role);
            index = end;
            continue;
        }
        if (character == '\'' && quote != Quote::Double)
            || (character == '"' && quote != Quote::Single)
        {
            colors.roles[offset + index] = Role::Quote;
            if quote == Quote::None {
                quote_start = index;
                quote = if character == '\'' {
                    Quote::Single
                } else {
                    Quote::Double
                };
            } else {
                colors.pairs.push((offset + quote_start, offset + index));
                quote = Quote::None;
            }
            index += 1;
            continue;
        }
        if quote == Quote::Single {
            index += 1;
            continue;
        }
        let substitution = if character == '$' && chars.get(index + 1) == Some(&'(') {
            colors.roles[offset + index] = Role::Operator;
            Some(index + 1)
        } else if character == '(' && quote == Quote::None {
            Some(index)
        } else {
            None
        };
        if let Some(open) = substitution {
            colors.roles[offset + open] = Role::Operator;
            let close = substitution_end(chars, open);
            let end = close.unwrap_or(chars.len());
            if depth < 32 {
                highlight_source(
                    &source[open + 1..end],
                    offset + open + 1,
                    colors,
                    command_exists,
                    depth + 1,
                );
            }
            if let Some(close) = close {
                colors.roles[offset + close] = Role::Operator;
                colors.pairs.push((offset + open, offset + close));
            }
            index = end + usize::from(close.is_some());
            continue;
        }
        if character == '$' {
            let start = index;
            while chars.get(index) == Some(&'$') {
                index += 1;
            }
            let name = index;
            while chars
                .get(index)
                .is_some_and(|c| crate::common::valid_var_name_char(*c))
            {
                index += 1;
            }
            colors.paint(
                offset + start..offset + index,
                if name == index {
                    Role::Error
                } else {
                    Role::Variable
                },
            );
            continue;
        }
        if quote == Quote::None {
            match character {
                '{' | '[' if colors.roles[offset + index] != Role::Command => {
                    brackets.push((character, index));
                    colors.roles[offset + index] = Role::Operator;
                }
                '}' | ']' => {
                    if let Some(&(open, at)) = brackets.last() {
                        if matches!((open, character), ('{', '}') | ('[', ']')) {
                            brackets.pop();
                            colors.roles[offset + index] = Role::Operator;
                            colors.pairs.push((offset + at, offset + index));
                        }
                    }
                }
                '*' => colors.roles[offset + index] = Role::Operator,
                '~' if index == 0 => colors.roles[offset + index] = Role::Operator,
                ',' if brackets.iter().any(|(open, _)| *open == '{') => {
                    colors.roles[offset + index] = Role::Operator;
                }
                _ => (),
            }
        }
        index += 1;
    }
}

fn escape_end(chars: &[char], start: usize, quote: Quote) -> usize {
    let Some(character) = chars.get(start + 1) else {
        return start + 1;
    };
    let mut end = start + 2;
    if quote != Quote::None {
        return end;
    }
    let (radix, max) = match character {
        'u' => (16, 4),
        'U' => (16, 8),
        'x' | 'X' => (16, 2),
        '0'..='7' => (8, 2),
        'c' => return (end + 1).min(chars.len()),
        _ => return end,
    };
    for _ in 0..max {
        if !chars.get(end).is_some_and(|c| c.is_digit(radix)) {
            break;
        }
        end += 1;
    }
    end
}

/// Find the close of a substitution without treating quotes, escapes, or
/// comments inside it as delimiters. Each nested command starts fresh quoting.
fn substitution_end(chars: &[char], open: usize) -> Option<usize> {
    let mut stack = vec![Quote::None];
    let mut index = open + 1;
    let mut token_start = true;
    while index < chars.len() {
        let quote = *stack.last().unwrap();
        let character = chars[index];
        if character == '\\'
            && (quote == Quote::None
                || chars.get(index + 1).is_some_and(|c| match quote {
                    Quote::Single => matches!(c, '\\' | '\''),
                    Quote::Double => matches!(c, '\\' | '"' | '$' | '\n'),
                    Quote::None => true,
                }))
        {
            index = (index + 2).min(chars.len());
            token_start = false;
            continue;
        }
        if character == '#' && quote == Quote::None && token_start {
            while chars.get(index).is_some_and(|c| *c != '\n') {
                index += 1;
            }
            continue;
        }
        if character == '\'' && quote != Quote::Double {
            *stack.last_mut().unwrap() = if quote == Quote::Single {
                Quote::None
            } else {
                Quote::Single
            };
        } else if character == '"' && quote != Quote::Single {
            *stack.last_mut().unwrap() = if quote == Quote::Double {
                Quote::None
            } else {
                Quote::Double
            };
        } else if character == '('
            && (quote == Quote::None
                || (quote == Quote::Double && chars.get(index.wrapping_sub(1)) == Some(&'$')))
        {
            stack.push(Quote::None);
            token_start = true;
            index += 1;
            continue;
        } else if character == ')' && quote == Quote::None {
            stack.pop();
            if stack.is_empty() {
                return Some(index);
            }
        }
        token_start =
            quote == Quote::None && (character.is_whitespace() || ";|&".contains(character));
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn colors(line: &str) -> Vec<Role> {
        let source = WString::from(line);
        let mut colors = Colors {
            roles: vec![Role::Normal; source.len()],
            pairs: vec![],
        };
        highlight_source(
            &source,
            0,
            &mut colors,
            &|name| matches!(name, "echo" | "cargo" | "cat" | "true" | "["),
            0,
        );
        colors.roles
    }

    fn assert_role(line: &str, fragment: &str, role: Role) {
        let start = line[..line.find(fragment).unwrap()].chars().count();
        assert_eq!(
            &colors(line)[start..start + fragment.chars().count()],
            vec![role; fragment.chars().count()],
            "{fragment:?} in {line:?}"
        );
    }

    fn without_ansi(rendered: &str) -> String {
        let mut plain = String::new();
        let mut escape = false;
        for character in rendered.chars() {
            if character == '\x1b' {
                escape = true;
            } else if escape && character == 'm' {
                escape = false;
            } else if !escape {
                plain.push(character);
            }
        }
        plain
    }

    #[test]
    fn command_positions_follow_fish_grammar() {
        let line = "if true; command cargo --version | cat; echo cargo; end # cargo";
        assert_role(line, "if", Role::Keyword);
        assert_role(line, "true", Role::Command);
        assert_role(line, "command", Role::Keyword);
        assert_role(line, "cargo", Role::Command);
        assert_role(line, "cat", Role::Command);
        assert_role(line, "end", Role::Keyword);
        assert_role(line, "# cargo", Role::Comment);
        assert_role("echo cargo", "cargo", Role::Param);
        assert_role("[ true ]", "[", Role::Command);
        assert_role("echo okay; missing-command", "missing-command", Role::Error);
        assert_role("'missing command' arg", "'missing command'", Role::Error);
    }

    #[test]
    fn quotes_variables_escapes_and_unicode_preserve_scalar_ranges() {
        let line = "echo 🐟 '$literal' \"hello $USER\" \\u03b1 $items[1]";
        assert_role(line, "'$literal'", Role::Quote);
        assert_role(line, "$USER", Role::Variable);
        assert_role(line, "\\u03b1", Role::Escape);
        assert_role(line, "$items", Role::Variable);
        assert_role(line, "[", Role::Operator);
        assert_role(line, "🐟", Role::Param);
        assert_role("echo $", "$", Role::Error);
        assert_role("echo \\uD800", "\\uD800", Role::Error);
        assert_role("echo # '$x'", "# '$x'", Role::Comment);
        assert_role("echo '# literal'", "'# literal'", Role::Quote);
    }

    #[test]
    fn substitutions_use_command_context_without_running_source() {
        let line = "echo \"$(echo (cargo --version))\" (missing-command)";
        assert_role(line, "cargo", Role::Command);
        assert_role(line, "--version", Role::Option);
        assert_role(line, "missing-command", Role::Error);
        assert_role("$(echo argument)", "argument", Role::Param);
        assert_role("echo (echo '# )'; cargo)", "cargo", Role::Command);
        assert_role("echo (echo # )\ncargo)", "cargo", Role::Command);
        let visited = RefCell::new(Vec::new());
        highlight("$dynamic; (echo source); *.exe", 0, |name| {
            visited.borrow_mut().push(name.to_owned());
            true
        });
        assert_eq!(&*visited.borrow(), &["echo"]);
    }

    #[test]
    fn incomplete_source_keeps_useful_colors() {
        assert_role("if true\n echo \"unfinished", "true", Role::Command);
        assert_role("if true\n echo \"unfinished", "\"unfinished", Role::Quote);
        assert_role("echo (cargo --", "cargo", Role::Command);
        assert_role("echo 0>| cat", "0>|", Role::Error);
    }

    #[test]
    fn ansi_output_preserves_text_and_marks_matching_delimiters() {
        let line = "echo 🐟 (cargo)";
        let rendered = highlight(line, line.len(), |_| true);
        assert!(rendered.contains(";1;4m("));
        assert!(rendered.contains(";1;4m)"));
        assert_eq!(without_ansi(&rendered), line);
        assert!(rendered.ends_with("\x1b[0m"));
        assert_eq!(
            Palette::default().highlight_hint(" future"),
            "\x1b[90m future\x1b[0m"
        );
    }

    #[test]
    fn every_typing_prefix_preserves_input_including_incomplete_syntax() {
        let line =
            "if true\n echo 🐟 '$literal' \"$(echo (cargo))\" \\u03b1 $items[1..-1] {a,b} # )\nend";
        for end in line
            .char_indices()
            .map(|(index, _)| index)
            .chain([line.len()])
        {
            let prefix = &line[..end];
            assert_eq!(without_ansi(&highlight(prefix, end, |_| true)), prefix);
        }
    }

    #[test]
    fn bash_highlighting_preserves_every_unicode_prefix_and_resolves_literal_commands() {
        let line = "if true; then echo 🐟 \"${item} $(cargo build)\"; fi # ' )";
        for end in line
            .char_indices()
            .map(|(index, _)| index)
            .chain([line.len()])
        {
            let prefix = &line[..end];
            let rendered = highlight_for_language(
                prefix,
                end,
                |_| true,
                &Palette::default(),
                super::super::InputLanguage::Bash,
            );
            assert_eq!(without_ansi(&rendered), prefix);
        }
        let visited = RefCell::new(Vec::new());
        let rendered = highlight_for_language(
            "echo $(missing-command) `literal",
            0,
            |name| {
                visited.borrow_mut().push(name.to_owned());
                name == "echo"
            },
            &Palette::default(),
            super::super::InputLanguage::Bash,
        );
        assert_eq!(&*visited.borrow(), &["echo", "missing-command"]);
        assert!(rendered.contains(&format!(
            "{}missing-command",
            Palette::default().sequence("fish_color_error", false)
        )));
        assert_eq!(without_ansi(&rendered), "echo $(missing-command) `literal");
    }
}
