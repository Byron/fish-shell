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
    pub(super) fn set_input_config(
        &mut self,
        abbreviations: super::interactive_abbreviations::Abbreviations,
        bindings: &super::interactive_bindings::Bindings,
    ) {
        self.inner.set_input_config(abbreviations, bindings);
    }
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
    use crate::native::interactive_abbreviations::Abbreviations;
    use crate::native::interactive_bindings::{Action, Bindings};
    use crate::native::interactive_completion::CompletionEngine;
    use crate::native::interactive_keys::{HistoryKey, HistorySearch, SuggestionKey};
    use crate::native::interactive_pager::{EditorHint, PagerBindings};
    use rustyline::completion::{Completer, Pair};
    use rustyline::config::Configurer as _;
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
        custom_bindings: Vec<(KeyEvent, Option<EventHandler>)>,
    }

    fn readline_error(error: ReadlineError) -> Error {
        Error {
            message: format!("native editor: {error}"),
            status: 1,
        }
    }

    impl Editor {
        pub(super) fn set_input_config(
            &mut self,
            abbreviations: Abbreviations,
            bindings: &Bindings,
        ) {
            *self.editor.helper().unwrap().abbreviations.lock().unwrap() = abbreviations;
            for (key, previous) in self.custom_bindings.drain(..) {
                if let Some(handler) = previous {
                    self.editor.bind_sequence(key, handler);
                } else {
                    self.editor.unbind_sequence(key);
                }
            }
            let helper = self.editor.helper().unwrap();
            let pager = helper.pager.clone();
            let abbreviations = Arc::clone(&helper.abbreviations);
            for (key, action) in bindings.entries() {
                let handler = UserAction {
                    action,
                    pager: pager.clone(),
                    abbreviations: Arc::clone(&abbreviations),
                    search: Arc::clone(&self.search),
                };
                let previous = self.editor.bind_sequence(
                    key,
                    pager.wrap_user(EventHandler::Conditional(Box::new(handler))),
                );
                self.custom_bindings.push((key, previous));
            }
        }
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
            self.editor
                .set_edit_mode(crate::native::interactive_abbreviations::edit_mode(
                    variables,
                ));
            let escape_delay = variables
                .get("fish_escape_delay_ms")
                .and_then(|values| values.first())
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|delay| (10..5000).contains(delay))
                .unwrap_or(30);
            self.editor.set_keyseq_timeout(Some(escape_delay));
        }
        pub(super) fn set_registrations(
            &mut self,
            registrations: Vec<crate::native::interactive_completion::CompletionSpec>,
        ) {
            self.editor
                .helper_mut()
                .unwrap()
                .engine
                .lock()
                .unwrap()
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
            let pager = editor.helper().unwrap().pager.clone();
            let abbreviations = Arc::clone(&editor.helper().unwrap().abbreviations);
            let suspended = Arc::new(AtomicBool::new(false));
            editor.bind_sequence(
                KeyEvent::ctrl('Z'),
                if language == "fish" {
                    pager.wrap(Some(EventHandler::Conditional(Box::new(SuspendKey(
                        Arc::clone(&suspended),
                    )))))
                } else {
                    EventHandler::Conditional(Box::new(SuspendKey(Arc::clone(&suspended))))
                },
            );
            let search = Arc::new(Mutex::new(HistorySearch::default()));
            if language == "fish" {
                editor.bind_sequence(
                    KeyEvent(KeyCode::Char(' '), Modifiers::NONE),
                    pager.wrap(Some(EventHandler::Conditional(Box::new(InputKey {
                        abbreviations: Arc::clone(&abbreviations),
                        pager: pager.clone(),
                        space: true,
                    })))),
                );
                editor.bind_sequence(Event::Any, pager.wrap(None));
                editor.bind_sequence(KeyEvent(KeyCode::Tab, Modifiers::NONE), pager.wrap(None));
                editor.bind_sequence(
                    KeyEvent(KeyCode::BackTab, Modifiers::NONE),
                    pager.wrap(None),
                );
                for (key, reverse) in [(KeyCode::Up, true), (KeyCode::Down, false)] {
                    editor.bind_sequence(
                        KeyEvent(key, Modifiers::NONE),
                        pager.wrap(Some(EventHandler::Conditional(Box::new(HistoryKey {
                            search: Arc::clone(&search),
                            reverse,
                        })))),
                    );
                }
                for key in [
                    KeyEvent(KeyCode::Right, Modifiers::NONE),
                    KeyEvent::ctrl('F'),
                ] {
                    editor.bind_sequence(
                        key,
                        pager.wrap(Some(EventHandler::Conditional(Box::new(SuggestionKey {
                            word: false,
                            pager: pager.clone(),
                        })))),
                    );
                }
                for key in [KeyEvent(KeyCode::Right, Modifiers::ALT), KeyEvent::alt('f')] {
                    editor.bind_sequence(
                        key,
                        pager.wrap(Some(EventHandler::Conditional(Box::new(SuggestionKey {
                            word: true,
                            pager: pager.clone(),
                        })))),
                    );
                }
                editor.bind_sequence(
                    KeyEvent(KeyCode::Enter, Modifiers::ALT),
                    pager.wrap(Some(EventHandler::Conditional(Box::new(
                        crate::native::interactive_layout::EnterKey {
                            force_newline: true,
                            pager: pager.clone(),
                        },
                    )))),
                );
                for key in [
                    KeyEvent(KeyCode::Enter, Modifiers::NONE),
                    KeyEvent::ctrl('J'),
                ] {
                    editor.bind_sequence(
                        key,
                        pager.wrap(Some(EventHandler::Conditional(Box::new(InputKey {
                            abbreviations: Arc::clone(&abbreviations),
                            pager: pager.clone(),
                            space: false,
                        })))),
                    );
                }
                editor.bind_sequence(
                    KeyEvent(KeyCode::Insert, Modifiers::SHIFT),
                    pager.wrap(Some(Cmd::PasteFromClipboard.into())),
                );
                editor.bind_sequence(KeyEvent::ctrl('_'), pager.wrap(Some(Cmd::Undo(1).into())));
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
                custom_bindings: Vec::new(),
            })
        }

        pub(super) fn set_completion_context(
            &mut self,
            commands: Vec<String>,
            path: Option<OsString>,
            variables: Vec<String>,
        ) {
            let completion = self.editor.helper_mut().unwrap();
            completion
                .engine
                .lock()
                .unwrap()
                .set_context(commands, path, variables);
        }

        pub(super) fn read_line(&mut self, prompt: &str) -> Result<ReadEvent> {
            self.suspended.store(false, Ordering::Relaxed);
            self.editor.helper().unwrap().pager.reset();
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

    struct InputKey {
        abbreviations: Arc<Mutex<Abbreviations>>,
        pager: PagerBindings,
        space: bool,
    }

    impl ConditionalEventHandler for InputKey {
        fn handle(
            &self,
            _: &Event,
            _: RepeatCount,
            _: bool,
            ctx: &EventContext<'_>,
        ) -> Option<Cmd> {
            if self.space
                && ctx.mode() == rustyline::EditMode::Vi
                && ctx.input_mode() != rustyline::InputMode::Insert
            {
                return None;
            }
            if let Some(mut expansion) = self
                .abbreviations
                .lock()
                .ok()?
                .expansion(ctx.line(), ctx.pos())
            {
                if self.space {
                    expansion.text.push(' ');
                }
                return Some(self.pager.queue_completion(
                    ctx.line(),
                    ctx.pos(),
                    expansion.range.start,
                    expansion.text,
                ));
            }
            if self.space {
                // Let the native keymap self-insert an ordinary Space. Its
                // Cmd::Insert path uses vi paste positioning even in insert
                // mode, placing subsequent typing before that space.
                None
            } else {
                Some(crate::native::interactive_layout::enter_command(
                    &self.pager,
                    ctx.line(),
                    ctx.pos(),
                    false,
                ))
            }
        }
    }

    struct UserAction {
        action: Action,
        pager: PagerBindings,
        abbreviations: Arc<Mutex<Abbreviations>>,
        search: Arc<Mutex<HistorySearch>>,
    }

    impl ConditionalEventHandler for UserAction {
        fn handle(
            &self,
            event: &Event,
            count: RepeatCount,
            positive: bool,
            ctx: &EventContext<'_>,
        ) -> Option<Cmd> {
            match self.action {
                Action::AcceptAutosuggestion => Some(if ctx.has_hint() {
                    Cmd::CompleteHint
                } else {
                    Cmd::Noop
                }),
                Action::ForwardChar | Action::ForwardWord => Some(
                    SuggestionKey {
                        word: self.action == Action::ForwardWord,
                        pager: self.pager.clone(),
                    }
                    .command(count, ctx),
                ),
                Action::HistorySearchBackward | Action::HistorySearchForward => HistoryKey {
                    search: Arc::clone(&self.search),
                    reverse: self.action == Action::HistorySearchBackward,
                }
                .command(ctx.line()),
                Action::Execute => InputKey {
                    abbreviations: Arc::clone(&self.abbreviations),
                    pager: self.pager.clone(),
                    space: false,
                }
                .handle(event, count, positive, ctx),
                Action::Complete => self
                    .pager
                    .handle(&KeyEvent(KeyCode::Tab, Modifiers::NONE).into(), ctx),
                _ => self.action.command(),
            }
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
        engine: Arc<Mutex<CompletionEngine>>,
        pager: PagerBindings,
        abbreviations: Arc<Mutex<Abbreviations>>,
        palette: crate::native::interactive_colors::Palette,
        autosuggestion: bool,
    }

    impl Completion {
        fn new(language: &str) -> Self {
            let engine = Arc::new(Mutex::new(CompletionEngine::new(language)));
            let pager = PagerBindings::new(Arc::clone(&engine));
            Self {
                fish: language == "fish",
                engine,
                pager,
                abbreviations: Arc::new(Mutex::new(Abbreviations::default())),
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
            if let Some(completion) = self.pager.take_completion(line, pos) {
                return Ok(completion);
            }
            Ok(self
                .engine
                .lock()
                .unwrap()
                .complete(line, pos, &std::env::current_dir()?))
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
                            self.abbreviations.lock().unwrap().contains(name)
                                || self.engine.lock().unwrap().command_exists(
                                    name,
                                    &std::env::current_dir().unwrap_or_default(),
                                )
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
            if let Some(hint) = self.pager.highlight_hint(hint) {
                return hint;
            }
            std::borrow::Cow::Owned(self.palette.highlight_hint(hint))
        }
    }
    impl Hinter for Completion {
        type Hint = EditorHint;
        fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<EditorHint> {
            if let Some(hint) = self.pager.hint(line, pos) {
                return Some(hint);
            }
            if !self.fish || !self.autosuggestion || line.starts_with(char::is_whitespace) {
                return None;
            }
            HistoryHinter::new()
                .hint(line, pos, ctx)
                .map(EditorHint::suggestion)
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
