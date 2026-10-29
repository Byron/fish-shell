//! Native command-line completion with an optional Fish evaluator callback.
//!
//! Keep terminal handling out of this module. Completion consumes a snapshot of
//! the evaluator's functions, variables and registrations. An evaluator callback
//! runs only registered conditions and generators in the live session. Git refs
//! come from in-process gix queries when the `git` feature is enabled; typed
//! source is never executed.

use super::InputLanguage;
#[cfg(feature = "git")]
use super::interactive_git_context::RefFilter;
use super::interactive_git_context::{self as git_context, Target};
#[cfg(feature = "git")]
use crate::git::{GitRef, GitRefs, RefKind as GitRefKind};
use rustyline::completion::Pair;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[path = "interactive_completion_paths.rs"]
mod variable_paths;

/// A registration keeps source unevaluated until a relevant completion query.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct CompletionSpec {
    pub command: String,
    #[serde(default)]
    pub is_path: bool,
    pub short_option: Option<char>,
    pub long_option: Option<String>,
    pub old_option: Option<String>,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub argument_sources: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<String>,
    pub description: String,
    pub no_files: bool,
    #[serde(default)]
    pub force_files: bool,
    pub requires_argument: bool,
    #[serde(default)]
    pub keep_order: bool,
    /// Wrapping and explicit erasure are stored in the same session snapshot as
    /// registrations, but do not contribute option or argument candidates.
    #[serde(default)]
    pub wrap_target: Option<String>,
    #[serde(default)]
    pub erased: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(super) struct CompletionContext {
    pub line: String,
    /// UTF-8 byte offset, like the native editor cursor.
    pub cursor: usize,
}

type Evaluator<'a> =
    dyn FnMut(&CompletionContext, &CompletionSpec, bool) -> rash::Result<Option<Vec<String>>> + 'a;

pub(super) struct CompletionEngine {
    fish: bool,
    input_language: InputLanguage,
    builtins: BTreeSet<String>,
    commands: BTreeSet<String>,
    functions: BTreeSet<String>,
    executables: BTreeSet<String>,
    executable_paths: BTreeMap<String, PathBuf>,
    variables: Vec<String>,
    variable_values: BTreeMap<String, super::fish::Variable>,
    registrations: Vec<CompletionSpec>,
    wrappers: BTreeMap<String, Vec<Vec<String>>>,
    #[cfg(feature = "git")]
    git: GitRefs,
}

impl CompletionEngine {
    pub(super) fn new(language: &str) -> Self {
        let builtins = if language == "rash" {
            rash::session::BUILTINS
                .iter()
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect()
        } else {
            super::fish::builtin_names()
                .iter()
                .copied()
                .chain([
                    "switch", "case", "if", "else", "end", "for", "while", "function", "begin",
                    "not", "and", "or",
                ])
                .map(str::to_owned)
                .collect()
        };
        let mut engine = Self {
            fish: language == "fish",
            input_language: InputLanguage::Fish,
            builtins,
            commands: BTreeSet::new(),
            functions: BTreeSet::new(),
            executables: BTreeSet::new(),
            executable_paths: BTreeMap::new(),
            variables: Vec::new(),
            variable_values: BTreeMap::new(),
            registrations: Vec::new(),
            wrappers: BTreeMap::new(),
            #[cfg(feature = "git")]
            git: GitRefs::new(),
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
        self.executable_paths.clear();
        self.variables = variables;
        self.variable_values.clear();
        for name in super::utilities::names() {
            self.commands.insert((*name).into());
            self.executables.insert((*name).into());
        }
        let directories = path
            .as_ref()
            .into_iter()
            .flat_map(|path| std::env::split_paths(path));
        for directory in directories {
            let directory = if directory.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                directory
            };
            let directory = std::path::absolute(&directory).unwrap_or(directory);
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if executable_name(name) && entry.path().is_file() {
                    self.commands.insert(command_name(name).into());
                    self.executables.insert(name.into());
                    self.executable_paths
                        .entry(command_name(name).to_ascii_lowercase())
                        .or_insert_with(|| entry.path());
                }
            }
        }
    }

    pub(super) fn set_registrations(&mut self, registrations: Vec<CompletionSpec>) {
        self.registrations = registrations;
    }

    pub(super) fn set_variable_values(&mut self, values: BTreeMap<String, super::fish::Variable>) {
        self.variable_values = values;
    }

    pub(super) fn set_wrappers(&mut self, wrappers: Vec<(String, Vec<String>)>) {
        self.wrappers = wrappers
            .into_iter()
            .map(|(name, targets)| {
                (
                    name,
                    targets
                        .iter()
                        .take(MAX_WRAPPER_CONTEXTS)
                        .filter_map(|target| literal_wrapper(target))
                        .collect(),
                )
            })
            .collect();
    }

    pub(super) fn set_environment(&mut self, environment: Vec<(OsString, OsString)>) {
        #[cfg(feature = "git")]
        self.git.set_environment(environment);
        #[cfg(not(feature = "git"))]
        let _ = environment;
    }

    pub(super) fn set_input_language(&mut self, language: InputLanguage) {
        self.input_language = language;
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
        self.complete_evaluated(line, pos, cwd, false, &mut |_, spec, arguments| {
            Ok(Some(if arguments {
                spec.arguments.clone()
            } else {
                Vec::new()
            }))
        })
        .expect("literal completion cannot fail")
    }

    pub(super) fn complete_evaluated(
        &self,
        line: &str,
        pos: usize,
        cwd: &Path,
        shell_output: bool,
        evaluate: &mut Evaluator<'_>,
    ) -> rash::Result<(usize, Vec<Pair>)> {
        if pos > line.len() || !line.is_char_boundary(pos) {
            return Ok((pos.min(line.len()), Vec::new()));
        }
        let word = completion_word(
            line,
            pos,
            self.fish && self.input_language == InputLanguage::Fish,
        );
        if word.comment {
            return Ok((pos, Vec::new()));
        }
        if let Some(start) = word.variable {
            let mut matches = Candidates::new(&line[start..pos]);
            for variable in &self.variables {
                if valid_variable(variable)
                    && (self.input_language != InputLanguage::Bash || variable.is_ascii())
                {
                    matches.add(variable, variable, "Variable", false);
                }
            }
            let mut result = matches.finish_style(false, false, shell_output);
            let suffix: String = line[pos..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if word.braced_variable {
                let tail = &line[pos + suffix.len()..];
                if !tail.is_empty()
                    && !tail.starts_with('}')
                    && !word.quote.is_some_and(|quote| tail.starts_with(quote))
                {
                    // Keep unsupported parameter operators and indices whole.
                    // Closing the name before `:-` or `[0]` would turn the
                    // remainder of an expansion into unrelated literal text.
                    return Ok((pos, Vec::new()));
                }
            }
            preserve_suffix(&mut result, &suffix);
            if word.braced_variable && suffix.is_empty() && !line[pos..].starts_with('}') {
                for candidate in &mut result {
                    candidate.replacement.push('}');
                }
            }
            return Ok((start, result));
        }
        // Only bounded variable directory expressions are eligible for path
        // lookup. Typed substitutions are never evaluated.
        if (word.expansion && self.variable_path(line, pos, &word, "").is_none())
            || word.value.contains('\n')
            || word.value.contains('`') && self.input_language != InputLanguage::Bash
        {
            return Ok((word.start, Vec::new()));
        }

        let command = word.command();
        let prefix = &word.value;
        let mut matches = Candidates::new(prefix);
        let mut files = true;
        if !word.redirect {
            if command.is_some() {
                if self.fish {
                    files = self.contextual_with_wrappers(
                        &word,
                        CompletionContext {
                            line: line.into(),
                            cursor: pos,
                        },
                        cwd,
                        &mut matches,
                        evaluate,
                    )?;
                }
            } else if !prefix.contains(['/', '\\']) {
                for name in &self.builtins {
                    if self.input_language != InputLanguage::Bash || !fish_only_keyword(name) {
                        matches.add(name, name, "Builtin", true);
                    }
                }
                if self.input_language == InputLanguage::Bash {
                    for name in ["then", "fi", "elif", "do", "done", "until", "in"] {
                        matches.add(name, name, "Keyword", true);
                    }
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
            if word.expansion {
                let option = matches.file_prefix.clone().unwrap_or_default();
                if let Some(path) = self.variable_path(line, pos, &word, &option) {
                    self.complete_variable_paths(&word, &path, line, pos, cwd, &mut matches);
                }
            } else if let Some(prefix) = matches.file_prefix.clone() {
                let mut argument = word.clone();
                argument.value = word.value[prefix.len()..].into();
                let mut paths = Candidates::new(&argument.value);
                self.complete_paths(&argument, cwd, &mut paths);
                for entry in paths.entries.into_values() {
                    let value = format!("{prefix}{}", entry.value);
                    matches.add_path(&value, &entry.description, entry.space, Some(entry.rank));
                }
            } else {
                self.complete_paths(&word, cwd, &mut matches);
            }
        }
        // A following token or closing quote already provides separation. For
        // an interior cursor, only offer candidates that preserve its suffix.
        let tail = token_tail(line, pos, word.quote);
        let append_space = pos == line.len();
        // Bash's input adapter has no ANSI-C quoting. Do not insert literal
        // terminal controls or offer replacements that change their value.
        if self.input_language == InputLanguage::Bash {
            matches
                .entries
                .retain(|_, entry| !entry.value.chars().any(char::is_control));
        }
        let mut result = matches.finish_style(true, append_space, shell_output);
        preserve_suffix(&mut result, &tail);
        Ok((word.start, result))
    }

    /// Discover completion providers without evaluating any typed source. The
    /// loader can refresh function/registration snapshots and repeat discovery
    /// when a newly sourced file introduces additional wrapping relationships.
    pub(super) fn autoload_commands(
        &self,
        line: &str,
        pos: usize,
        cwd: &Path,
    ) -> Vec<(String, CompletionContext)> {
        if !self.fish || pos > line.len() || !line.is_char_boundary(pos) {
            return Vec::new();
        }
        let word = completion_word(line, pos, self.input_language == InputLanguage::Fish);
        if word.comment
            || word.redirect
            || word.variable.is_some()
            || (word.expansion && self.variable_path(line, pos, &word, "").is_none())
            || word.value.contains('\n')
            || word.value.contains('`') && self.input_language != InputLanguage::Bash
        {
            return Vec::new();
        }
        let mut seen = BTreeSet::new();
        self.wrapper_contexts(
            &word,
            CompletionContext {
                line: line.into(),
                cursor: pos,
            },
        )
        .into_iter()
        .filter_map(|(word, context)| {
            let raw = word.command()?;
            let name = command_name(raw.rsplit(['/', '\\']).next().unwrap_or(raw));
            if (name == "git" || self.command_exists(raw, cwd)) && seen.insert(name.to_owned()) {
                Some((name.to_owned(), context))
            } else {
                None
            }
        })
        .collect()
    }

    fn contextual_with_wrappers(
        &self,
        word: &Word,
        context: CompletionContext,
        cwd: &Path,
        matches: &mut Candidates,
        evaluate: &mut Evaluator<'_>,
    ) -> rash::Result<bool> {
        let mut files = FilePolicy::Inherit;
        for (word, context) in self.wrapper_contexts(word, context) {
            let Some(command) = word.command() else {
                continue;
            };
            let policy = self.contextual(command, &word, &context, cwd, matches, evaluate)?;
            if files == FilePolicy::Inherit {
                files = policy;
            }
        }
        Ok(files != FilePolicy::Skip)
    }

    fn wrapper_contexts(
        &self,
        word: &Word,
        context: CompletionContext,
    ) -> Vec<(Word, CompletionContext)> {
        let mut pending = vec![(word.clone(), context, BTreeSet::new())];
        let mut contexts = Vec::new();
        for _ in 0..MAX_WRAPPER_CONTEXTS {
            let Some((word, context, mut visited)) = pending.pop() else {
                break;
            };
            let Some(index) = word.command_index() else {
                continue;
            };
            let command = &word.words[index];
            contexts.push((word.clone(), context.clone()));
            if !visited.insert(command.clone()) {
                continue;
            }
            // Completion wrappers also apply to explicit external commands;
            // function metadata follows normal function dispatch rules.
            let mut targets: Vec<_> = self
                .registrations
                .iter()
                .filter(|spec| !spec.is_path && spec.command == *command)
                .filter_map(|spec| spec.wrap_target.as_deref().and_then(literal_wrapper))
                .collect();
            if !word.words[..index]
                .iter()
                .any(|word| matches!(word.as_str(), "command" | "builtin" | "exec"))
            {
                targets.extend(self.wrappers.get(command).into_iter().flatten().cloned());
            }
            for target in targets.iter().rev() {
                if self.input_language == InputLanguage::Bash
                    && target.iter().any(|word| word.chars().any(char::is_control))
                {
                    continue;
                }
                if pending.len() >= MAX_WRAPPER_CONTEXTS
                    || word.words.len() + target.len() > MAX_WRAPPER_WORDS
                {
                    break;
                }
                // Keep preceding syntax and already-typed arguments intact while
                // supplying the wrapped command's fixed arguments before them.
                let replacement = target
                    .iter()
                    .map(|word| quote_completion(word))
                    .collect::<Vec<_>>()
                    .join(" ");
                let span = word.spans[index].clone();
                let mut wrapped_context = context.clone();
                wrapped_context
                    .line
                    .replace_range(span.clone(), &replacement);
                wrapped_context.cursor = context.cursor - span.len() + replacement.len();
                let wrapped = completion_word(
                    &wrapped_context.line,
                    wrapped_context.cursor,
                    self.input_language == InputLanguage::Fish,
                );
                pending.push((wrapped, wrapped_context, visited.clone()));
            }
        }
        contexts
    }

    fn contextual(
        &self,
        raw_command: &str,
        word: &Word,
        context: &CompletionContext,
        cwd: &Path,
        matches: &mut Candidates,
        evaluate: &mut Evaluator<'_>,
    ) -> rash::Result<FilePolicy> {
        let command = command_name(
            raw_command
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(raw_command),
        );
        let args = word.arguments();
        let positional = args.iter().filter(|arg| !arg.starts_with('-')).count();
        let use_options = !args.iter().any(|arg| arg == "--");
        let options = word.value.starts_with('-') && use_options;
        let mut files = true;
        let registrations: Vec<_> = self
            .registrations
            .iter()
            .enumerate()
            .filter(|(_, spec)| self.registration_matches(spec, raw_command, command, cwd))
            .collect();
        // Script registrations and explicit erasure own this command's choices.
        // Native conveniences are only a fallback before scripts load.
        if registrations.is_empty() {
            if command == "git" {
                if let Some(context) = git_context::context(args, &word.value) {
                    files = context.files;
                    match context.target {
                        Target::Subcommand => {
                            add_choices(matches, subcommand_choices("git").unwrap());
                        }
                        Target::Options(choices) => add_choices(matches, choices),
                        #[cfg(feature = "git")]
                        Target::Refs(filter) if word.literal_command() => {
                            for reference in self.git.query(cwd, &context.globals) {
                                add_git_ref(matches, reference, filter, &context.prefix);
                            }
                        }
                        Target::Refs(filter) => {
                            let _ = (filter, &context.prefix);
                        }
                        Target::Files | Target::None => {}
                    }
                }
            } else if options {
                if let Some(entries) = option_choices(command) {
                    add_choices(matches, entries);
                }
            } else if positional == 0
                && let Some(entries) = subcommand_choices(command)
            {
                add_choices(matches, entries);
                files = false;
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
            if command == "history" {
                files = false;
            }
            return Ok(if files {
                FilePolicy::Inherit
            } else {
                FilePolicy::Skip
            });
        }

        let mut active = Vec::new();
        for (index, spec) in registrations.into_iter().rev() {
            if !spec.erased
                && spec.wrap_target.is_none()
                && spec.applies_to_token(
                    &word.value,
                    args.last().map_or("", String::as_str),
                    use_options,
                )
                && evaluate(context, spec, false)?.is_some()
            {
                active.push((index, spec));
            }
        }
        let current_short = short_option_pos(&word.value, &active);
        let previous = args.last().map_or("", String::as_str);
        let previous_short = short_option_pos(previous, &active);
        let old_previous = active.iter().any(|(_, spec)| {
            spec.old_option
                .as_ref()
                .is_some_and(|name| previous == format!("-{name}"))
        });
        let mut file_policy = FilePolicy::Inherit;
        let mut claimed = false;
        if use_options {
            for (index, spec) in &active {
                let attached = if word.value.starts_with('-') {
                    spec.short_option
                        .and_then(|option| {
                            current_short
                                .filter(|(_, c)| *c == option)
                                .map(|(offset, c)| offset + c.len_utf8())
                        })
                        .or_else(|| {
                            [
                                spec.long_option.as_ref().map(|name| format!("--{name}=")),
                                spec.old_option.as_ref().map(|name| format!("-{name}=")),
                            ]
                            .into_iter()
                            .flatten()
                            .find(|prefix| word.value.starts_with(prefix))
                            .map(|prefix| prefix.len())
                        })
                } else {
                    None
                };
                let follows = !word.value.starts_with('-')
                    && if old_previous {
                        spec.old_option
                            .as_ref()
                            .is_some_and(|name| previous == format!("-{name}"))
                    } else {
                        spec.requires_argument
                            && (spec
                                .long_option
                                .as_ref()
                                .is_some_and(|name| previous == format!("--{name}"))
                                || spec.short_option.is_some_and(|c| {
                                    previous_short
                                        == Some((previous.len().saturating_sub(c.len_utf8()), c))
                                }))
                    };
                if attached.is_some() || follows {
                    claimed |= spec.requires_argument;
                    file_policy = file_policy.max(spec.file_policy());
                    let prefix = attached.map_or("", |at| &word.value[..at]);
                    if !prefix.is_empty() {
                        matches.file_prefix = Some(prefix.into());
                    }
                    self.add_arguments(context, spec, *index, prefix, matches, evaluate)?;
                }
            }
        }
        if claimed {
            return Ok(file_policy);
        }
        let short_requires_argument = current_short.is_some_and(|(_, c)| {
            active
                .iter()
                .filter(|(_, spec)| spec.short_option == Some(c))
                .all(|(_, spec)| spec.requires_argument)
        });
        for (index, spec) in active {
            if spec.short_option.is_none()
                && spec.long_option.is_none()
                && spec.old_option.is_none()
            {
                file_policy = file_policy.max(spec.file_policy());
                self.add_arguments(context, spec, index, "", matches, evaluate)?;
            }
            if !options {
                continue;
            }
            matches.order_group = spec.keep_order.then_some(index);
            if let Some(option) = spec.short_option {
                let value = if word.value == "-" {
                    Some(format!("-{option}"))
                } else if current_short.is_some_and(|(at, c)| at + c.len_utf8() == word.value.len())
                    && !short_requires_argument
                    && !word.value[1..].contains(option)
                {
                    Some(format!("{}{option}", word.value))
                } else {
                    None
                };
                if let Some(value) = value {
                    matches.add(&value, &value, &spec.description, true);
                }
            }
            for flag in [
                spec.long_option.as_ref().map(|name| format!("--{name}")),
                spec.old_option.as_ref().map(|name| format!("-{name}")),
            ]
            .into_iter()
            .flatten()
            {
                if spec.long_option.is_some()
                    && !spec.requires_argument
                    && (!spec.arguments.is_empty()
                        || spec
                            .argument_sources
                            .iter()
                            .any(|source| !source.is_empty()))
                {
                    let attached = format!("{flag}=");
                    matches.add(&attached, &attached, &spec.description, false);
                }
                matches.add(&flag, &flag, &spec.description, true);
            }
        }
        matches.order_group = None;
        Ok(file_policy)
    }

    fn registration_matches(
        &self,
        spec: &CompletionSpec,
        raw: &str,
        command: &str,
        cwd: &Path,
    ) -> bool {
        let value = if spec.is_path {
            if raw.contains(['/', '\\']) {
                std::path::absolute(cwd.join(raw)).ok()
            } else {
                self.executable_paths
                    .get(&command_name(raw).to_ascii_lowercase())
                    .cloned()
            }
            .and_then(|path| std::path::absolute(path).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
        } else {
            Some(command.to_owned())
        };
        value.is_some_and(|value| {
            let pattern = fish_wcstringutil::unescape_wildcards(
                &fish_widestring::WString::from(if spec.is_path {
                    spec.command.replace('\\', "/")
                } else {
                    spec.command.clone()
                }),
                false,
            );
            fish_wcstringutil::wildcard_match(
                fish_widestring::WString::from(value),
                &pattern,
                false,
            )
        })
    }

    fn add_arguments(
        &self,
        context: &CompletionContext,
        spec: &CompletionSpec,
        index: usize,
        prefix: &str,
        matches: &mut Candidates,
        evaluate: &mut Evaluator<'_>,
    ) -> rash::Result<()> {
        matches.order_group = spec.keep_order.then_some(index);
        if let Some(arguments) = evaluate(context, spec, true)? {
            for argument in arguments {
                let (value, description) = argument
                    .split_once('\t')
                    .unwrap_or((&argument, &spec.description));
                let value = format!("{prefix}{value}");
                matches.add(&value, &value, description, true);
            }
        }
        matches.order_group = None;
        Ok(())
    }

    fn complete_paths(&self, word: &Word, cwd: &Path, matches: &mut Candidates) {
        let value = word.value.replace('\\', "/");
        if matches!(value.as_str(), "." | "..") {
            let directory = format!("{value}/");
            matches.add_path(&directory, "Directory", false, None);
        }
        if value == "~" && home_dir().is_some_and(|home| home.is_dir()) {
            matches.add_path("~/", "Home directory", false, None);
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
            if command.is_none() && !word.redirect && !is_directory && directory.is_empty() {
                // Bare command names come from PATH or packaged utilities.
                // A local executable outside PATH requires an explicit path.
                continue;
            }
            let value = format!("{directory}{name}{}", if is_directory { "/" } else { "" });
            // Match the basename rather than characters in the already typed
            // directory. Preserve ~/ instead of replacing it with a long path.
            if let Some(rank) = match_rank(name, prefix) {
                matches.add_path(
                    &value,
                    if is_directory { "Directory" } else { "" },
                    !is_directory,
                    Some(rank),
                );
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FilePolicy {
    Inherit,
    Skip,
    Force,
}

impl CompletionSpec {
    /// Like POSIX completion, only ask conditions for options which could
    /// contribute a name, argument or file policy at this cursor. Conditions
    /// often run external programs, so unrelated options must stay lazy.
    fn applies_to_token(&self, current: &str, previous: &str, use_options: bool) -> bool {
        if self.short_option.is_none() && self.long_option.is_none() && self.old_option.is_none() {
            return true;
        }
        if !use_options {
            return false;
        }
        if self.short_option.is_some()
            && ((current.starts_with('-') && !current.starts_with("--"))
                || (previous.starts_with('-')
                    && !previous.starts_with("--")
                    && !current.starts_with('-')))
        {
            // Keep the whole short-option group for argument-boundary lookup.
            return true;
        }
        for (name, old) in [
            (self.long_option.as_ref(), false),
            (self.old_option.as_ref(), true),
        ] {
            let Some(name) = name else { continue };
            let flag = format!("{}{name}", if old { "-" } else { "--" });
            if current.starts_with('-') {
                if match_rank(&flag, current).is_some() || current.starts_with(&format!("{flag}="))
                {
                    return true;
                }
            } else if (old || self.requires_argument) && previous == flag {
                return true;
            }
        }
        false
    }

    fn file_policy(&self) -> FilePolicy {
        if self.force_files {
            FilePolicy::Force
        } else if self.no_files {
            FilePolicy::Skip
        } else {
            FilePolicy::Inherit
        }
    }
}

/// Walk a short-option group until an unknown switch or an option taking an
/// argument. Byte offsets preserve non-ASCII short options in the native editor.
fn short_option_pos(value: &str, options: &[(usize, &CompletionSpec)]) -> Option<(usize, char)> {
    if !value.starts_with('-') || value.starts_with("--") {
        return None;
    }
    let mut last = None;
    for (offset, c) in value.char_indices().skip(1) {
        let Some((_, spec)) = options
            .iter()
            .find(|(_, spec)| spec.short_option == Some(c))
        else {
            break;
        };
        last = Some((offset, c));
        if spec.requires_argument {
            break;
        }
    }
    last
}

fn fish_only_keyword(name: &str) -> bool {
    matches!(
        name,
        "begin" | "end" | "switch" | "case" | "and" | "or" | "not"
    )
}

const MAX_WRAPPER_CONTEXTS: usize = 64;
const MAX_WRAPPER_WORDS: usize = 256;

fn literal_wrapper(target: &str) -> Option<Vec<String>> {
    use crate::tokenizer::{TokFlags, TokenType, Tokenizer};
    use fish_widestring::WString;

    if target.len() > 32 * 1024 || target.contains('\0') {
        return None;
    }
    let complete = format!("{target} ");
    let context = completion_word(&complete, complete.len(), true);
    if !context.literal_command() || context.quote.is_some() || context.comment {
        return None;
    }
    let source = WString::from(target);
    let mut words = Vec::new();
    for token in Tokenizer::new(
        &source,
        TokFlags {
            show_comments: true,
            ..Default::default()
        },
    ) {
        if token.type_ != TokenType::String || words.len() >= MAX_WRAPPER_WORDS {
            return None;
        }
        let raw = source[token.offset as usize..(token.offset + token.length) as usize].to_string();
        words.push(super::fish::unescape_literal(&raw).ok()?);
    }
    (!words.is_empty()).then_some(words)
}

#[cfg(feature = "git")]
fn add_git_ref(matches: &mut Candidates, reference: GitRef, filter: RefFilter, prefix: &str) {
    if filter == RefFilter::LocalBranches && reference.kind != GitRefKind::LocalBranch
        || filter == RefFilter::Branches
            && !matches!(
                reference.kind,
                GitRefKind::LocalBranch | GitRefKind::RemoteBranch
            )
    {
        return;
    }
    let (namespace, description) = match reference.kind {
        GitRefKind::LocalBranch => ("refs/heads/", "Local branch"),
        GitRefKind::RemoteBranch => ("refs/remotes/", "Remote branch"),
        GitRefKind::Tag => ("refs/tags/", "Tag"),
        GitRefKind::Head => ("", "HEAD"),
    };
    let short = reference
        .name
        .strip_prefix(namespace)
        .unwrap_or(&reference.name);
    // `switch` expects a branch name; a same-named tag is irrelevant there.
    let name = if filter == RefFilter::LocalBranches {
        short
    } else {
        &reference.name
    };
    let value = format!("{prefix}{name}");
    // Disambiguating a branch/tag collision must not make a matching file
    // outrank both refs merely because their replacements need qualification.
    let alias = format!("{prefix}{short}");
    if let Some(rank) = [
        match_rank(&value, &matches.prefix),
        match_rank(&alias, &matches.prefix),
    ]
    .into_iter()
    .flatten()
    .min()
    {
        matches.add_ranked(&value, &value, description, true, rank);
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

type Rank = u32;

fn match_rank(name: &str, prefix: &str) -> Option<Rank> {
    fish_wcstringutil::string_fuzzy_match_string(
        &fish_widestring::WString::from(prefix),
        &fish_widestring::WString::from(name),
        false,
    )
    .map(|matched| matched.rank())
}

struct Candidate {
    value: String,
    display: String,
    description: String,
    generated_description: bool,
    space: bool,
    rank: Rank,
    order_group: Option<usize>,
    sequence: usize,
    /// A directory expression must retain its variable and quote syntax.
    replacement: Option<String>,
}

struct Candidates {
    prefix: String,
    entries: BTreeMap<String, Candidate>,
    order_group: Option<usize>,
    file_prefix: Option<String>,
}

impl Candidates {
    fn add_path(&mut self, value: &str, description: &str, space: bool, rank: Option<Rank>) {
        if self.entries.contains_key(value) {
            return;
        }
        if let Some(rank) = rank.or_else(|| match_rank(value, &self.prefix)) {
            self.add_ranked(value, value, description, space, rank);
            self.entries.get_mut(value).unwrap().generated_description = true;
        }
    }

    fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.into(),
            entries: BTreeMap::new(),
            order_group: None,
            file_prefix: None,
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
        let sequence = self.entries.len();
        self.entries
            .entry(value.to_owned())
            .or_insert_with(|| Candidate {
                value: value.into(),
                display: display.into(),
                description: description.into(),
                generated_description: false,
                space,
                rank,
                order_group: self.order_group,
                sequence,
                replacement: None,
            });
    }

    #[cfg(all(test, feature = "git"))]
    fn finish(self, quote: bool, append_space: bool) -> Vec<Pair> {
        self.finish_style(quote, append_space, false)
    }

    fn finish_style(self, quote: bool, append_space: bool, shell_output: bool) -> Vec<Pair> {
        let best = self
            .entries
            .values()
            .map(|entry| entry.rank)
            .min()
            .unwrap_or(0);
        let mut entries: Vec<_> = self
            .entries
            .into_values()
            .filter(|entry| entry.rank == best)
            .collect();
        entries.sort_by(|a, b| {
            b.order_group.cmp(&a.order_group).then_with(|| {
                if a.order_group.is_some() {
                    a.sequence.cmp(&b.sequence)
                } else {
                    fish_util::wcsfilecmp(
                        &fish_widestring::WString::from(
                            a.value.strip_prefix(&self.prefix).unwrap_or(&a.value),
                        ),
                        &fish_widestring::WString::from(
                            b.value.strip_prefix(&self.prefix).unwrap_or(&b.value),
                        ),
                    )
                }
            })
        });
        entries
            .into_iter()
            .map(|entry| Pair {
                display: if !shell_output {
                    let display = display_controls(&entry.display);
                    if entry.description.is_empty() {
                        display
                    } else {
                        format!("{}  ({})", display, display_controls(&entry.description))
                    }
                } else if entry.description.is_empty() || entry.generated_description {
                    entry.display
                } else {
                    format!("{}\t{}", entry.display, entry.description)
                },
                replacement: format!(
                    "{}{}",
                    if let Some(replacement) = entry.replacement {
                        replacement
                    } else if quote {
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
        if character.is_control() {
            write!(result, "\\u{:04x}", character as u32).unwrap();
            continue;
        }
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

fn display_controls(value: &str) -> String {
    let mut display = String::new();
    for character in value.chars() {
        if character.is_control() {
            display.extend(character.escape_default());
        } else {
            display.push(character);
        }
    }
    display
}

#[derive(Clone, Default)]
struct Word {
    start: usize,
    value: String,
    words: Vec<String>,
    spans: Vec<std::ops::Range<usize>>,
    quote: Option<char>,
    variable: Option<usize>,
    braced_variable: bool,
    bash: bool,
    expansion: bool,
    /// A preceding argument needs shell expansion, so repository routing is unknown.
    expanded_words: bool,
    redirect: bool,
    comment: bool,
}

impl Word {
    fn literal_command(&self) -> bool {
        !self.expanded_words
            && self
                .command_index()
                .is_some_and(|index| !self.words[..index].iter().any(|word| word.contains('=')))
    }

    fn command_index(&self) -> Option<usize> {
        self.words.iter().position(|word| {
            !matches!(
                word.as_str(),
                "command" | "builtin" | "exec" | "not" | "and" | "or" | "if" | "while" | "else"
            ) && !(self.bash
                && matches!(
                    word.as_str(),
                    "then" | "do" | "elif" | "until" | "!" | "{" | "}"
                ))
                && !word
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
    if !fish {
        return scan_completion_word(line, pos, false);
    }
    // Use the same process/substitution boundaries as POSIX completion and
    // commandline queries, including substitutions inside double quotes.
    let source = fish_widestring::WString::from(line);
    let cursor = line[..pos].chars().count();
    let range = crate::commandline_extent::get_process_extent(&source, cursor, None);
    let start = line
        .char_indices()
        .nth(range.start)
        .map_or(line.len(), |(at, _)| at);
    let mut word = scan_completion_word(&line[start..], pos - start, true);
    word.start += start;
    word.variable = word.variable.map(|at| at + start);
    for span in &mut word.spans {
        span.start += start;
        span.end += start;
    }
    word
}

fn scan_completion_word(line: &str, pos: usize, fish: bool) -> Word {
    let mut word = Word {
        start: pos,
        bash: !fish,
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
                    bash: !fish,
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
            word.variable = None;
            if let Some((_, next)) = chars.next() {
                if next == '\n' {
                    continue;
                }
                if fish && next == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                    chars.next();
                    continue;
                }
                active = true;
                if word.quote == Some('"')
                    && (!matches!(next, '$' | '`' | '"' | '\\' | '\n') || !fish && next == '`')
                {
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
        } else if !fish
            && character == '$'
            && word.quote != Some('\'')
            && chars.peek().is_some_and(|(_, next)| *next == '(')
        {
            chars.next();
            word.expansion = true;
            substitutions.push(word);
            word = Word {
                start: pos,
                bash: true,
                ..Word::default()
            };
            active = false;
        } else if !fish
            && word.quote.is_none()
            && character == '('
            && chars.peek().is_some_and(|(_, next)| *next == ')')
            && (word.words.is_empty() || word.words == ["function"])
        {
            // A Bash function signature opens a body, not a substitution in
            // the function name. Keep completion inside its following group.
            chars.next();
            word = Word {
                start: pos,
                bash: true,
                ..Word::default()
            };
            active = false;
        } else if word.quote.is_none() && character == '(' {
            substitutions.push(word);
            word = Word {
                start: pos,
                bash: !fish,
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
                bash: !fish,
                ..Word::default()
            };
            active = false;
        } else if word.quote.is_none() && matches!(character, '<' | '>') {
            if active && (word.expansion || !word.value.chars().all(|c| c.is_ascii_digit())) {
                word.expanded_words |= word.expansion;
                word.words.push(std::mem::take(&mut word.value));
                word.spans.push(word.start..at);
            }
            word.value.clear();
            word.redirect = true;
            word.variable = None;
            word.braced_variable = false;
            word.expansion = false;
            active = false;
        } else if word.quote.is_none() && character.is_whitespace() {
            if active {
                if word.redirect {
                    word.redirect = false;
                } else {
                    word.expanded_words |= word.expansion;
                    word.words.push(std::mem::take(&mut word.value));
                    word.spans.push(word.start..at);
                }
            }
            word.value.clear();
            word.variable = None;
            word.braced_variable = false;
            word.expansion = false;
            active = false;
        } else {
            active = true;
            if word.quote.is_none()
                && (matches!(character, '*' | '?' | '[' | '{')
                    || character == '~' && word.value.is_empty())
            {
                word.expansion = true;
            }
            if character == '$' && word.quote != Some('\'') {
                word.variable = Some(at + 1);
                word.braced_variable = false;
                word.expansion = true;
                if !fish && chars.peek().is_some_and(|(_, next)| *next == '{') {
                    chars.next();
                    word.value.push_str("${");
                    word.variable = Some(at + 2);
                    word.braced_variable = true;
                    continue;
                }
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
        "history" => &[
            ("search", "Search command history"),
            ("append", "Add commands to history"),
            ("delete", "Remove exact commands"),
            ("save", "Save command history"),
            ("merge", "Read other sessions' history"),
            ("clear", "Clear command history"),
            ("clear-session", "Clear this session's history"),
        ],
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
            ("reset", "Reset the index or current branch"),
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
            ("is-native-windows", "Test native Windows backend"),
            ("current-command", "Print current command"),
            ("is-block", "Test block scope"),
            ("is-command-substitution", "Test captured output"),
            ("fish-path", "Print path to fish"),
        ],
        "fish_input_mode" => &[
            ("fish", "Use Fish input syntax"),
            ("bash", "Use Bash-shaped Fish input"),
        ],
        _ => return None,
    })
}

fn option_choices(command: &str) -> Option<Choices> {
    Some(match command {
        "history" => &[
            ("--prefix", "Match commands starting with a pattern"),
            ("--contains", "Match a pattern anywhere"),
            ("--exact", "Match complete commands"),
            ("--case-sensitive", "Match letter case"),
            ("--max", "Limit the number of results"),
            ("--reverse", "Reverse selected results"),
            ("--null", "Terminate records with NUL"),
            ("--show-time", "Show command timestamps"),
            ("--color", "Control syntax colors"),
            ("--help", "Show help"),
        ],
        "set" => &[
            ("--local", "Set a local variable"),
            ("--global", "Set a global variable"),
            ("--export", "Export to child processes"),
            ("--unexport", "Do not export"),
            ("--erase", "Erase a variable"),
            ("--query", "Test whether variables exist"),
            ("--append", "Append values"),
            ("--prepend", "Prepend values"),
            ("--no-event", "Do not fire variable event handlers"),
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
            ("-g", "Print pipeline group identifiers"),
            ("--group", "Print pipeline group identifiers"),
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

    #[test]
    fn generated_control_characters_are_safe_to_display_and_insert() {
        let directory = Directory::new();
        let mut engine = engine();
        let value = "alpha\r\n\x1bomega";
        engine.set_registrations(vec![CompletionSpec {
            command: "control_probe".into(),
            arguments: vec![value.into()],
            description: "description\x1b\r\n".into(),
            no_files: true,
            ..Default::default()
        }]);
        let (_, candidates) = engine.complete("control_probe ", 14, &directory.0);
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert!(!candidate.display.chars().any(char::is_control));
        assert!(!candidate.replacement.chars().any(char::is_control));
        assert_eq!(
            crate::native::fish::unescape_literal(candidate.replacement.trim_end()).unwrap(),
            value
        );
        engine.set_input_language(InputLanguage::Bash);
        assert!(
            engine
                .complete("control_probe ", 14, &directory.0)
                .1
                .is_empty()
        );
    }

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

    #[test]
    fn autoload_discovery_reuses_wrapped_contexts_and_stops_cycles() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_context(
            vec!["outer".into(), "inner".into(), "target".into()],
            None,
            Vec::new(),
        );
        engine.set_wrappers(vec![
            ("outer".into(), vec!["inner fixed".into()]),
            ("inner".into(), vec!["target more".into()]),
            ("target".into(), vec!["outer".into()]),
        ]);
        let line = "echo 文 | outer par";
        let contexts = engine.autoload_commands(line, line.len(), &directory.0);
        assert_eq!(
            contexts
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["outer", "inner", "target"]
        );
        assert_eq!(contexts[0].1.line, line);
        assert_eq!(contexts[1].1.line, "echo 文 | inner fixed par");
        assert_eq!(contexts[2].1.line, "echo 文 | target more fixed par");
        assert!(
            contexts
                .iter()
                .all(|(_, context)| context.cursor == context.line.len())
        );
    }

    #[test]
    fn autoload_discovery_retains_completion_wrappers_on_explicit_commands() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_context(
            vec![
                "outer".into(),
                "function_target".into(),
                "registered_target".into(),
            ],
            None,
            Vec::new(),
        );
        engine.set_wrappers(vec![("outer".into(), vec!["function_target fixed".into()])]);
        engine.set_registrations(vec![CompletionSpec {
            command: "outer".into(),
            wrap_target: Some("registered_target extra".into()),
            ..Default::default()
        }]);
        for decoration in ["command", "builtin", "exec"] {
            let line = format!("{decoration} outer ");
            let contexts = engine.autoload_commands(&line, line.len(), &directory.0);
            assert_eq!(
                contexts
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
                ["outer", "registered_target"]
            );
            assert_eq!(
                contexts[1].1.line,
                format!("{decoration} registered_target extra ")
            );
        }
        let line = "outer ";
        assert_eq!(
            engine
                .autoload_commands(line, line.len(), &directory.0)
                .len(),
            3
        );
    }

    #[test]
    fn autoload_discovery_only_loads_existing_argument_commands() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("probe.exe"), b"").unwrap();
        let engine = engine();
        for line in [
            "git",
            "git # comment",
            "git $root",
            "git $root/",
            "git > path",
            "git (echo side)/file",
            "missing_native_helper ",
            "(echo git) ",
        ] {
            assert!(
                engine
                    .autoload_commands(line, line.len(), &directory.0)
                    .is_empty(),
                "{line}"
            );
        }
        for (line, expected) in [
            ("git ", "git"),
            ("echo ", "echo"),
            ("./probe.exe arg", "probe"),
            ("git (echo side) tail", "git"),
        ] {
            let contexts = engine.autoload_commands(line, line.len(), &directory.0);
            assert_eq!(contexts.len(), 1, "{line}");
            assert_eq!(contexts[0].0, expected, "{line}");
        }
        assert!(
            engine
                .autoload_commands("echo 文", 6, &directory.0)
                .is_empty()
        );
    }

    #[test]
    fn native_builtin_inventory_and_fish_keywords_remain_completable() {
        let directory = Directory::new();
        let engine = engine();
        for (prefix, expected) in [("realp", "realpath "), ("pat", "path "), ("whil", "while ")] {
            assert!(
                replacements(&engine, prefix, &directory.0).contains(&expected.into()),
                "{prefix}"
            );
        }
        for name in super::super::fish::builtin_names() {
            assert!(engine.builtins.contains(*name), "{name}");
        }
        let rash = CompletionEngine::new("rash");
        assert!(!rash.builtins.contains("fish_input_mode"));
        assert!(!rash.builtins.contains("realpath"));
    }

    #[test]
    fn packaged_utilities_complete_as_external_commands_without_path() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_context(Vec::new(), None, Vec::new());
        for name in super::super::utilities::names() {
            assert!(engine.commands.contains(*name), "{name}");
            assert!(engine.executables.contains(*name), "{name}");
            assert!(!engine.builtins.contains(*name), "{name}");
            assert!(!engine.executable_paths.contains_key(*name), "{name}");
            assert!(engine.command_exists(name, &directory.0), "{name}");
            assert!(
                replacements(&engine, name, &directory.0).contains(&format!("{name} ")),
                "{name}"
            );
            assert!(
                !replacements(&engine, &format!("builtin {name}"), &directory.0)
                    .contains(&format!("{name} ")),
                "{name}"
            );
        }
    }

    #[test]
    fn wrappers_reuse_registrations_and_preserve_fixed_arguments() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_registrations(vec![CompletionSpec {
            command: "target".into(),
            long_option: Some("mode".into()),
            arguments: vec!["fast".into(), "slow".into()],
            no_files: true,
            requires_argument: true,
            ..Default::default()
        }]);
        engine.set_wrappers(vec![
            ("alias".into(), vec!["target --mode".into()]),
            ("outer".into(), vec!["inner build".into()]),
            ("inner".into(), vec!["cargo".into()]),
        ]);
        assert_eq!(replacements(&engine, "alias f", &directory.0), ["fast "]);
        assert_eq!(
            replacements(&engine, "outer --rel", &directory.0),
            ["--release "]
        );
        assert!(replacements(&engine, "command alias f", &directory.0).is_empty());
        assert!(replacements(&engine, "builtin alias f", &directory.0).is_empty());
        // A wrapper may add fixed arguments to an external command with the
        // same name, without recursively inheriting its own wrapper again.
        engine.set_wrappers(vec![("cargo".into(), vec!["cargo build".into()])]);
        assert_eq!(
            replacements(&engine, "cargo --rel", &directory.0),
            ["--release "]
        );
    }

    #[test]
    fn multiple_wrappers_and_cycles_keep_all_reachable_choices() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_registrations(
            [("alias", "own"), ("first", "alpha"), ("second", "beta")]
                .into_iter()
                .map(|(command, choice)| CompletionSpec {
                    command: command.into(),
                    arguments: vec![choice.into()],
                    no_files: true,
                    ..Default::default()
                })
                .collect(),
        );
        engine.set_wrappers(vec![
            ("alias".into(), vec!["cycle".into(), "second".into()]),
            ("cycle".into(), vec!["alias extra".into(), "first".into()]),
        ]);
        assert_eq!(
            replacements(&engine, "alias ", &directory.0),
            ["alpha ", "beta ", "own "]
        );
        engine.set_wrappers(Vec::new());
        assert_eq!(replacements(&engine, "alias ", &directory.0), ["own "]);
    }

    #[test]
    fn wrapper_targets_are_literal_single_commands() {
        assert_eq!(
            literal_wrapper("git -C 'two words 文' checkout"),
            Some(vec![
                "git".into(),
                "-C".into(),
                "two words 文".into(),
                "checkout".into(),
            ])
        );
        assert_eq!(
            literal_wrapper("git -C 'literal$repo' checkout"),
            Some(vec![
                "git".into(),
                "-C".into(),
                "literal$repo".into(),
                "checkout".into(),
            ])
        );
        for target in [
            "",
            "git -C (echo repo) checkout",
            "git -C $REPO checkout",
            "git -C ~/repo checkout",
            "git -C repo* checkout",
            "git -C {one,two} checkout",
            "GIT_DIR=elsewhere git checkout",
            "git; cargo",
            "git | cargo",
            "git > output",
            "git # comment",
            "git 'unfinished",
        ] {
            assert!(literal_wrapper(target).is_none(), "{target}");
        }
    }

    #[cfg(feature = "git")]
    #[test]
    fn git_wrappers_route_prefilled_commands_to_shared_reference_queries() {
        let directory = Directory::new();
        let mut repository = gix::ThreadSafeRepository::init_opts(
            directory.0.join("repo with 文"),
            gix::create::Kind::WithWorktree,
            Default::default(),
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        repository.refs.write_reflog = gix::refs::store::WriteReflog::Disable;
        repository
            .reference(
                "refs/heads/topic",
                gix::ObjectId::from_hex(b"0123456789012345678901234567890123456789").unwrap(),
                gix::refs::transaction::PreviousValue::Any,
                "fixture",
            )
            .unwrap();
        let mut engine = engine();
        engine.set_environment(vec![("PATH".into(), "".into())]);
        engine.set_wrappers(vec![(
            "gco".into(),
            vec!["git -C 'repo with 文' checkout".into()],
        )]);
        for line in ["gco top", "not gco top"] {
            assert_eq!(
                replacements(&engine, line, &directory.0),
                ["topic "],
                "{line}"
            );
        }
        assert!(replacements(&engine, "gco -- top", &directory.0).is_empty());
        assert!(replacements(&engine, "GIT_DIR=elsewhere gco top", &directory.0).is_empty());
        let (start, candidates) = engine.complete("gco tpic", 5, &directory.0);
        assert_eq!(start, 4);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "to");
        engine.set_input_language(InputLanguage::Bash);
        assert_eq!(replacements(&engine, "gco top", &directory.0), ["topic "]);
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
    fn bash_contexts_reuse_fish_commands_and_contextual_completions() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_input_language(InputLanguage::Bash);
        for line in [
            "if true; then ech",
            "for item in one; do ech",
            "if false; then true; elif ech",
            "if false; then true; else ech",
            "until ech",
            "hello() { ech",
            "! ech",
        ] {
            assert!(
                replacements(&engine, line, &directory.0).contains(&"echo ".into()),
                "{line}"
            );
        }
        for line in [
            "if true; then cargo bu",
            "echo $(cargo bu",
            "echo \"$(cargo bu",
        ] {
            assert_eq!(
                replacements(&engine, line, &directory.0),
                ["build "],
                "{line}"
            );
        }
        assert_eq!(
            replacements(&engine, "fish_input_mode b", &directory.0),
            ["bash "]
        );
        assert!(!replacements(&engine, "end", &directory.0).contains(&"end ".into()));
        assert!(replacements(&engine, "fi", &directory.0).contains(&"fi ".into()));
        engine.set_input_language(InputLanguage::Fish);
        assert!(replacements(&engine, "end", &directory.0).contains(&"end ".into()));
    }

    #[test]
    fn bash_variable_completion_preserves_braces_quotes_and_interior_suffixes() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_input_language(InputLanguage::Bash);
        for (line, expected) in [
            ("echo ${PA", "PATH}"),
            ("echo \"${PA", "PATH}"),
            ("echo ${HOME}$PA", "PATH"),
        ] {
            let (start, candidates) = engine.complete(line, line.len(), &directory.0);
            assert_eq!(&line[start..], "PA", "{line}");
            assert_eq!(candidates[0].replacement, expected, "{line}");
        }
        for (line, cursor, expected) in [
            ("echo ${PA}", 9, "PATH"),
            ("echo ${PATH} tail", 9, "PA"),
            ("echo ${PATH", 9, "PA"),
        ] {
            let (start, candidates) = engine.complete(line, cursor, &directory.0);
            assert_eq!(start, 7);
            assert_eq!(candidates[0].replacement, expected, "{line}");
        }
        for line in ["echo '${PA", "echo ${HOME:-fall", "echo ${HOME}/prefix"] {
            assert!(
                replacements(&engine, line, &directory.0).is_empty(),
                "{line}"
            );
        }
        for line in [
            "echo ${PA:-fallback}",
            "echo ${PA[0]}",
            "echo ${PATH%tail}",
            "echo ${PA#prefix}",
            "echo ${PA other}",
        ] {
            let (_, candidates) = engine.complete(line, 9, &directory.0);
            assert!(candidates.is_empty(), "{line}");
        }
    }

    #[test]
    fn bash_literal_path_completion_preserves_quoting_and_source_after_the_cursor() {
        let directory = Directory::new();
        let mut engine = engine();
        engine.set_input_language(InputLanguage::Bash);
        let filename = "space 🐟's `report.txt";
        std::fs::write(directory.0.join(filename), "").unwrap();
        let source = "echo 'space 🐟";
        let (start, candidates) = engine.complete(source, source.len(), &directory.0);
        let mut completed = source[..start].to_owned();
        completed.push_str(&candidates[0].replacement);
        let translated =
            crate::bash_to_fish::translate(&fish_widestring::WString::from(completed.as_str()))
                .unwrap();
        assert!(translated.to_string().contains("report.txt"));
        assert_eq!(
            super::super::interactive_bash::literal_word(candidates[0].replacement.trim_end()),
            Some(filename.into())
        );
        let source = "echo \"$(cargo build)\" tail";
        let cursor = source.find("build").unwrap() + 2;
        let (start, candidates) = engine.complete(source, cursor, &directory.0);
        assert_eq!(candidates[0].replacement, "bu");
        let mut edited = source.to_owned();
        edited.replace_range(start..cursor, &candidates[0].replacement);
        assert_eq!(edited, source);
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
        // The shared fuzzy matcher may still find native helper builtins.
        assert!(!replacements(&engine, "native-p", &absent).contains(&"native-probe ".into()));
        assert!(!replacements(&engine, "native-p", &directory.0).contains(&"native-probe ".into()));
        assert!(!engine.command_exists("native-probe", &directory.0));
        assert_eq!(
            replacements(&engine, "./native-p", &directory.0),
            ["./native-probe.exe "]
        );
        assert!(engine.command_exists("./native-probe.exe", &directory.0));
    }

    #[test]
    fn command_inventory_includes_executable_symlink_targets() {
        let directory = Directory::new();
        let target = directory.0.join("native-target.exe");
        let link = directory.0.join("native-link.exe");
        std::fs::write(&target, "").unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
            if error.raw_os_error() == Some(1314) {
                // Windows requires developer mode or the symlink privilege.
                return;
            }
            panic!("cannot create executable symlink: {error}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut engine = engine();
        engine.set_context(
            Vec::new(),
            Some(directory.0.clone().into_os_string()),
            Vec::new(),
        );
        assert!(engine.command_exists("native-link", &directory.0));
        assert_eq!(
            replacements(&engine, "native-l", &directory.0),
            ["native-link "]
        );
        assert_eq!(engine.executable_paths.get("native-link"), Some(&link));
        assert_eq!(
            engine.autoload_commands("native-link ", 12, &directory.0)[0].0,
            "native-link"
        );
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

    fn path_variables(
        values: &[(&str, Vec<String>)],
    ) -> BTreeMap<String, super::super::fish::Variable> {
        values
            .iter()
            .map(|(name, values)| {
                (
                    (*name).into(),
                    super::super::fish::Variable {
                        values: values.clone(),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn variable_directory_completion_preserves_quotes_and_literal_filenames() {
        let directory = Directory::new();
        let filename = "space 🐟's (report) $final.txt";
        std::fs::write(directory.0.join(filename), "").unwrap();
        std::fs::create_dir(directory.0.join("source tree")).unwrap();
        let mut engine = engine();
        engine.set_variable_values(path_variables(&[
            ("base", vec![directory.0.to_string_lossy().into_owned()]),
            ("HOME", vec![directory.0.to_string_lossy().into_owned()]),
        ]));
        for line in [
            "echo $base/spa",
            "echo \"$base/spa",
            "echo \"$base\"/spa",
            "echo $base/'spa",
            "echo $base/sp\"a",
            "echo $base/SPA",
            "echo ~/spa",
        ] {
            let (start, candidates) = engine.complete(line, line.len(), &directory.0);
            assert_eq!(start, 5, "{line}");
            assert_eq!(candidates.len(), 1, "{line}");
            let replacement = candidates[0].replacement.trim_end();
            assert!(
                replacement.contains("$base") || replacement.starts_with("~/"),
                "{replacement}"
            );
            let value =
                super::super::fish::completion_words(replacement, &engine.variable_values, true)
                    .unwrap();
            assert_eq!(
                value,
                [format!("{}/{filename}", directory.0.to_string_lossy())],
                "{line}"
            );
        }
        assert_eq!(
            replacements(&engine, "cd \"$base/so", &directory.0),
            ["\"$base/source tree/"]
        );
        assert_eq!(
            replacements(&engine, "echo ~/so", &directory.0),
            ["~/source\\ tree/"]
        );
        assert!(replacements(&engine, "echo '$base/spa", &directory.0).is_empty());
        assert!(replacements(&engine, "echo \\$base/spa", &directory.0).is_empty());
        engine.set_input_language(InputLanguage::Bash);
        assert!(replacements(&engine, "echo $base/spa", &directory.0).is_empty());
    }

    #[test]
    fn variable_path_lists_slices_and_indirection_use_session_values() {
        let first = Directory::new();
        let second = Directory::new();
        std::fs::write(first.0.join("first.txt"), "").unwrap();
        std::fs::write(second.0.join("final.txt"), "").unwrap();
        for directory in [&first, &second] {
            std::fs::write(directory.0.join("shared.txt"), "").unwrap();
        }
        let mut engine = engine();
        engine.set_variable_values(path_variables(&[
            (
                "roots",
                vec![
                    first.0.to_string_lossy().into_owned(),
                    second.0.to_string_lossy().into_owned(),
                ],
            ),
            ("ref", vec!["roots".into()]),
        ]));
        assert_eq!(
            replacements(&engine, "echo $roots/fi", &first.0),
            ["$roots/final.txt ", "$roots/first.txt "]
        );
        assert_eq!(
            replacements(&engine, "echo $roots[-1]/fi", &first.0),
            ["$roots[-1]/final.txt "]
        );
        assert_eq!(
            replacements(&engine, "echo $$ref/fi", &first.0),
            ["$$ref/final.txt ", "$$ref/first.txt "]
        );
        assert_eq!(
            replacements(&engine, "echo $roots/sh", &first.0),
            ["$roots/shared.txt "]
        );
        assert!(replacements(&engine, "echo \"$roots/fi", &first.0).is_empty());
        assert!(replacements(&engine, "echo $missing/fi", &first.0).is_empty());
    }

    #[test]
    fn variable_paths_preserve_cursor_suffixes_and_obey_file_policy() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("final.txt"), "").unwrap();
        let mut engine = engine();
        engine.set_variable_values(path_variables(&[(
            "base",
            vec![directory.0.to_string_lossy().into_owned()],
        )]));
        for (line, pos, expected) in [
            (
                "echo $base/f.txt | cat",
                "echo $base/f".len(),
                "echo $base/final.txt | cat",
            ),
            (
                "echo \"$base/f.txt\"",
                "echo \"$base/f".len(),
                "echo \"$base/final.txt\"",
            ),
            (
                "echo $base/final.txt",
                "echo $base/f".len(),
                "echo $base/final.txt",
            ),
        ] {
            let (start, candidates) = engine.complete(line, pos, &directory.0);
            assert_eq!(candidates.len(), 1, "{line}");
            assert_eq!(
                format!(
                    "{}{}{}",
                    &line[..start],
                    candidates[0].replacement,
                    &line[pos..]
                ),
                expected
            );
        }
        engine.set_registrations(vec![CompletionSpec {
            command: "probe".into(),
            no_files: true,
            ..Default::default()
        }]);
        assert!(replacements(&engine, "probe $base/fi", &directory.0).is_empty());
        assert_eq!(
            replacements(&engine, "probe > $base/fi", &directory.0),
            ["$base/final.txt "]
        );
        engine.registrations[0].force_files = true;
        assert_eq!(
            replacements(&engine, "probe $base/fi", &directory.0),
            ["$base/final.txt "]
        );
        engine.set_registrations(vec![CompletionSpec {
            command: "probe".into(),
            short_option: Some('I'),
            requires_argument: true,
            ..Default::default()
        }]);
        assert_eq!(
            replacements(&engine, "probe -I$base/fi", &directory.0),
            ["-I$base/final.txt "]
        );
        for line in [
            "echo (echo unsafe)/fi",
            "echo $base/(echo unsafe)/fi",
            "echo $base[1(echo unsafe)]/fi",
            "echo $base/*/fi",
            "echo $base/$name.txt",
        ] {
            assert!(
                replacements(&engine, line, &directory.0).is_empty(),
                "{line}"
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
            ["my-readme.txt ", "released.md ", "roadmap.txt ",]
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
    fn git_routing_requires_literal_preceding_arguments() {
        for (line, fish) in [
            ("git -C (pwd) reset fea", true),
            ("git -C \"$REPO\" reset fea", true),
            ("git -C $(pwd) reset fea", false),
            ("git -C ~/project reset fea", true),
            ("git -C project* reset fea", false),
            ("GIT_DIR=elsewhere git reset fea", false),
            ("git -C (pwd)>log reset fea", true),
        ] {
            assert!(
                !completion_word(line, line.len(), fish).literal_command(),
                "{line}"
            );
        }
        for (line, fish) in [
            ("git -C 'literal repo' reset fea", true),
            ("git -C 'literal$repo' reset fea", false),
            ("git -C \"literal repo\" reset fea", true),
            ("echo (git reset fea", true),
            ("echo $(git reset fea", false),
            ("echo $variable; git reset fea", true),
        ] {
            assert!(
                completion_word(line, line.len(), fish).literal_command(),
                "{line}"
            );
        }
    }

    #[cfg(feature = "git")]
    #[test]
    fn colliding_git_names_still_match_alongside_paths_and_switch_uses_branch_names() {
        let reference = |kind, name: &str| GitRef {
            kind,
            name: name.into(),
        };
        let mut candidates = Candidates::new("top");
        candidates.add("topic-file", "topic-file", "", true);
        add_git_ref(
            &mut candidates,
            reference(GitRefKind::LocalBranch, "refs/heads/topic"),
            RefFilter::All,
            "",
        );
        add_git_ref(
            &mut candidates,
            reference(GitRefKind::Tag, "refs/tags/topic"),
            RefFilter::All,
            "",
        );
        assert_eq!(candidates.finish(false, false).len(), 3);
        let mut candidates = Candidates::new("top");
        add_git_ref(
            &mut candidates,
            reference(GitRefKind::LocalBranch, "refs/heads/topic"),
            RefFilter::LocalBranches,
            "",
        );
        assert_eq!(candidates.finish(false, false)[0].replacement, "topic");
        let mut candidates = Candidates::new("--onto=top");
        add_git_ref(
            &mut candidates,
            reference(GitRefKind::Tag, "refs/tags/topic"),
            RefFilter::All,
            "--onto=",
        );
        assert_eq!(
            candidates.finish(false, false)[0].replacement,
            "--onto=refs/tags/topic"
        );
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
        // Option-specific file policy applies when completing that option's
        // argument; it does not disable the command's positional file choices.
        assert_eq!(
            replacements(&engine, "example ", &directory.0),
            ["unrelated.txt "]
        );
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
