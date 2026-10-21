//! Native prompt editing and shell-specific completion.
//!
//! The editor restores the console mode before returning. Foreground processes
//! and the process host own input and control events while commands are running.

#[cfg(windows)]
use rash::Error;
use rash::Result;
#[cfg(windows)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::PathBuf;

pub enum ReadEvent {
    Line(String),
    Eof,
    Interrupted,
    SuspendRequested,
}

pub struct Editor {
    #[cfg(windows)]
    inner: windows::Editor,
}

impl Editor {
    pub fn new(language: &str, persistent: bool) -> Result<Self> {
        Self::with_history_path(
            language,
            persistent.then(|| history_path(language)).flatten(),
        )
    }

    pub fn with_history_path(language: &str, history: Option<PathBuf>) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::Editor::new(language, history)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (language, history);
            Ok(Self {})
        }
    }

    /// Refresh from the evaluator before reading a command. In particular, PATH
    /// changes made inside the shell must also affect command completion.
    pub fn set_completion_context(&mut self, commands: Vec<String>, path: Option<OsString>) {
        #[cfg(windows)]
        self.inner.set_completion_context(commands, path);
        #[cfg(not(windows))]
        let _ = (commands, path);
    }

    pub fn read_line(&mut self, prompt: &str) -> Result<ReadEvent> {
        #[cfg(windows)]
        return self.inner.read_line(prompt);
        #[cfg(not(windows))]
        {
            use std::io::{self, BufRead as _, Write as _};
            write!(io::stdout(), "{prompt}")?;
            io::stdout().flush()?;
            let mut line = String::new();
            match io::stdin().lock().read_line(&mut line) {
                Ok(0) => Ok(ReadEvent::Eof),
                Ok(_) => Ok(ReadEvent::Line(line.trim_end_matches(['\r', '\n']).into())),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    Ok(ReadEvent::Interrupted)
                }
                Err(error) => Err(error.into()),
            }
        }
    }

    /// Store a completed submission, including multiline source, once per command.
    pub fn add_history(&mut self, source: &str) -> Result<()> {
        #[cfg(windows)]
        return self.inner.add_history(source);
        #[cfg(not(windows))]
        {
            let _ = source;
            Ok(())
        }
    }
}

/// Existing configuration is read by the caller; discovering it never creates
/// or rewrites a configuration file. Bash startup belongs to Rash's Session.
pub fn startup_file(language: &str) -> Option<PathBuf> {
    if language != "fish" {
        return None;
    }
    let directory = absolute_env("XDG_CONFIG_HOME")
        .or_else(|| absolute_env("APPDATA"))
        .or_else(|| home_dir().map(|home| home.join(".config")))?;
    let path = directory.join("fish/config.fish");
    path.is_file().then_some(path)
}

fn history_path(language: &str) -> Option<PathBuf> {
    let filename = match language {
        "fish" => "native_fish_history",
        "rash" => "native_rash_history",
        _ => return None,
    };
    let directory = absolute_env("XDG_DATA_HOME")
        .or_else(|| absolute_env("LOCALAPPDATA"))
        .or_else(|| home_dir().map(|home| home.join(".local/share")))?;
    // These names keep the editor's multiline history format separate from the
    // POSIX Fish history files; neither existing format is imported or replaced.
    Some(directory.join("fish").join(filename))
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn home_dir() -> Option<PathBuf> {
    absolute_env("HOME").or_else(|| absolute_env("USERPROFILE"))
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::native::interactive_keys::{HistoryKey, HistorySearch, SuggestionKey};
    use rustyline::completion::{Completer, Pair};
    use rustyline::error::ReadlineError;
    use rustyline::highlight::Highlighter;
    use rustyline::hint::{Hinter, HistoryHinter};
    use rustyline::history::{DefaultHistory, History, SearchDirection};
    use rustyline::validate::{ValidationContext, ValidationResult, Validator};
    use rustyline::{
        Cmd, CompletionType, ConditionalEventHandler, Config, Context, Event, EventContext,
        EventHandler, Helper, KeyCode, KeyEvent, Modifiers, RepeatCount,
    };
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    pub(super) struct Editor {
        editor: rustyline::Editor<Completion, DefaultHistory>,
        history: Option<PathBuf>,
        suspended: Arc<AtomicBool>,
        search: Arc<Mutex<HistorySearch>>,
    }

    fn readline_error(error: ReadlineError) -> Error {
        Error {
            message: format!("native editor: {error}"),
            status: 1,
        }
    }

    impl Editor {
        pub(super) fn new(language: &str, history: Option<PathBuf>) -> Result<Self> {
            let config = Config::builder()
                .max_history_size(10_000)
                .map_err(readline_error)?
                .history_ignore_dups(true)
                .map_err(readline_error)?
                .history_ignore_space(true)
                .auto_add_history(false)
                .completion_type(CompletionType::List)
                .build();
            let mut editor = rustyline::Editor::with_config(config).map_err(readline_error)?;
            editor.set_helper(Some(Completion::new(language)));
            let suspended = Arc::new(AtomicBool::new(false));
            editor.bind_sequence(
                KeyEvent::ctrl('Z'),
                EventHandler::Conditional(Box::new(SuspendKey(Arc::clone(&suspended)))),
            );
            let search = Arc::new(Mutex::new(HistorySearch::default()));
            if language == "fish" {
                for (key, reverse) in [(KeyCode::Up, true), (KeyCode::Down, false)] {
                    editor.bind_sequence(
                        KeyEvent(key, Modifiers::NONE),
                        EventHandler::Conditional(Box::new(HistoryKey {
                            search: Arc::clone(&search),
                            reverse,
                        })),
                    );
                }
                for key in [
                    KeyEvent(KeyCode::Right, Modifiers::NONE),
                    KeyEvent::ctrl('F'),
                ] {
                    editor.bind_sequence(
                        key,
                        EventHandler::Conditional(Box::new(SuggestionKey { word: false })),
                    );
                }
                for key in [KeyEvent(KeyCode::Right, Modifiers::ALT), KeyEvent::alt('f')] {
                    editor.bind_sequence(
                        key,
                        EventHandler::Conditional(Box::new(SuggestionKey { word: true })),
                    );
                }
                editor.bind_sequence(KeyEvent(KeyCode::Enter, Modifiers::ALT), Cmd::Newline);
                editor.bind_sequence(KeyEvent::ctrl('_'), Cmd::Undo(1));
            }
            if let Some(path) = &history {
                match editor.load_history(path) {
                    Ok(()) => {}
                    Err(ReadlineError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(readline_error(error)),
                }
            }
            Ok(Self {
                editor,
                history,
                suspended,
                search,
            })
        }

        pub(super) fn set_completion_context(
            &mut self,
            commands: Vec<String>,
            path: Option<OsString>,
        ) {
            let completion = self.editor.helper_mut().unwrap();
            completion.commands = commands;
            completion.path = path;
        }

        pub(super) fn read_line(&mut self, prompt: &str) -> Result<ReadEvent> {
            self.suspended.store(false, Ordering::Relaxed);
            let entries = (0..self.editor.history().len())
                .filter_map(|index| {
                    self.editor
                        .history()
                        .get(index, SearchDirection::Forward)
                        .ok()
                        .flatten()
                        .map(|entry| entry.entry.into_owned())
                })
                .collect();
            self.search.lock().unwrap().reset(entries);
            match self.editor.readline(prompt) {
                Ok(line) => Ok(ReadEvent::Line(line)),
                Err(ReadlineError::Eof) => Ok(ReadEvent::Eof),
                Err(ReadlineError::Interrupted) => {
                    if self.suspended.swap(false, Ordering::Relaxed) {
                        Ok(ReadEvent::SuspendRequested)
                    } else {
                        Ok(ReadEvent::Interrupted)
                    }
                }
                Err(error) => Err(readline_error(error)),
            }
        }

        pub(super) fn add_history(&mut self, source: &str) -> Result<()> {
            let source = source.trim_end_matches(['\r', '\n']);
            if !self
                .editor
                .add_history_entry(source)
                .map_err(readline_error)?
            {
                return Ok(());
            }
            if let Some(path) = &self.history {
                if let Some(directory) = path.parent() {
                    std::fs::create_dir_all(directory)?;
                }
                self.editor.append_history(path).map_err(readline_error)?;
            }
            Ok(())
        }
    }

    struct SuspendKey(Arc<AtomicBool>);
    impl ConditionalEventHandler for SuspendKey {
        fn handle(&self, _: &Event, _: RepeatCount, _: bool, _: &EventContext<'_>) -> Option<Cmd> {
            self.0.store(true, Ordering::Relaxed);
            Some(Cmd::Interrupt)
        }
    }

    struct Completion {
        fish: bool,
        builtins: Vec<String>,
        commands: Vec<String>,
        path: Option<OsString>,
    }

    impl Completion {
        fn command_exists(&self, name: &str) -> bool {
            if self
                .builtins
                .iter()
                .chain(&self.commands)
                .any(|command| command == name)
            {
                return true;
            }
            let executable = |path: PathBuf| path.is_file() || path.with_extension("exe").is_file();
            if executable(PathBuf::from(name)) {
                return true;
            }
            self.path.as_ref().is_some_and(|path| {
                std::env::split_paths(path).any(|directory| executable(directory.join(name)))
            })
        }

        fn new(language: &str) -> Self {
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
            Self {
                fish: language == "fish",
                builtins,
                commands: vec![],
                path: std::env::var_os("PATH"),
            }
        }

        fn complete_in(&self, line: &str, pos: usize, cwd: &Path) -> (usize, Vec<Pair>) {
            let word = completion_word(line, pos, self.fish);
            // Completion must not evaluate shell source. Leave variable and
            // command substitutions to the evaluator instead of guessing values.
            if word.value.contains(['$', '`', '\n']) {
                return (word.start, vec![]);
            }
            let value = word.value.replace('\\', "/");
            let value = if let Some(rest) = value.strip_prefix("~/") {
                match home_dir().and_then(|home| home.to_str().map(str::to_owned)) {
                    Some(home) => format!("{}/{rest}", home.replace('\\', "/")),
                    None => value,
                }
            } else {
                value
            };
            let split = value.rfind('/').map_or(0, |at| at + 1);
            let (directory, prefix) = value.split_at(split);
            let path = if directory.is_empty() {
                cwd.to_owned()
            } else {
                cwd.join(directory)
            };
            let mut matches = BTreeSet::new();
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if !prefix_matches(name, prefix)
                        || name.starts_with('.') && !prefix.starts_with('.')
                    {
                        continue;
                    }
                    let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                    if word.command && directory.is_empty() && !is_dir && !executable_name(name) {
                        continue;
                    }
                    matches.insert(format!(
                        "{directory}{name}{}",
                        if is_dir { "/" } else { "" }
                    ));
                }
            }
            if word.command && directory.is_empty() {
                for name in self.builtins.iter().chain(&self.commands) {
                    if prefix_matches(name, prefix) {
                        matches.insert(name.clone());
                    }
                }
                if let Some(path) = &self.path {
                    for directory in std::env::split_paths(path) {
                        let Ok(entries) = std::fs::read_dir(cwd.join(directory)) else {
                            continue;
                        };
                        for entry in entries.flatten() {
                            let name = entry.file_name();
                            let Some(name) = name.to_str() else { continue };
                            if prefix_matches(name, prefix)
                                && executable_name(name)
                                && entry.file_type().is_ok_and(|kind| kind.is_file())
                            {
                                matches.insert(name.into());
                            }
                        }
                    }
                }
            }
            (
                word.start,
                matches
                    .into_iter()
                    .map(|value| Pair {
                        replacement: quote_completion(&value),
                        display: value,
                    })
                    .collect(),
            )
        }
    }

    impl Completer for Completion {
        type Candidate = Pair;
        fn complete(
            &self,
            line: &str,
            pos: usize,
            _: &Context<'_>,
        ) -> rustyline::Result<(usize, Vec<Pair>)> {
            Ok(self.complete_in(line, pos, &std::env::current_dir()?))
        }
    }
    impl Highlighter for Completion {
        fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
            if self.fish {
                std::borrow::Cow::Owned(crate::native::interactive_highlight::highlight(
                    line,
                    pos,
                    |name| self.command_exists(name),
                ))
            } else {
                std::borrow::Cow::Borrowed(line)
            }
        }

        fn highlight_char(&self, _: &str, _: usize, _: bool) -> bool {
            self.fish
        }

        fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
            std::borrow::Cow::Owned(crate::native::interactive_highlight::highlight_hint(hint))
        }
    }
    impl Hinter for Completion {
        type Hint = String;
        fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
            if !self.fish || line.starts_with(char::is_whitespace) {
                return None;
            }
            HistoryHinter::new().hint(line, pos, ctx)
        }
    }
    impl Validator for Completion {
        fn validate(&self, ctx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
            Ok(
                if self.fish && crate::native::fish::is_incomplete(ctx.input()) {
                    ValidationResult::Incomplete
                } else {
                    ValidationResult::Valid(None)
                },
            )
        }
    }
    impl Helper for Completion {}

    fn prefix_matches(name: &str, prefix: &str) -> bool {
        name.to_lowercase().starts_with(&prefix.to_lowercase())
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

    struct Word {
        start: usize,
        value: String,
        command: bool,
    }

    fn completion_word(line: &str, pos: usize, fish: bool) -> Word {
        let mut start = pos;
        let mut value = String::new();
        let mut quote = None;
        let mut command = true;
        let mut active = false;
        let mut chars = line[..pos].char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if !active {
                start = at;
            }
            if c == '\\' && quote != Some('\'') {
                active = true;
                if let Some((_, next)) = chars.next() {
                    if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                        value.push('\\');
                    }
                    value.push(next);
                }
            } else if c == '\\'
                && fish
                && quote == Some('\'')
                && chars
                    .peek()
                    .is_some_and(|(_, next)| matches!(next, '\\' | '\''))
            {
                active = true;
                value.push(chars.next().unwrap().1);
            } else if Some(c) == quote {
                quote = None;
            } else if quote.is_none() && matches!(c, '\'' | '"') {
                active = true;
                quote = Some(c);
            } else if quote.is_none() && matches!(c, ';' | '|' | '&' | '\n') {
                command = true;
                active = false;
                value.clear();
            } else if quote.is_none() && c.is_whitespace() {
                if active {
                    command = command
                        && (matches!(
                            value.as_str(),
                            "command"
                                | "builtin"
                                | "exec"
                                | "not"
                                | "and"
                                | "or"
                                | "if"
                                | "while"
                                | "else"
                        ) || value.contains('='));
                }
                active = false;
                value.clear();
            } else {
                active = true;
                value.push(c);
            }
        }
        Word {
            start: if active { start } else { pos },
            value,
            command,
        }
    }

    fn quote_completion(value: &str) -> String {
        if value
            .chars()
            .all(|c| c.is_alphanumeric() || "-._/:".contains(c))
        {
            value.into()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct Directory(PathBuf);
        impl Directory {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "fish-editor-{}-{}",
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

        #[test]
        fn completion_uses_quoted_unicode_paths_and_updated_command_path() {
            let directory = Directory::new();
            std::fs::write(directory.0.join("space 🐟's.txt"), b"").unwrap();
            std::fs::write(directory.0.join("native-probe.exe"), b"").unwrap();
            let mut helper = Completion::new("fish");
            helper.path = Some(directory.0.clone().into_os_string());
            let (start, paths) = helper.complete_in("echo 'spa", 9, &directory.0);
            assert_eq!(start, 5);
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].replacement, "'space 🐟'\\''s.txt'");
            let (start, commands) =
                helper.complete_in("echo value; native-p", 20, &directory.0.join("missing"));
            assert_eq!(start, 12);
            assert_eq!(
                commands
                    .iter()
                    .map(|candidate| candidate.display.as_str())
                    .collect::<Vec<_>>(),
                ["native-probe.exe"]
            );
            helper.path = None;
            assert!(
                helper
                    .complete_in("native-p", 8, &directory.0.join("missing"))
                    .1
                    .is_empty()
            );
        }

        #[test]
        fn history_append_preserves_other_sessions_and_multiline_entries() {
            let directory = Directory::new();
            let history = directory.0.join("history");
            let mut first = Editor::new("fish", Some(history.clone())).unwrap();
            let mut second = Editor::new("fish", Some(history.clone())).unwrap();
            first.add_history("echo first").unwrap();
            second.add_history("begin\necho 🐟\nend").unwrap();
            first.add_history("echo last").unwrap();
            let contents = std::fs::read_to_string(history).unwrap();
            assert!(contents.contains("echo first\n"));
            assert!(contents.contains("begin\\necho 🐟\\nend\n"));
            assert!(contents.contains("echo last\n"));
        }
    }
}
