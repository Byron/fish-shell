//! Native abbreviation registration using Fish's shared matcher and cursor markers.
use crate::abbrs::{Abbreviation, AbbreviationSet, Position, Replacement};
use crate::ast::{self, Leaf as _};
use crate::parse_constants::{ParseTreeFlags, SourceRange};
use fish_widestring::WString;
use pcre2::utf32::RegexBuilder;
use rustyline::EditMode;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Definition {
    name: String,
    replacement: String,
    function: bool,
    regex: Option<String>,
    anywhere: bool,
    commands: Vec<String>,
    marker: Option<String>,
    description: Option<String>,
}

impl Definition {
    fn compile(&self) -> Result<Abbreviation, String> {
        let mut abbreviation = Abbreviation::new(
            self.name.as_str().into(),
            self.regex.as_deref().unwrap_or(&self.name).into(),
            self.replacement.as_str().into(),
            if self.anywhere {
                Position::Anywhere
            } else {
                Position::Command
            },
            false,
        );
        if let Some(pattern) = &self.regex {
            let mut builder = RegexBuilder::new();
            builder.block_utf_pattern_directive(true);
            let compile = |pattern: &str| {
                builder
                    .build(WString::from(pattern).as_char_slice())
                    .map_err(|error| {
                        format!(
                            "abbr: invalid regular expression: {}",
                            error.error_message()
                        )
                    })
            };
            compile(pattern)?;
            abbreviation.regex = Some(Box::new(compile(&format!("^(?:{pattern})$"))?));
        }
        abbreviation.replacement_is_function = self.function;
        abbreviation.commands = self.commands.iter().map(|s| s.as_str().into()).collect();
        abbreviation.set_cursor_marker = self.marker.as_deref().map(Into::into);
        abbreviation.description = self.description.as_deref().map(Into::into);
        Ok(abbreviation)
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Abbreviations {
    entries: Vec<Definition>,
    #[serde(skip)]
    compiled: OnceLock<Arc<AbbreviationSet>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Expansion {
    pub range: Range<usize>,
    pub text: String,
    pub cursor: Option<usize>,
    pub function: bool,
    pub marker: Option<String>,
    pub token: String,
}

impl Expansion {
    pub(super) fn finish(mut self, text: String) -> Self {
        let replacement = Replacement::new(
            SourceRange {
                start: 0,
                length: self.token.chars().count() as u32,
            },
            text.as_str().into(),
            self.marker.as_deref().map(Into::into),
        );
        self.text = replacement.text.to_string();
        self.cursor = replacement.cursor.map(|cursor| {
            self.range.start
                + self
                    .text
                    .char_indices()
                    .nth(cursor)
                    .map_or(self.text.len(), |(offset, _)| offset)
        });
        self.function = false;
        self
    }
}

impl Abbreviations {
    pub fn run_builtin(&mut self, args: &[String]) -> Result<(i32, String), String> {
        let mut operation = "add";
        let mut explicit_operation = false;
        let mut definition = Definition {
            name: String::new(),
            replacement: String::new(),
            function: false,
            regex: None,
            anywhere: false,
            commands: Vec::new(),
            marker: None,
            description: None,
        };
        let mut words = Vec::new();
        let mut index = 0;
        while let Some(option) = args.get(index) {
            index += 1;
            let (flag, attached) = option
                .split_once('=')
                .map_or((option.as_str(), None), |(flag, value)| (flag, Some(value)));
            if attached.is_some()
                && matches!(
                    flag,
                    "--add" | "--erase" | "--query" | "--show" | "--list" | "--rename" | "--global"
                )
            {
                return Err(format!("abbr: {flag} does not take an argument"));
            }
            let mut argument = || -> Result<String, String> {
                if let Some(value) = attached {
                    return Ok(value.into());
                }
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("abbr: {flag} requires an argument"))?
                    .clone();
                index += 1;
                Ok(value)
            };
            let selected = match flag {
                "-a" | "--add" => Some("add"),
                "-e" | "--erase" => Some("erase"),
                "-q" | "--query" => Some("query"),
                "-s" | "--show" => Some("show"),
                "-l" | "--list" => Some("list"),
                "--rename" => Some("rename"),
                "-p" | "--position" => {
                    definition.anywhere = match argument()?.as_str() {
                        "command" => false,
                        "anywhere" => true,
                        _ => return Err("abbr: position must be command or anywhere".into()),
                    };
                    None
                }
                "-r" | "--regex" => {
                    definition.regex = Some(argument()?);
                    None
                }
                "-f" | "--function" => {
                    definition.replacement = argument()?;
                    definition.function = true;
                    None
                }
                "-c" | "--command" => {
                    definition.commands.push(argument()?);
                    definition.anywhere = true;
                    None
                }
                "--description" => {
                    definition.description = Some(argument()?);
                    None
                }
                "--set-cursor" => {
                    definition.marker = Some(attached.unwrap_or("%").into());
                    None
                }
                "-g" | "--global" => None,
                "--" => {
                    words.extend_from_slice(&args[index..]);
                    break;
                }
                _ if option.starts_with('-') => {
                    return Err(format!("abbr: unsupported option {option}"));
                }
                _ => {
                    words.push(option.clone());
                    if words.len() >= 2 && operation == "add" && !definition.function {
                        words.extend_from_slice(&args[index..]);
                        break;
                    }
                    None
                }
            };
            if let Some(selected) = selected {
                if explicit_operation && operation != selected {
                    return Err("abbr: operations cannot be combined".into());
                }
                operation = selected;
                explicit_operation = true;
            }
        }
        if words.is_empty() && !explicit_operation {
            operation = "show";
        }
        match operation {
            "add" => {
                if words.is_empty()
                    || (!definition.function && words.len() < 2)
                    || (definition.function && words.len() != 1)
                {
                    return Err("abbr: --add requires a name and an expansion or --function".into());
                }
                definition.name = words.remove(0);
                if !literal_name(&definition.name) {
                    return Err("abbr: name must be one unquoted word".into());
                }
                if !definition.function {
                    definition.replacement = words.join(" ");
                }
                if definition.replacement.contains('\0') || definition.marker.as_deref() == Some("")
                {
                    return Err("abbr: invalid expansion or empty cursor marker".into());
                }
                definition.compile()?;
                self.entries.retain(|entry| {
                    entry.name != definition.name || entry.commands != definition.commands
                });
                self.entries.push(definition);
                self.compiled.take();
                Ok((0, String::new()))
            }
            "erase" => {
                let mut missing = words.is_empty();
                for word in words {
                    let before = self.entries.len();
                    self.entries.retain(|entry| {
                        entry.name != word || entry.commands != definition.commands
                    });
                    missing |= before == self.entries.len();
                }
                self.compiled.take();
                Ok((i32::from(missing), String::new()))
            }
            "query" => Ok((
                i32::from(
                    !words
                        .iter()
                        .any(|word| self.entries.iter().any(|entry| &entry.name == word)),
                ),
                String::new(),
            )),
            "rename" => {
                if words.len() != 2 {
                    return Err("abbr: --rename requires two names".into());
                }
                if !literal_name(&words[1]) {
                    return Err("abbr: new name must be one unquoted word".into());
                }
                if self.entries.iter().any(|entry| entry.name == words[1]) {
                    return Err("abbr: new name already exists".into());
                }
                let Some(entry) = self
                    .entries
                    .iter_mut()
                    .find(|entry| entry.name == words[0] && entry.commands == definition.commands)
                else {
                    return Ok((1, String::new()));
                };
                entry.name.clone_from(&words[1]);
                self.compiled.take();
                Ok((0, String::new()))
            }
            "show" => {
                let mut result = String::new();
                for entry in &self.entries {
                    if !words.is_empty() && !words.contains(&entry.name) {
                        continue;
                    }
                    result.push_str("abbr --add");
                    if entry.anywhere {
                        result.push_str(" --position anywhere");
                    }
                    for (flag, value) in [
                        ("--regex", entry.regex.as_ref()),
                        ("--function", entry.function.then_some(&entry.replacement)),
                        ("--description", entry.description.as_ref()),
                    ] {
                        if let Some(value) = value {
                            write!(result, " {flag} {}", quote(value)).unwrap();
                        }
                    }
                    if let Some(marker) = &entry.marker {
                        write!(result, " --set-cursor={}", quote(marker)).unwrap();
                    }
                    for command in &entry.commands {
                        write!(result, " --command {}", quote(command)).unwrap();
                    }
                    write!(result, " -- {}", quote(&entry.name)).unwrap();
                    if !entry.function {
                        write!(result, " {}", quote(&entry.replacement)).unwrap();
                    }
                    result.push('\n');
                }
                Ok((0, result))
            }
            "list" => {
                if !words.is_empty() {
                    return Err("abbr: --list does not accept names".into());
                }
                let mut names = std::collections::BTreeSet::new();
                Ok((
                    0,
                    self.entries
                        .iter()
                        .filter(|entry| names.insert(&entry.name))
                        .map(|entry| format!("{}\n", entry.name))
                        .collect(),
                ))
            }
            _ => unreachable!(),
        }
    }

    fn compiled(&self) -> &AbbreviationSet {
        self.compiled.get_or_init(|| {
            let mut set = AbbreviationSet::default();
            for definition in &self.entries {
                if let Ok(abbreviation) = definition.compile() {
                    set.add(abbreviation);
                }
            }
            Arc::new(set)
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.compiled().has_match(
            &WString::from(name),
            Position::Command,
            &WString::from(name),
        )
    }

    #[cfg(test)]
    pub fn expansion(&self, line: &str, cursor: usize) -> Option<Expansion> {
        self.candidates(line, cursor).into_iter().next()
    }

    pub(super) fn candidates(&self, line: &str, cursor: usize) -> Vec<Expansion> {
        if self.entries.is_empty() || cursor > line.len() || !line.is_char_boundary(cursor) {
            return Vec::new();
        }
        let full_source = WString::from(line);
        let cursor_chars = line[..cursor].chars().count();
        let extent = crate::commandline_extent::get_cmdsubst_extent(&full_source, cursor_chars);
        let source = &full_source[extent.clone()];
        let tree = ast::parse(
            source,
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
            let Some(command_range) = statement.command.range() else {
                continue;
            };
            let command = &source[command_range.as_usize()];
            let mut ranges = vec![(command_range.as_usize(), Position::Command)];
            for argument in &statement.args_or_redirs {
                if let ast::ArgumentOrRedirection::Argument(argument) = argument
                    && let Some(range) = argument.range()
                {
                    ranges.push((range.as_usize(), Position::Anywhere));
                }
            }
            for (range, position) in ranges {
                if range.end + extent.start != cursor_chars {
                    continue;
                }
                let token = &source[range.clone()];
                // Match bare source only; escaped or quoted tokens remain literal.
                if !literal_name(&token.to_string()) {
                    return Vec::new();
                }
                let start = line
                    .char_indices()
                    .nth(range.start + extent.start)
                    .map_or(line.len(), |(offset, _)| offset);
                return self
                    .compiled()
                    .r#match(token, position, command)
                    .into_iter()
                    .map(|replacer| {
                        let text = replacer.replacement.to_string();
                        let expansion = Expansion {
                            range: start..cursor,
                            text: text.clone(),
                            cursor: None,
                            function: replacer.is_function,
                            marker: replacer.set_cursor_marker.map(|marker| marker.to_string()),
                            token: token.to_string(),
                        };
                        if expansion.function {
                            expansion
                        } else {
                            expansion.finish(text)
                        }
                    })
                    .collect();
            }
        }
        Vec::new()
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
        abbreviations
            .run_builtin(&words(&["--add", "x", "--function", "f"]))
            .unwrap();
        abbreviations
            .run_builtin(&words(&["--function", "f", "x"]))
            .unwrap();
        abbreviations
            .run_builtin(&words(&["--position", "anywhere", "x", "echo"]))
            .unwrap();
    }

    #[test]
    fn regex_positions_commands_and_nested_substitutions_share_matching_rules() {
        let mut abbreviations = abbreviations();
        abbreviations
            .run_builtin(&words(&[
                "--add",
                "--regex",
                "item[0-9]+",
                "--command",
                "echo",
                "items",
                "selected",
            ]))
            .unwrap();
        for line in ["echo item42", "begin; echo item42", "echo (echo item42"] {
            let expansion = abbreviations.expansion(line, line.len()).unwrap();
            assert_eq!(expansion.text, "selected");
            assert_eq!(&line[expansion.range], "item42");
        }
        for line in [
            "item42",
            "printf item42",
            "echo xitem42",
            "echo 'item42'",
            "echo >item42",
        ] {
            assert!(
                abbreviations.expansion(line, line.len()).is_none(),
                "{line}"
            );
        }
        assert_eq!(
            abbreviations.expansion("echo (gco", 9).unwrap().text,
            "git checkout"
        );
        assert!(
            abbreviations
                .run_builtin(&words(&["--regex", "[", "invalid", "bad"]))
                .is_err()
        );
    }

    #[test]
    fn cursor_markers_and_newest_matching_definitions_survive_serialization() {
        let mut abbreviations = abbreviations();
        abbreviations
            .run_builtin(&words(&[
                "--add",
                "--set-cursor=@",
                "--regex",
                "g.*",
                "general",
                "echo 🐟@tail",
            ]))
            .unwrap();
        let expansion = abbreviations.expansion("gco", 3).unwrap();
        assert_eq!(expansion.text, "echo 🐟tail");
        assert_eq!(expansion.cursor, Some("echo 🐟".len()));
        let restored: Abbreviations =
            crate::native::worker::decode(&crate::native::worker::encode(&abbreviations).unwrap())
                .unwrap();
        assert_eq!(restored.expansion("gco", 3), Some(expansion));
        abbreviations
            .run_builtin(&words(&["--add", "gco", "newer"]))
            .unwrap();
        assert_eq!(abbreviations.expansion("gco", 3).unwrap().text, "newer");
        assert_eq!(restored.expansion("gco", 3).unwrap().text, "echo 🐟tail");
    }

    #[test]
    fn erase_and_rename_select_command_restrictions_and_reject_empty_names() {
        let mut abbreviations = Abbreviations::default();
        abbreviations
            .run_builtin(&words(&["--add", "x", "global"]))
            .unwrap();
        abbreviations
            .run_builtin(&words(&["--add", "--command", "echo", "x", "specific"]))
            .unwrap();
        abbreviations
            .run_builtin(&words(&["--erase", "x"]))
            .unwrap();
        assert!(abbreviations.expansion("x", 1).is_none());
        assert_eq!(
            abbreviations.expansion("echo x", 6).unwrap().text,
            "specific"
        );
        assert!(
            abbreviations
                .run_builtin(&words(&["--rename", "x", ""]))
                .is_err()
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["--rename", "x", "y"]))
                .unwrap()
                .0,
            1
        );
        assert_eq!(
            abbreviations
                .run_builtin(&words(&["--rename", "--command", "echo", "x", "y"]))
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            abbreviations.expansion("echo y", 6).unwrap().text,
            "specific"
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
