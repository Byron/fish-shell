//! Native prompt editing and shell-specific completion.
//!
//! The editor restores the console mode before returning. Foreground processes
//! and the process host own input and control events while commands are running.

#[cfg(windows)]
use rash::Error;
use rash::Result;
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
    #[cfg(windows)]
    pub(super) fn set_variables(
        &mut self,
        variables: &std::collections::BTreeMap<String, Vec<String>>,
    ) {
        self.inner.set_variables(variables);
    }
    #[cfg(windows)]
    pub(super) fn set_registrations(
        &mut self,
        registrations: Vec<super::interactive_completion::CompletionSpec>,
    ) {
        self.inner.set_registrations(registrations);
    }
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
    pub fn set_completion_context(
        &mut self,
        commands: Vec<String>,
        path: Option<OsString>,
        variables: Vec<String>,
    ) {
        #[cfg(windows)]
        self.inner.set_completion_context(commands, path, variables);
        #[cfg(not(windows))]
        let _ = (commands, path, variables);
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
    use crate::native::interactive_completion::CompletionEngine;
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
        pub(super) fn set_variables(
            &mut self,
            variables: &std::collections::BTreeMap<String, Vec<String>>,
        ) {
            let helper = self.editor.helper_mut().unwrap();
            helper.palette = crate::native::interactive_colors::Palette::from_variables(variables);
            helper.autosuggestion = variables
                .get("fish_autosuggestion_enabled")
                .and_then(|value| value.first())
                .is_none_or(|value| value != "0");
        }
        pub(super) fn set_registrations(
            &mut self,
            registrations: Vec<crate::native::interactive_completion::CompletionSpec>,
        ) {
            self.editor
                .helper_mut()
                .unwrap()
                .engine
                .set_registrations(registrations);
        }
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
            variables: Vec<String>,
        ) {
            let completion = self.editor.helper_mut().unwrap();
            completion.engine.set_context(commands, path, variables);
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
        engine: CompletionEngine,
        palette: crate::native::interactive_colors::Palette,
        autosuggestion: bool,
    }

    impl Completion {
        fn new(language: &str) -> Self {
            Self {
                fish: language == "fish",
                engine: CompletionEngine::new(language),
                palette: Default::default(),
                autosuggestion: true,
            }
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
            Ok(self.engine.complete(line, pos, &std::env::current_dir()?))
        }
    }
    impl Highlighter for Completion {
        fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
            if self.fish {
                std::borrow::Cow::Owned(
                    crate::native::interactive_highlight::highlight_with_palette(
                        line,
                        pos,
                        |name| {
                            self.engine
                                .command_exists(name, &std::env::current_dir().unwrap_or_default())
                        },
                        &self.palette,
                    ),
                )
            } else {
                std::borrow::Cow::Borrowed(line)
            }
        }

        fn highlight_char(&self, _: &str, _: usize, _: bool) -> bool {
            self.fish
        }

        fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
            std::borrow::Cow::Owned(self.palette.highlight_hint(hint))
        }
    }
    impl Hinter for Completion {
        type Hint = String;
        fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
            if !self.fish || !self.autosuggestion || line.starts_with(char::is_whitespace) {
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
