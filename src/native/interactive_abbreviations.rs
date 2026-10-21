//! Literal, command-position abbreviations for the native Windows reader.
//!
//! Like Fish abbreviations these are editing operations, never aliases in the
//! evaluator. Quoting a word suppresses expansion, and scripts remain unchanged.

use crate::ast::{self, Leaf as _};
use crate::parse_constants::ParseTreeFlags;
use fish_widestring::WString;
use rustyline::EditMode;
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Abbreviations {
    entries: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Expansion {
    /// Byte offsets, matching the native editor rather than the shared AST.
    pub range: Range<usize>,
    pub text: String,
}

impl Abbreviations {
    /// The native subset deliberately excludes regex and function expansions:
    /// generating an edit never evaluates code or launches another process.
    pub fn run_builtin(&mut self, args: &[String]) -> Result<(i32, String), String> {
        #[derive(Clone, Copy, PartialEq)]
        enum Operation {
            Add,
            Erase,
            Query,
            Show,
            List,
        }
        let mut operation = None;
        let mut position = false;
        let mut words = Vec::new();
        let mut index = 0;
        while let Some(option) = args.get(index) {
            let selected = match option.as_str() {
                "-a" | "--add" => Some(Operation::Add),
                "-e" | "--erase" => Some(Operation::Erase),
                "-q" | "--query" => Some(Operation::Query),
                "-s" | "--show" => Some(Operation::Show),
                "-l" | "--list" => Some(Operation::List),
                "-p" | "--position" => {
                    position = true;
                    index += 1;
                    if args.get(index).map(String::as_str) != Some("command") {
                        return Err("abbr: native abbreviations support --position command".into());
                    }
                    None
                }
                "--position=command" => {
                    position = true;
                    None
                }
                "-g" | "--global" => None,
                "--" => {
                    index += 1;
                    words.extend_from_slice(&args[index..]);
                    break;
                }
                _ if option.starts_with('-') => {
                    return Err(format!(
                        "abbr: unsupported option {option}; native abbreviations support literal command words"
                    ));
                }
                _ => {
                    words.push(option.clone());
                    if words.len() >= 2
                        && operation.is_none_or(|operation| operation == Operation::Add)
                    {
                        words.extend_from_slice(&args[index + 1..]);
                        break;
                    }
                    None
                }
            };
            if let Some(selected) = selected {
                if operation.is_some_and(|previous| previous != selected) {
                    return Err("abbr: operations cannot be combined".into());
                }
                operation = Some(selected);
            }
            index += 1;
        }
        let operation = operation.unwrap_or(if words.is_empty() {
            Operation::Show
        } else {
            Operation::Add
        });
        if position && operation != Operation::Add {
            return Err("abbr: --position requires --add".into());
        }
        match operation {
            Operation::Add => {
                if words.len() < 2 {
                    return Err("abbr: --add requires a name and an expansion".into());
                }
                let name = &words[0];
                if !literal_name(name) {
                    return Err("abbr: name must be one unquoted command word".into());
                }
                let replacement = words[1..].join(" ");
                if replacement.contains('\0') {
                    return Err("abbr: expansion contains a null character".into());
                }
                self.entries.insert(name.clone(), replacement);
                Ok((0, String::new()))
            }
            Operation::Erase => {
                if words.is_empty() {
                    return Ok((1, String::new()));
                }
                let mut missing = false;
                for word in &words {
                    missing |= self.entries.remove(word).is_none();
                }
                Ok((i32::from(missing), String::new()))
            }
            Operation::Query => Ok((
                i32::from(!words.iter().any(|word| self.entries.contains_key(word))),
                String::new(),
            )),
            Operation::Show => {
                let mut result = String::new();
                for (name, replacement) in &self.entries {
                    if words.is_empty() || words.contains(name) {
                        result.push_str(&format!(
                            "abbr --add -- {} {}\n",
                            quote(name),
                            quote(replacement)
                        ));
                    }
                }
                Ok((0, result))
            }
            Operation::List => {
                if !words.is_empty() {
                    return Err("abbr: --list does not accept names".into());
                }
                Ok((
                    0,
                    self.entries
                        .keys()
                        .map(|name| format!("{name}\n"))
                        .collect(),
                ))
            }
        }
    }

    /// Abbreviations are also valid command-colored words before expansion.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Expand only a complete bare command token ending at the cursor. Parsing
    /// the full buffer avoids turning the prefix of a longer word into a match
    /// while the cursor is in the middle of an edit.
    pub fn expansion(&self, line: &str, cursor: usize) -> Option<Expansion> {
        if self.entries.is_empty() || cursor > line.len() || !line.is_char_boundary(cursor) {
            return None;
        }
        let source = WString::from(line);
        let cursor_chars = line[..cursor].chars().count();
        let tree = ast::parse(
            &source,
            ParseTreeFlags {
                continue_after_error: true,
                accept_incomplete_tokens: true,
                leave_unterminated: true,
                ..Default::default()
            },
            None,
        );
        for node in tree.walk() {
            let ast::Kind::DecoratedStatement(statement) = node.kind() else {
                continue;
            };
            let Some(range) = statement.command.range() else {
                continue;
            };
            let range = range.as_usize();
            if range.end != cursor_chars {
                continue;
            }
            let word = source[range.clone()].to_string();
            // Matching raw source is intentional: 'g', \g, and $g do not expand.
            let replacement = self.entries.get(&word)?;
            if replacement == &word {
                return None;
            }
            let start = line
                .char_indices()
                .nth(range.start)
                .map(|(offset, _)| offset)?;
            return Some(Expansion {
                range: start..cursor,
                text: replacement.clone(),
            });
        }
        None
    }
}

fn literal_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "\\'\"$(){}[]*?~;|&<>#".contains(c))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Fish's mode-selection variable can be set in config.fish or at any prompt.
/// The evaluator's fish_vi_key_bindings/fish_default_key_bindings commands set
/// the same variable; the next reader refresh applies the new keymap.
pub(super) fn edit_mode(variables: &BTreeMap<String, Vec<String>>) -> EditMode {
    if variables
        .get("fish_key_bindings")
        .and_then(|values| values.first())
        .is_some_and(|value| value == "fish_vi_key_bindings")
    {
        EditMode::Vi
    } else {
        EditMode::Emacs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|word| (*word).into()).collect()
    }

    fn abbreviations() -> Abbreviations {
        let mut abbreviations = Abbreviations::default();
        abbreviations
            .run_builtin(&words(&["--add", "gco", "git checkout"]))
            .unwrap();
        abbreviations
            .run_builtin(&words(&["--add", "🐟", "echo fish"]))
            .unwrap();
        abbreviations
    }

    #[test]
    fn command_position_in_blocks_and_pipelines_expands_without_touching_arguments() {
        let abbreviations = abbreviations();
        for line in [
            "gco",
            "if gco",
            "not gco",
            "begin\n    gco",
            "echo yes | gco",
            "echo yes; gco",
            "echo yes && gco",
            "echo yes\ngco",
        ] {
            let expansion = abbreviations.expansion(line, line.len()).unwrap();
            assert_eq!(&line[expansion.range], "gco", "{line}");
            assert_eq!(expansion.text, "git checkout", "{line}");
        }
        for line in [
            "echo gco",
            "echo >gco",
            "# gco",
            "'gco'",
            "\"gco\"",
            "\\gco",
            "$gco",
        ] {
            assert!(
                abbreviations.expansion(line, line.len()).is_none(),
                "{line}"
            );
        }
    }

    #[test]
    fn expansion_uses_unicode_byte_ranges_and_preserves_existing_suffix() {
        let abbreviations = abbreviations();
        let line = "echo 🐟; 🐟 existing";
        let cursor = line.find(" existing").unwrap();
        let expansion = abbreviations.expansion(line, cursor).unwrap();
        assert_eq!(&line[expansion.range.clone()], "🐟");
        let mut result = line.to_owned();
        result.replace_range(expansion.range, &expansion.text);
        assert_eq!(result, "echo 🐟; echo fish existing");
        assert!(abbreviations.expansion("gcoother", 3).is_none());
        assert!(abbreviations.expansion("🐟", 1).is_none());
    }

    #[test]
    fn registrations_replace_query_erase_and_quote_their_source() {
        let mut abbreviations = abbreviations();
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["-q", "gco", "🐟"]))
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["-q", "gco", "missing"]))
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["-q", "missing"]))
                .unwrap()
                .0,
            1
        );
        abbreviations
            .run_builtin(&words(&["--add", "gco", "echo 'x' \\path"]))
            .unwrap();
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["--show", "gco"]))
                .unwrap()
                .1,
            "abbr --add -- 'gco' 'echo \\'x\\' \\\\path'\n"
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["--erase", "gco"]))
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["--erase", "gco"]))
                .unwrap()
                .0,
            1
        );
        assert!(!abbreviations.contains("gco"));
        assert!(
            abbreviations
                .run_builtin(&words(&["--add", "two words", "echo"]))
                .is_err()
        );
        assert!(
            abbreviations
                .run_builtin(&words(&["--add", "x", "--function", "f"]))
                .is_err()
        );
        assert!(
            abbreviations
                .run_builtin(&words(&["--function", "f", "x"]))
                .is_err()
        );
        assert!(
            abbreviations
                .run_builtin(&words(&["--position", "anywhere", "x", "echo"]))
                .is_err()
        );
    }

    #[test]
    fn selecting_vi_and_default_keymaps_uses_fish_configuration() {
        let mut variables = BTreeMap::new();
        assert_eq!(edit_mode(&variables), EditMode::Emacs);
        variables.insert(
            "fish_key_bindings".into(),
            vec!["fish_vi_key_bindings".into()],
        );
        assert_eq!(edit_mode(&variables), EditMode::Vi);
        variables.insert(
            "fish_key_bindings".into(),
            vec!["fish_default_key_bindings".into()],
        );
        assert_eq!(edit_mode(&variables), EditMode::Emacs);
    }
}
