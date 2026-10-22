//! Native, side-effect-free command-line completion.
//!
//! Keep terminal handling out of this module. Completion consumes a snapshot of
//! the evaluator's functions, variables and declarative registrations; it never
//! evaluates the command line or launches a program to produce suggestions.

use rustyline::completion::Pair;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The literal subset of Fish's `complete` registrations supported natively.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct CompletionSpec {
    pub command: String,
    pub short_option: Option<char>,
    pub long_option: Option<String>,
    pub old_option: Option<String>,
    pub arguments: Vec<String>,
    pub description: String,
    pub no_files: bool,
    pub requires_argument: bool,
}

pub(super) struct CompletionEngine {
    fish: bool,
    builtins: BTreeSet<String>,
    commands: BTreeSet<String>,
    functions: BTreeSet<String>,
    executables: BTreeSet<String>,
    variables: Vec<String>,
    registrations: Vec<CompletionSpec>,
}

impl CompletionEngine {
    pub(super) fn new(language: &str) -> Self {
        let builtins = if language == "rash" {
            rash::session::BUILTINS
                .iter()
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect()
        } else {
            [
                ":",
                ".",
                "[",
                "echo",
                "set",
                "cd",
                "pwd",
                "count",
                "test",
                "true",
                "false",
                "source",
                "eval",
                "exit",
                "return",
                "break",
                "continue",
                "printf",
                "math",
                "string",
                "contains",
                "read",
                "status",
                "type",
                "command",
                "builtin",
                "functions",
                "complete",
                "set_color",
                "abbr",
                "bind",
                "fish_vi_key_bindings",
                "fish_default_key_bindings",
                "switch",
                "case",
                "jobs",
                "bg",
                "fg",
                "wait",
                "kill",
                "disown",
                "if",
                "else",
                "end",
                "for",
                "while",
                "function",
                "begin",
                "not",
                "and",
                "or",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        };
        let mut engine = Self {
            fish: language == "fish",
            builtins,
            commands: BTreeSet::new(),
            functions: BTreeSet::new(),
            executables: BTreeSet::new(),
            variables: Vec::new(),
            registrations: Vec::new(),
        };
        engine.set_context(
            Vec::new(),
            std::env::var_os("PATH"),
            std::env::vars_os()
                .filter_map(|(name, _)| name.into_string().ok())
                .collect(),
        );
        engine
    }

    /// Refresh once before each prompt, not on every keystroke. This also picks
    /// up executables installed or removed while the previous command ran.
    pub(super) fn set_context(
        &mut self,
        commands: Vec<String>,
        path: Option<OsString>,
        variables: Vec<String>,
    ) {
        self.functions = commands.into_iter().collect();
        self.commands = self.functions.clone();
        self.executables.clear();
        self.variables = variables;
        let directories = std::env::current_dir().ok().into_iter().chain(
            path.as_ref()
                .into_iter()
                .flat_map(|path| std::env::split_paths(path)),
        );
        for directory in directories {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if executable_name(name) && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    self.commands.insert(command_name(name).into());
                    self.executables.insert(name.into());
                }
            }
        }
    }

    pub(super) fn set_registrations(&mut self, registrations: Vec<CompletionSpec>) {
        self.registrations = registrations;
    }

    /// Share command discovery with syntax highlighting. Explicit paths use
    /// Windows' executable extensions, without searching or executing source.
    pub(super) fn command_exists(&self, name: &str, cwd: &Path) -> bool {
        if self.builtins.contains(name) || self.functions.contains(name) {
            return true;
        }
        if self.executables.iter().any(|entry| {
            entry.eq_ignore_ascii_case(name) || command_name(entry).eq_ignore_ascii_case(name)
        }) {
            return true;
        }
        if name.contains(['/', '\\']) {
            return executable_path(&cwd.join(name));
        }
        false
    }

    pub(super) fn complete(&self, line: &str, pos: usize, cwd: &Path) -> (usize, Vec<Pair>) {
        if pos > line.len() || !line.is_char_boundary(pos) {
            return (pos.min(line.len()), Vec::new());
        }
        let word = completion_word(line, pos, self.fish);
        if word.comment {
            return (pos, Vec::new());
        }
        if let Some(start) = word.variable {
            let mut matches = Candidates::new(&line[start..pos]);
            for variable in &self.variables {
                if valid_variable(variable) {
                    matches.add(variable, variable, "Variable", false);
                }
            }
            let mut result = matches.finish(false, false);
            let suffix: String = line[pos..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            preserve_suffix(&mut result, &suffix);
            return (start, result);
        }
        // Partial substitutions and literal dollar signs are not paths. Never
        // evaluate them, and do not accidentally replace them with a literal.
        if word.expansion || word.value.contains(['`', '\n']) {
            return (word.start, Vec::new());
        }

        let command = word.command();
        let prefix = &word.value;
        let mut matches = Candidates::new(prefix);
        let mut files = true;
        if !word.redirect {
            if let Some(command) = command {
                if self.fish {
                    files = self.contextual(command, &word, &mut matches);
                }
            } else if !prefix.contains(['/', '\\']) {
                for name in &self.builtins {
                    matches.add(name, name, "Builtin", true);
                }
                if !word.words.iter().any(|word| word == "builtin") {
                    for name in &self.commands {
                        matches.add(name, name, "Command", true);
                    }
                    // Hide .exe normally, but honor a suffix typed explicitly.
                    if prefix
                        .rsplit_once('.')
                        .is_some_and(|(_, suffix)| "exe".starts_with(&suffix.to_ascii_lowercase()))
                    {
                        for name in &self.executables {
                            matches.add(name, name, "Command", true);
                        }
                    }
                }
            }
        }

        if files {
            self.complete_paths(&word, cwd, &mut matches);
        }
        // A following token or closing quote already provides separation. For
        // an interior cursor, only offer candidates that preserve its suffix.
        let tail = token_tail(line, pos, word.quote);
        let append_space = pos == line.len();
        let mut result = matches.finish(true, append_space);
        preserve_suffix(&mut result, &tail);
        (word.start, result)
    }

    fn contextual(&self, command: &str, word: &Word, matches: &mut Candidates) -> bool {
        let command = command_name(command.rsplit(['/', '\\']).next().unwrap_or(command));
        let args = word.arguments();
        let positional = args.iter().filter(|arg| !arg.starts_with('-')).count();
        let options = word.value.starts_with('-') && !args.iter().any(|arg| arg == "--");
        let mut files = true;

        if options {
            if let Some(entries) = option_choices(command) {
                add_choices(matches, entries);
            }
        } else if positional == 0 {
            if let Some(entries) = subcommand_choices(command) {
                add_choices(matches, entries);
                files = false;
            }
        }
        if command == "set"
            && !options
            && (positional == 0
                || args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-q" | "--query" | "-e" | "--erase")))
        {
            for name in &self.variables {
                matches.add(name, name, "Variable", true);
            }
            files = false;
        }
        if matches!(command, "type" | "command" | "builtin" | "functions") && !options {
            for name in &self.builtins {
                if command != "functions" {
                    matches.add(name, name, "Builtin", true);
                }
            }
            if command != "builtin" {
                let names = if command == "functions" {
                    &self.functions
                } else {
                    &self.commands
                };
                for name in names {
                    matches.add(name, name, "Command", true);
                }
            }
            files = false;
        }
        // Cargo's flags depend on the subcommand; these are common to the
        // build/check/run/test family and should not appear as filenames.
        if command == "cargo" && options && positional > 0 {
            add_choices(matches, CARGO_BUILD_OPTIONS);
        }
        for spec in self
            .registrations
            .iter()
            .filter(|spec| spec.command == command)
        {
            files &= !spec.no_files;
            let short = spec.short_option.map(|option| format!("-{option}"));
            let long = spec
                .long_option
                .as_ref()
                .map(|option| format!("--{option}"));
            let old = spec.old_option.as_ref().map(|option| format!("-{option}"));
            let flags = [short.as_deref(), long.as_deref(), old.as_deref()];
            let has_option = flags.iter().any(Option::is_some);
            if options {
                for flag in flags.iter().flatten() {
                    matches.add(flag, flag, &spec.description, true);
                }
            }
            let follows_option = args
                .last()
                .is_some_and(|arg| flags.iter().flatten().any(|flag| arg == flag));
            let attached = word
                .value
                .split_once('=')
                .filter(|(name, _)| flags.iter().flatten().any(|flag| name == flag));
            if !has_option || follows_option || attached.is_some() {
                for argument in &spec.arguments {
                    let (value, description) = argument
                        .split_once('\t')
                        .unwrap_or((argument, &spec.description));
                    let value = attached
                        .map_or_else(|| value.to_owned(), |(flag, _)| format!("{flag}={value}"));
                    matches.add(&value, &value, description, true);
                }
                if follows_option && spec.requires_argument {
                    files = !spec.no_files;
                }
            }
        }
        files
    }

    fn complete_paths(&self, word: &Word, cwd: &Path, matches: &mut Candidates) {
        let value = word.value.replace('\\', "/");
        if matches!(value.as_str(), "." | "..") {
            let directory = format!("{value}/");
            matches.add(&directory, &directory, "Directory", false);
        }
        if value == "~" && home_dir().is_some_and(|home| home.is_dir()) {
            matches.add("~/", "~/", "Home directory", false);
        }
        let split = value.rfind('/').map_or(0, |at| at + 1);
        let (directory, prefix) = value.split_at(split);
        let lookup = if let Some(rest) = directory.strip_prefix("~/") {
            home_dir().map(|home| home.join(rest))
        } else {
            Some(cwd.join(directory))
        };
        let Some(lookup) = lookup else { return };
        let Ok(entries) = std::fs::read_dir(lookup) else {
            return;
        };
        let command = word.command();
        let directories_only = !word.redirect && command == Some("cd");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') && !prefix.starts_with('.') {
                continue;
            }
            // Follow directory symlinks/junctions, which are navigable too.
            let is_directory = entry.path().is_dir();
            if directories_only && !is_directory {
                continue;
            }
            if command.is_none() && !word.redirect && !is_directory && !executable_name(name) {
                continue;
            }
            let name = if command.is_none()
                && directory.is_empty()
                && !is_directory
                && !prefix
                    .rsplit_once('.')
                    .is_some_and(|(_, suffix)| "exe".starts_with(&suffix.to_ascii_lowercase()))
            {
                command_name(name)
            } else {
                name
            };
            let value = format!("{directory}{name}{}", if is_directory { "/" } else { "" });
            // Match the basename rather than characters in the already typed
            // directory. Preserve ~/ instead of replacing it with a long path.
            if let Some(rank) = match_rank(name, prefix) {
                matches.add_ranked(
                    &value,
                    &value,
                    if is_directory { "Directory" } else { "" },
                    !is_directory,
                    rank,
                );
            }
        }
    }
}

fn preserve_suffix(candidates: &mut Vec<Pair>, suffix: &str) {
    if !suffix.is_empty() {
        candidates.retain_mut(|candidate| {
            if let Some(prefix) = candidate.replacement.strip_suffix(suffix) {
                candidate.replacement = prefix.to_owned();
                true
            } else {
                false
            }
        });
    }
}

fn executable_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            ["exe", "com", "bat", "cmd"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn executable_path(path: &Path) -> bool {
    if path.is_file() && path.to_str().is_some_and(executable_name) {
        return true;
    }
    ["exe", "com", "bat", "cmd"].iter().any(|extension| {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(format!(".{extension}"));
        Path::new(&candidate).is_file()
    })
}

fn command_name(name: &str) -> &str {
    if name
        .get(name.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".exe"))
    {
        &name[..name.len() - 4]
    } else {
        name
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn valid_variable(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

type Rank = (u8, usize);

fn match_rank(name: &str, prefix: &str) -> Option<Rank> {
    if name.starts_with(prefix) {
        return Some((0, 0));
    }
    let name = name.to_lowercase();
    let prefix = prefix.to_lowercase();
    if name.starts_with(&prefix) {
        return Some((1, 0));
    }
    if let Some(at) = name.find(&prefix) {
        return Some((2, at));
    }
    let mut chars = name.chars().enumerate();
    let mut distance = 0;
    for needle in prefix.chars() {
        let (at, _) = chars.find(|(_, character)| *character == needle)?;
        distance = at;
    }
    Some((3, distance))
}

struct Candidate {
    value: String,
    display: String,
    description: String,
    space: bool,
    rank: Rank,
}

struct Candidates {
    prefix: String,
    entries: BTreeMap<String, Candidate>,
}

impl Candidates {
    fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.into(),
            entries: BTreeMap::new(),
        }
    }

    fn add(&mut self, value: &str, display: &str, description: &str, space: bool) {
        if let Some(rank) = match_rank(value, &self.prefix) {
            self.add_ranked(value, display, description, space, rank);
        }
    }

    fn add_ranked(
        &mut self,
        value: &str,
        display: &str,
        description: &str,
        space: bool,
        rank: Rank,
    ) {
        self.entries
            .entry(value.to_owned())
            .or_insert_with(|| Candidate {
                value: value.into(),
                display: display.into(),
                description: description.into(),
                space,
                rank,
            });
    }

    fn finish(self, quote: bool, append_space: bool) -> Vec<Pair> {
        let best = self
            .entries
            .values()
            .map(|entry| entry.rank.0)
            .min()
            .unwrap_or(0);
        let mut entries: Vec<_> = self
            .entries
            .into_values()
            .filter(|entry| entry.rank.0 <= best.max(1))
            .collect();
        entries.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.value.cmp(&b.value)));
        entries
            .into_iter()
            .map(|entry| Pair {
                display: if entry.description.is_empty() {
                    entry.display
                } else {
                    format!("{}  ({})", entry.display, entry.description)
                },
                replacement: format!(
                    "{}{}",
                    if quote {
                        quote_completion(&entry.value)
                    } else {
                        entry.value
                    },
                    if append_space && entry.space { " " } else { "" }
                ),
            })
            .collect()
    }
}

fn quote_completion(value: &str) -> String {
    // Escaping individual special characters keeps directory completion open:
    // "my\\ directory/" can immediately be continued by the next component.
    let mut result = String::new();
    for (index, character) in value.chars().enumerate() {
        if !(character.is_alphanumeric()
            || "-._/:".contains(character)
            || character == '=' && value.starts_with('-')
            || character == '~' && index == 0 && value.starts_with("~/"))
        {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

#[derive(Default)]
struct Word {
    start: usize,
    value: String,
    words: Vec<String>,
    quote: Option<char>,
    variable: Option<usize>,
    expansion: bool,
    redirect: bool,
    comment: bool,
}

impl Word {
    fn command_index(&self) -> Option<usize> {
        self.words.iter().position(|word| {
            !matches!(
                word.as_str(),
                "command" | "builtin" | "exec" | "not" | "and" | "or" | "if" | "while" | "else"
            ) && !word
                .split_once('=')
                .is_some_and(|(name, _)| valid_variable(name))
        })
    }

    fn command(&self) -> Option<&str> {
        self.command_index().map(|index| self.words[index].as_str())
    }

    fn arguments(&self) -> &[String] {
        self.command_index()
            .map_or(&[], |index| &self.words[index + 1..])
    }
}

fn completion_word(line: &str, pos: usize, fish: bool) -> Word {
    let mut word = Word {
        start: pos,
        ..Word::default()
    };
    let mut active = false;
    let mut substitutions = Vec::new();
    let mut chars = line[..pos].char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        if word.comment {
            if character == '\n' {
                word = Word {
                    start: pos,
                    ..Word::default()
                };
                active = false;
            }
            continue;
        }
        if !active {
            word.start = at;
        }
        if character == '\\' && word.quote != Some('\'') {
            active = true;
            word.variable = None;
            if let Some((_, next)) = chars.next() {
                if word.quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                    word.value.push('\\');
                }
                word.value.push(next);
            }
        } else if character == '\\'
            && fish
            && word.quote == Some('\'')
            && chars
                .peek()
                .is_some_and(|(_, next)| matches!(next, '\\' | '\''))
        {
            active = true;
            word.value.push(chars.next().unwrap().1);
        } else if Some(character) == word.quote {
            word.quote = None;
            word.variable = None;
        } else if word.quote.is_none() && matches!(character, '\'' | '"') {
            active = true;
            word.quote = Some(character);
        } else if word.quote.is_none() && character == '#' && !active {
            word.comment = true;
        } else if word.quote.is_none() && character == '(' {
            substitutions.push(word);
            word = Word {
                start: pos,
                ..Word::default()
            };
            active = false;
        } else if word.quote.is_none() && character == ')' {
            word = substitutions.pop().unwrap_or_default();
            word.expansion = true;
            active = true;
        } else if word.quote.is_none() && matches!(character, ';' | '|' | '&' | '\n') {
            word = Word {
                start: pos,
                ..Word::default()
            };
            active = false;
        } else if word.quote.is_none() && matches!(character, '<' | '>') {
            if active && !word.value.chars().all(|c| c.is_ascii_digit()) {
                word.words.push(std::mem::take(&mut word.value));
            }
            word.value.clear();
            word.redirect = true;
            word.variable = None;
            word.expansion = false;
            active = false;
        } else if word.quote.is_none() && character.is_whitespace() {
            if active {
                if word.redirect {
                    word.redirect = false;
                } else {
                    word.words.push(std::mem::take(&mut word.value));
                }
            }
            word.value.clear();
            word.variable = None;
            word.expansion = false;
            active = false;
        } else {
            active = true;
            if character == '$' && word.quote != Some('\'') {
                word.variable = Some(at + 1);
                word.expansion = true;
            } else if !(character.is_alphanumeric() || character == '_') {
                word.variable = None;
            }
            word.value.push(character);
        }
    }
    if !active {
        word.start = pos;
    }
    word
}

fn token_tail(line: &str, pos: usize, mut quote: Option<char>) -> String {
    let mut end = pos;
    let mut escaped = false;
    for (offset, character) in line[pos..].char_indices() {
        if !escaped
            && quote.is_none()
            && (character.is_whitespace() || ";|&()< >".contains(character))
        {
            break;
        }
        if !escaped && character == '\\' {
            escaped = true;
        } else {
            if !escaped && Some(character) == quote {
                quote = None;
            } else if !escaped && quote.is_none() && matches!(character, '\'' | '"') {
                quote = Some(character);
            }
            escaped = false;
        }
        end = pos + offset + character.len_utf8();
    }
    line[pos..end].to_owned()
}

type Choices = &'static [(&'static str, &'static str)];

fn add_choices(matches: &mut Candidates, choices: Choices) {
    for &(value, description) in choices {
        matches.add(value, value, description, true);
    }
}

fn subcommand_choices(command: &str) -> Option<Choices> {
    Some(match command {
        "cargo" => &[
            ("build", "Compile the package"),
            ("check", "Check without building"),
            ("test", "Run tests"),
            ("run", "Run a binary"),
            ("clippy", "Check lints"),
            ("fmt", "Format Rust source"),
            ("doc", "Build documentation"),
            ("add", "Add a dependency"),
            ("remove", "Remove a dependency"),
            ("update", "Update dependencies"),
            ("clean", "Remove build artifacts"),
            ("install", "Install a binary"),
            ("uninstall", "Remove an installed binary"),
            ("new", "Create a package"),
            ("init", "Initialize a package"),
            ("tree", "Show dependency tree"),
            ("help", "Show help"),
        ],
        "git" => &[
            ("status", "Show working tree status"),
            ("diff", "Show changes"),
            ("add", "Stage changes"),
            ("commit", "Record changes"),
            ("log", "Show commit history"),
            ("show", "Show objects"),
            ("branch", "List or manage branches"),
            ("switch", "Switch branches"),
            ("checkout", "Switch branches or restore files"),
            ("restore", "Restore files"),
            ("fetch", "Download remote history"),
            ("pull", "Fetch and integrate"),
            ("push", "Update remote refs"),
            ("merge", "Merge histories"),
            ("rebase", "Reapply commits"),
            ("stash", "Save local changes"),
            ("clone", "Clone a repository"),
            ("init", "Create a repository"),
            ("remote", "Manage remote repositories"),
            ("worktree", "Manage working trees"),
            ("help", "Show help"),
        ],
        "string" => &[
            ("escape", "Escape special characters"),
            ("join", "Join strings"),
            ("length", "Print string lengths"),
            ("lower", "Convert to lowercase"),
            ("match", "Match patterns"),
            ("repeat", "Repeat strings"),
            ("replace", "Replace text"),
            ("split", "Split strings"),
            ("sub", "Extract a substring"),
            ("trim", "Trim whitespace"),
            ("unescape", "Unescape strings"),
            ("upper", "Convert to uppercase"),
        ],
        "status" => &[
            ("is-interactive", "Test interactive session"),
            ("is-login", "Test login shell"),
            ("current-command", "Print current command"),
            ("is-block", "Test block scope"),
            ("is-command-substitution", "Test captured output"),
            ("fish-path", "Print path to fish"),
        ],
        _ => return None,
    })
}

fn option_choices(command: &str) -> Option<Choices> {
    Some(match command {
        "set" => &[
            ("--local", "Set a local variable"),
            ("--global", "Set a global variable"),
            ("--export", "Export to child processes"),
            ("--unexport", "Do not export"),
            ("--erase", "Erase a variable"),
            ("--query", "Test whether variables exist"),
            ("--append", "Append values"),
            ("--prepend", "Prepend values"),
        ],
        "cargo" => &[
            ("--help", "Show help"),
            ("--version", "Show version"),
            ("--verbose", "Verbose output"),
            ("--quiet", "Reduce output"),
            ("--offline", "Use cached dependencies"),
            ("--locked", "Keep lockfile unchanged"),
        ],
        "git" => &[
            ("--help", "Show help"),
            ("--version", "Show version"),
            ("--no-pager", "Disable pager"),
        ],
        "jobs" => &[
            ("-p", "Print process IDs"),
            ("-l", "Include process IDs"),
            ("-r", "Show running jobs"),
            ("-s", "Show stopped jobs"),
        ],
        "type" => &[
            ("--all", "Show all matches"),
            ("--query", "Only set exit status"),
            ("--path", "Print command paths"),
        ],
        "functions" => &[
            ("--query", "Test function existence"),
            ("--erase", "Erase functions"),
        ],
        _ => return None,
    })
}

const CARGO_BUILD_OPTIONS: Choices = &[
    ("--release", "Build optimized artifacts"),
    ("--all-targets", "Build every target"),
    ("--all-features", "Enable all features"),
    ("--no-default-features", "Disable default features"),
    ("--features", "Select features"),
    ("--target", "Build for a target"),
    ("--target-dir", "Set artifact directory"),
    ("--package", "Select a package"),
    ("--workspace", "Select all workspace packages"),
    ("--manifest-path", "Path to Cargo.toml"),
    ("--lib", "Build the library"),
    ("--bin", "Select a binary"),
    ("--bins", "Build all binaries"),
];

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "fish-completion-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn engine() -> CompletionEngine {
        let mut engine = CompletionEngine::new("fish");
        engine.set_context(
            Vec::new(),
            None,
            vec!["HOME".into(), "PATH".into(), "fish_private".into()],
        );
        engine
    }

    fn replacements(engine: &CompletionEngine, line: &str, cwd: &Path) -> Vec<String> {
        engine
            .complete(line, line.len(), cwd)
            .1
            .into_iter()
            .map(|pair| pair.replacement)
            .collect()
    }

    #[test]
    fn cargo_and_git_offer_commands_and_contextual_flags_instead_of_project_files() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("Cargo.toml"), "").unwrap();
        let engine = engine();
        let choices = replacements(&engine, "cargo ", &directory.0);
        assert!(choices.contains(&"build ".into()));
        assert!(!choices.iter().any(|value| value.contains("Cargo.toml")));
        assert_eq!(replacements(&engine, "cargo bu", &directory.0), ["build "]);
        assert_eq!(replacements(&engine, "git sw", &directory.0), ["switch "]);
        assert_eq!(
            replacements(&engine, "cargo build --rel", &directory.0),
            ["--release "]
        );
    }

    #[test]
    fn command_inventory_elides_exe_and_refreshes_after_path_change() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("native-probe.exe"), "").unwrap();
        std::fs::write(directory.0.join("README.md"), "").unwrap();
        let mut engine = engine();
        engine.set_context(
            vec!["native_function".into()],
            Some(directory.0.clone().into_os_string()),
            Vec::new(),
        );
        let absent = directory.0.join("absent");
        assert_eq!(
            replacements(&engine, "native-p", &absent),
            ["native-probe "]
        );
        assert_eq!(
            replacements(&engine, "native-probe.e", &absent),
            ["native-probe.exe "]
        );
        assert!(engine.command_exists("NATIVE-PROBE", &absent));
        assert!(engine.command_exists("native-probe.exe", &absent));
        assert!(engine.command_exists("native_function", &absent));
        assert!(!engine.command_exists("README.md", &directory.0));
        assert_eq!(
            replacements(&engine, "native-p", &directory.0),
            ["native-probe "]
        );
        engine.set_context(Vec::new(), None, Vec::new());
        assert!(replacements(&engine, "native-p", &absent).is_empty());
    }

    #[test]
    fn directory_completion_continues_without_a_space_and_cd_filters_files() {
        let directory = Directory::new();
        std::fs::create_dir(directory.0.join("source tree")).unwrap();
        std::fs::write(directory.0.join("source.txt"), "").unwrap();
        let engine = engine();
        assert_eq!(
            replacements(&engine, "cd so", &directory.0),
            ["source\\ tree/"]
        );
        assert_eq!(replacements(&engine, "cd ..", &directory.0), ["../"]);
        assert_eq!(
            replacements(&engine, "echo so", &directory.0),
            ["source\\ tree/", "source.txt "]
        );
        // A redirection overrides cd's directory-only argument completion.
        assert_eq!(
            replacements(&engine, "cd . > source.t", &directory.0),
            ["source.txt "]
        );
    }

    #[test]
    fn quoted_unicode_completion_is_a_literal_fish_argument() {
        let directory = Directory::new();
        let filename = "space 🐟's (report) $final.txt";
        std::fs::write(directory.0.join(filename), "").unwrap();
        let engine = engine();
        let line = "echo 'spa";
        let (start, candidates) = engine.complete(line, line.len(), &directory.0);
        assert_eq!(start, 5);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            crate::native::fish::unescape_literal(candidates[0].replacement.trim_end()).unwrap(),
            filename
        );
    }

    #[test]
    fn variables_complete_in_scope_without_turning_expansions_into_literals() {
        let engine = engine();
        let directory = Directory::new();
        let line = "echo \"$fish_p";
        let (start, candidates) = engine.complete(line, line.len(), &directory.0);
        assert_eq!(start, "echo \"$".len());
        assert_eq!(candidates[0].replacement, "fish_private");
        assert!(replacements(&engine, "echo $HOME/", &directory.0).is_empty());
        assert!(replacements(&engine, "echo '$HO", &directory.0).is_empty());
        assert!(replacements(&engine, "echo \\$HO", &directory.0).is_empty());
        assert_eq!(
            replacements(&engine, "set fish_p", &directory.0),
            ["fish_private "]
        );
        for (line, pos) in [("echo \"$HO\"", 9), ("echo \"$HME\"", 8)] {
            let (start, candidates) = engine.complete(line, pos, &directory.0);
            assert_eq!(candidates.len(), 1);
            assert_eq!(
                format!(
                    "{}{}{}",
                    &line[..start],
                    candidates[0].replacement,
                    &line[pos..]
                ),
                "echo \"$HOME\""
            );
        }
    }

    #[test]
    fn match_quality_prefers_prefixes_then_substrings_then_subsequences() {
        let directory = Directory::new();
        for file in ["README.md", "roadmap.txt", "my-readme.txt", "released.md"] {
            std::fs::write(directory.0.join(file), "").unwrap();
        }
        let engine = engine();
        assert_eq!(
            replacements(&engine, "echo read", &directory.0),
            ["README.md "]
        );
        assert_eq!(
            replacements(&engine, "echo map", &directory.0),
            ["roadmap.txt "]
        );
        assert_eq!(
            replacements(&engine, "echo rdm", &directory.0),
            [
                "README.md ",
                "roadmap.txt ",
                "my-readme.txt ",
                "released.md "
            ]
        );
    }

    #[test]
    fn shell_contexts_reset_at_operators_and_enter_substitutions() {
        let directory = Directory::new();
        let engine = engine();
        for line in [
            "echo x; cargo bu",
            "echo x | cargo bu",
            "if cargo bu",
            "echo (cargo bu",
            "begin\ncargo bu",
        ] {
            assert_eq!(
                replacements(&engine, line, &directory.0),
                ["build "],
                "{line}"
            );
        }
        assert!(replacements(&engine, "echo # cargo bu", &directory.0).is_empty());
        assert_eq!(
            replacements(&engine, "echo # comment\ncargo bu", &directory.0),
            ["build "]
        );
    }

    #[test]
    fn redirected_paths_do_not_become_command_arguments() {
        let line = "cargo 2>errors.txt bu";
        let word = completion_word(line, line.len(), true);
        assert_eq!(word.command(), Some("cargo"));
        assert!(word.arguments().is_empty());
        assert_eq!(word.value, "bu");
    }

    #[test]
    fn declarative_completions_support_flags_arguments_descriptions_and_no_files() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("unrelated.txt"), "").unwrap();
        let mut engine = engine();
        engine.set_registrations(vec![CompletionSpec {
            command: "example".into(),
            short_option: Some('m'),
            long_option: Some("mode".into()),
            arguments: vec!["fast\tUse fewer passes".into(), "thorough".into()],
            description: "Choose a mode".into(),
            no_files: true,
            requires_argument: true,
            ..CompletionSpec::default()
        }]);
        assert_eq!(
            replacements(&engine, "example --m", &directory.0),
            ["--mode "]
        );
        assert!(replacements(&engine, "example ", &directory.0).is_empty());
        assert_eq!(
            replacements(&engine, "example --mode f", &directory.0),
            ["fast "]
        );
        assert_eq!(
            replacements(&engine, "example --mode=f", &directory.0),
            ["--mode=fast "]
        );
        let candidates = engine.complete("example -m f", 12, &directory.0).1;
        assert_eq!(candidates[0].display, "fast  (Use fewer passes)");
    }

    #[test]
    fn completion_preserves_text_after_the_cursor() {
        let engine = engine();
        let directory = Directory::new();
        let line = "cargo bild";
        let (start, candidates) = engine.complete(line, 7, &directory.0);
        assert_eq!(start, 6);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            format!(
                "{}{}{}",
                &line[..start],
                candidates[0].replacement,
                &line[7..]
            ),
            "cargo build"
        );
    }

    #[test]
    fn unicode_command_names_do_not_panic_during_suffix_detection() {
        assert_eq!(command_name("é魚"), "é魚");
        assert_eq!(command_name("魚.EXE"), "魚");
        assert_eq!(command_name("foo.cmd"), "foo.cmd");
    }

    #[test]
    fn function_arguments_only_complete_functions() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_context(vec!["my_function".into()], None, Vec::new());
        assert_eq!(
            replacements(&engine, "functions ", &directory.0),
            ["my_function "]
        );
    }
}
