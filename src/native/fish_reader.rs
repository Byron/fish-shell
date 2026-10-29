//! Editing callbacks borrow a snapshot of the live reader, never its terminal.
use super::*;
use crate::commandline_extent::{get_job_extent, get_process_extent, get_token_extent};
use crate::native::interactive_bindings::Action;
use crate::native::interactive_completion::CompletionContext;

#[derive(Clone, Copy, Default)]
pub(in crate::native) struct ReaderModes {
    pub search: bool,
    pub paging: bool,
    pub paging_full: bool,
}

#[derive(Clone)]
pub(in crate::native) struct ReaderState {
    pub(in crate::native) line: String,
    pub(in crate::native) cursor: usize,
    pub(in crate::native) actions: Vec<Action>,
    pub(in crate::native) modes: ReaderModes,
}

impl Session {
    pub(in crate::native) fn initialize_reader_bindings(
        &mut self,
        host: &mut impl Host,
    ) -> Result<()> {
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let result = (|| {
            if self.ensure_function("fish_user_key_bindings", host, &mut Output::default())? {
                self.eval_source("fish_user_key_bindings", host, &mut Output::default())?;
            }
            Ok(())
        })();
        if !self.exit_requested() {
            self.status = status;
            self.pipeline = pipeline;
        }
        result
    }

    pub(in crate::native) fn transient_prompt_enabled(&self) -> bool {
        self.values("fish_transient_prompt")
            .first()
            .is_some_and(|value| value == "1")
    }

    pub(in crate::native) fn render_reader_prompt(
        &mut self,
        name: &str,
        final_rendering: bool,
        host: &mut impl Host,
    ) -> Result<Option<String>> {
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let blocked = std::mem::replace(&mut self.input_mode_blocked, true);
        let mut output = Output {
            capture: true,
            ..Default::default()
        };
        let result = (|| {
            if !self.ensure_function(name, host, &mut Output::default())? {
                return Ok(None);
            }
            let command = if final_rendering {
                format!("{name} --final-rendering")
            } else {
                name.into()
            };
            self.eval_source(&command, host, &mut output)?;
            Ok(Some(
                String::from_utf8_lossy(&output.bytes.newline_serialized()).into_owned(),
            ))
        })();
        self.input_mode_blocked = blocked;
        if !self.exit_requested() {
            self.status = status;
            self.pipeline = pipeline;
        }
        result
    }

    pub(in crate::native) fn reader_abbreviation(
        &mut self,
        line: &str,
        cursor: usize,
        host: &mut impl Host,
    ) -> Result<Option<crate::native::interactive_abbreviations::Expansion>> {
        let candidates = self.abbreviations.candidates(line, cursor);
        let saved_context = self.completion_line.replace(CompletionContext {
            line: line.into(),
            cursor,
        });
        let saved_interactive = std::mem::replace(&mut self.interactive, false);
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let result = (|| {
            for candidate in candidates {
                if !candidate.function {
                    return Ok(Some(candidate));
                }
                let quote =
                    |value: &str| format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
                let words = self.expand(
                    &format!("({} {})", quote(&candidate.text), quote(&candidate.token)),
                    host,
                )?;
                if host.cancelled() {
                    return Ok(None);
                }
                if self.status == 0 {
                    let text = words.join("\n");
                    if text.len() > 1024 * 1024 || text.contains('\0') {
                        return Err(Error::new(
                            "abbr: generated expansion exceeds the native reader limit or contains a null character",
                        ));
                    }
                    return Ok(Some(candidate.finish(text)));
                }
            }
            Ok(None)
        })();
        self.completion_line = saved_context;
        self.interactive = saved_interactive;
        self.status = status;
        self.pipeline = pipeline;
        result
    }

    pub(in crate::native) fn reader_callback(
        &mut self,
        sources: &[String],
        line: &str,
        cursor: usize,
        modes: ReaderModes,
        host: &mut impl Host,
    ) -> Result<ReaderState> {
        let saved_context = self.completion_line.take();
        let saved_status = self.status;
        let saved_pipeline = self.pipeline.clone();
        let saved_blocked = std::mem::replace(&mut self.input_mode_blocked, true);
        self.completion_line = Some(CompletionContext {
            line: line.into(),
            cursor,
        });
        self.reader_state = Some(ReaderState {
            line: line.into(),
            cursor,
            actions: Vec::new(),
            modes,
        });
        let result = (|| {
            for source in sources {
                if let Ok(action) = Action::parse(source) {
                    self.reader_state.as_mut().unwrap().actions.push(action);
                } else {
                    self.eval_source(source, host, &mut Output::default())?;
                }
                if self.exit_requested() || host.cancelled() {
                    break;
                }
            }
            Ok(())
        })();
        let reader = self.reader_state.take().unwrap();
        self.completion_line = saved_context;
        if !self.exit_requested() {
            self.status = saved_status;
            self.pipeline = saved_pipeline;
        }
        self.input_mode_blocked = saved_blocked;
        result.map(|()| reader)
    }

    /// Return false for read-only queries, which use the shared completion path.
    pub(super) fn reader_commandline(&mut self, args: &[String]) -> Result<bool> {
        let mut part = 'b';
        let mut operation = 'r';
        let mut cursor = false;
        let mut functions = false;
        let mut words = Vec::new();
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            index += 1;
            let flags = match arg.as_str() {
                "--current-buffer" => "b",
                "--current-job" => "j",
                "--current-process" => "p",
                "--current-token" => "t",
                "--replace" => "r",
                "--insert" => "i",
                "--append" => "a",
                "--cursor" => "C",
                "--function" => "f",
                "--" => {
                    words.extend_from_slice(&args[index..]);
                    break;
                }
                _ if arg.starts_with('-')
                    && arg.len() > 1
                    && !arg.starts_with("--")
                    && arg[1..].parse::<i64>().is_err() =>
                {
                    &arg[1..]
                }
                _ if arg.starts_with("--") => return Ok(false),
                _ => {
                    words.extend_from_slice(&args[index - 1..]);
                    break;
                }
            };
            for flag in flags.chars() {
                match flag {
                    'b' | 'j' | 'p' | 't' => part = flag,
                    'r' | 'i' | 'a' => operation = flag,
                    'C' => cursor = true,
                    'f' => functions = true,
                    _ => return Ok(false),
                }
            }
        }
        if words.is_empty() && !functions {
            return Ok(false);
        }
        if self.completion_depth != 0 || self.substitution_depth != 0 {
            return Err(Error::new(
                "commandline: editing is unavailable in completion generators and command substitutions",
            ));
        }
        let state = self
            .reader_state
            .as_mut()
            .ok_or_else(|| Error::new("commandline: no interactive reader is available"))?;
        if functions {
            if words.is_empty() {
                return Err(Error::new(
                    "commandline: --function requires an input function",
                ));
            }
            let actions = words
                .iter()
                .map(|word| Action::parse(word).map_err(Error::new))
                .collect::<Result<Vec<_>>>()?;
            state.actions.extend(actions);
        } else {
            let source = WString::from(state.line.as_str());
            let position = state.line[..state.cursor].chars().count();
            let range = match part {
                'j' => get_job_extent(&source, position, None),
                'p' => get_process_extent(&source, position, None),
                't' => get_token_extent(&source, position).0,
                _ => 0..source.len(),
            };
            let byte = |at: usize| {
                state
                    .line
                    .char_indices()
                    .nth(at)
                    .map_or(state.line.len(), |(at, _)| at)
            };
            if cursor {
                if words.len() != 1 {
                    return Err(Error::new("commandline: --cursor accepts one position"));
                }
                let offset = words[0]
                    .parse::<i64>()
                    .map_err(|_| Error::new("commandline: invalid cursor position"))?;
                let position = range
                    .start
                    .saturating_add(offset.max(0) as usize)
                    .min(range.end);
                state.cursor = byte(position);
            } else {
                let replacement = words.join("\n");
                if state.line.len().saturating_add(replacement.len()) > 1024 * 1024 {
                    return Err(Error::new(
                        "commandline: input exceeds the native reader size limit",
                    ));
                }
                let range = byte(range.start)..byte(range.end);
                let replace = match operation {
                    'i' => state.cursor..state.cursor,
                    'a' => range.end..range.end,
                    _ => range,
                };
                state.line.replace_range(replace.clone(), &replacement);
                state.cursor = if operation == 'a' {
                    state.cursor
                } else {
                    replace.start + replacement.len()
                };
            }
            self.completion_line = Some(CompletionContext {
                line: state.line.clone(),
                cursor: state.cursor,
            });
        }
        self.status = 0;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::{ReadResult, RunResult};

    #[derive(Default)]
    struct Capture(Vec<u8>);
    impl Host for Capture {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            Err(Error::new("unexpected external command"))
        }
        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            unreachable!()
        }
        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            unreachable!()
        }
        fn write(
            &mut self,
            _: i32,
            bytes: &[u8],
            _: &[Redirection],
            capture: bool,
        ) -> Result<Vec<u8>> {
            if capture {
                Ok(bytes.to_vec())
            } else {
                self.0.extend(bytes);
                Ok(Vec::new())
            }
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    #[test]
    fn shared_arrow_helpers_observe_reader_modes_and_multiline_position() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session
            .eval(
                include_str!("../../share/functions/up-or-search.fish"),
                &mut host,
            )
            .unwrap();
        session
            .eval(
                include_str!("../../share/functions/down-or-search.fish"),
                &mut host,
            )
            .unwrap();
        for (source, cursor, modes, action) in [
            (
                "up-or-search",
                0,
                ReaderModes::default(),
                Action::HistorySearchBackward,
            ),
            ("up-or-search", 8, ReaderModes::default(), Action::UpLine),
            (
                "down-or-search",
                0,
                ReaderModes::default(),
                Action::DownLine,
            ),
            (
                "down-or-search",
                8,
                ReaderModes::default(),
                Action::HistorySearchForward,
            ),
            (
                "up-or-search",
                8,
                ReaderModes {
                    search: true,
                    ..Default::default()
                },
                Action::HistorySearchBackward,
            ),
            (
                "down-or-search",
                0,
                ReaderModes {
                    search: true,
                    ..Default::default()
                },
                Action::HistorySearchForward,
            ),
            (
                "up-or-search",
                0,
                ReaderModes {
                    paging: true,
                    ..Default::default()
                },
                Action::UpLine,
            ),
        ] {
            let state = session
                .reader_callback(&[source.into()], "echo\nfoo", cursor, modes, &mut host)
                .unwrap();
            assert_eq!(state.actions, [action], "{source}, cursor={cursor}");
        }
        assert!(host.0.is_empty());
        session.eval("commandline --search-mode; echo $status; commandline -P; echo $status; commandline -F; echo $status", &mut host).unwrap();
        assert_eq!(host.0, b"1\n1\n1\n");
    }

    #[test]
    fn callback_edits_unicode_token_preserves_suffix_and_session_changes() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        let line = "echo 🐟 old";
        let cursor = "echo 🐟".len();
        let state = session.reader_callback(&["set -g callback_seen (commandline -ct); commandline -rt 🦀; commandline -C 5; commandline -i fish; commandline -f repaint".into()], line, cursor, ReaderModes::default(), &mut host).unwrap();
        assert_eq!(state.line, "echo fish🦀 old");
        assert_eq!(state.cursor, 9);
        assert_eq!(session.values("callback_seen"), ["🐟"]);
        assert_eq!(state.actions, [Action::Repaint]);
        assert!(session.completion_line.is_none());
        assert!(session.reader_state.is_none());
    }

    #[test]
    fn append_preserves_cursor_and_queued_functions_preserve_order() {
        let mut session = Session::new().unwrap();
        let state = session
            .reader_callback(
                &[
                    "commandline -a tail; commandline -f backward-char repaint".into(),
                    "execute".into(),
                ],
                "echo ",
                2,
                ReaderModes::default(),
                &mut Capture::default(),
            )
            .unwrap();
        assert_eq!(state.line, "echo tail");
        assert_eq!(state.cursor, 2);
        assert_eq!(
            state.actions,
            [Action::BackwardChar, Action::Repaint, Action::Execute]
        );
    }

    #[test]
    fn callback_queries_observe_edits_and_do_not_replace_command_status() {
        let mut session = Session::new().unwrap();
        session.status = 42;
        let mut host = Capture::default();
        session
            .reader_callback(
                &[
                    "commandline -r 'echo changed'; set -g callback_line (commandline); false"
                        .into(),
                ],
                "old",
                3,
                ReaderModes::default(),
                &mut host,
            )
            .unwrap();
        assert_eq!(session.values("callback_line"), ["echo changed"]);
        assert_eq!(session.status, 42);
    }

    #[test]
    fn function_abbreviation_receives_token_and_falls_back_after_failure() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("function make_abbr; set -g observed_token $argv[1]; printf 'echo %s' $argv[1]; end; function deny_abbr; return 1; end; abbr --add --regex 'g[0-9]+' --function make_abbr generated; abbr --add --regex 'g[0-9]+' --function deny_abbr declined", &mut host).unwrap();
        let expansion = session
            .reader_abbreviation("g42", 3, &mut host)
            .unwrap()
            .unwrap();
        assert_eq!(expansion.text, "echo g42");
        assert_eq!(session.values("observed_token"), ["g42"]);
        assert_eq!(session.status, 0);
        assert!(session.completion_line.is_none());
    }

    #[test]
    fn nested_completion_cannot_edit_the_readers_buffer() {
        let mut session = Session::new().unwrap();
        session.reader_state = Some(ReaderState {
            line: "original".into(),
            cursor: 8,
            actions: Vec::new(),
            modes: ReaderModes::default(),
        });
        session.completion_depth = 1;
        assert!(
            session
                .reader_commandline(&["-r".into(), "bad".into()])
                .is_err()
        );
        assert_eq!(session.reader_state.unwrap().line, "original");
    }

    #[test]
    fn prompts_receive_final_rendering_and_preserve_previous_status() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session
            .eval(
                "function fish_prompt; printf '%s|%s' $status (string join , -- $argv); false; end",
                &mut host,
            )
            .unwrap();
        session.status = 42;
        session.pipeline = vec![3, 42];
        assert_eq!(
            session
                .render_reader_prompt("fish_prompt", true, &mut host)
                .unwrap()
                .as_deref(),
            Some("42|--final-rendering")
        );
        assert_eq!(session.status, 42);
        assert_eq!(session.pipeline, [3, 42]);
        assert!(!session.input_mode_blocked);
        assert!(!session.transient_prompt_enabled());
        session
            .eval("set -g fish_transient_prompt 1", &mut host)
            .unwrap();
        assert!(session.transient_prompt_enabled());
    }

    #[test]
    fn startup_bindings_and_prompt_exit_keep_their_session_effects() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("function fish_user_key_bindings; set -g hook_seen yes; bind ctrl-g 'commandline -i hook'; false; end", &mut host).unwrap();
        session.status = 17;
        session.pipeline = vec![0, 17];
        session.initialize_reader_bindings(&mut host).unwrap();
        assert_eq!(session.values("hook_seen"), ["yes"]);
        assert_eq!(session.status, 17);
        assert_eq!(session.pipeline, [0, 17]);
        session
            .eval("function fish_right_prompt; exit 23; end", &mut host)
            .unwrap();
        session
            .render_reader_prompt("fish_right_prompt", false, &mut host)
            .unwrap();
        assert!(session.exit_requested());
        assert_eq!(session.status, 23);
    }
}
