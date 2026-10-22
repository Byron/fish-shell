//! Native prompt variables and lifecycle events around outer reader activity.
use super::*;
use std::time::Duration;

impl Session {
    pub(super) fn initialize_runtime(&mut self) {
        for (name, value) in [
            ("version", crate::BUILD_VERSION),
            ("FISH_VERSION", crate::BUILD_VERSION),
            ("CMD_DURATION", "0"),
        ] {
            self.variables.insert(
                name.into(),
                Variable {
                    values: vec![value.into()],
                    ..Default::default()
                },
            );
        }
        let (columns, lines) = super::super::interactive_pager::console_size().unwrap_or((80, 24));
        for (name, size) in [("COLUMNS", columns), ("LINES", lines)] {
            let valid = self.variables.get(name).is_some_and(|variable| {
                variable.values.len() == 1
                    && variable.values[0]
                        .parse::<usize>()
                        .is_ok_and(|value| value > 0)
            });
            if !valid {
                self.variables.insert(
                    name.into(),
                    Variable {
                        values: vec![size.to_string()],
                        ..Default::default()
                    },
                );
            }
        }
    }

    pub(super) fn refresh_terminal_size(&mut self, host: &mut impl Host) -> Result<()> {
        let Some((columns, lines)) = super::super::interactive_pager::console_size() else {
            return Ok(());
        };
        self.update_terminal_size(columns, lines, host)
    }

    fn update_terminal_size(
        &mut self,
        columns: usize,
        lines: usize,
        host: &mut impl Host,
    ) -> Result<()> {
        for (name, size) in [("COLUMNS", columns), ("LINES", lines)] {
            let value = size.to_string();
            if self.values(name) == [value.clone()] {
                continue;
            }
            let variable = self.variables.entry(name.into()).or_default();
            variable.values = vec![value];
            variable.pathvar = false;
            self.fire_variable_event(name, false, host)?;
        }
        Ok(())
    }

    /// Post-execution and shutdown events still run when an outer command has
    /// requested exit. Preserve that request while retaining normal event rules.
    pub(in crate::native) fn lifecycle_event(
        &mut self,
        name: &str,
        arguments: &[String],
        host: &mut impl Host,
    ) -> Result<()> {
        let pending = std::mem::replace(&mut self.flow, Flow::Normal);
        let result = self.fire_event(&events::Description::Generic(name.into()), arguments, host);
        if pending != Flow::Normal {
            self.flow = pending;
        }
        result
    }

    pub(in crate::native) fn command_finished(
        &mut self,
        command: &str,
        elapsed: Duration,
        host: &mut impl Host,
    ) -> Result<()> {
        // Like the shared reader, whitespace and comment-only submissions fire
        // command events but leave the previous command's duration intact.
        if !crate::tokenizer::tok_command(&WString::from(command)).is_empty() {
            self.variables.insert(
                "CMD_DURATION".into(),
                Variable {
                    values: vec![elapsed.as_millis().to_string()],
                    ..Default::default()
                },
            );
            let pending = std::mem::replace(&mut self.flow, Flow::Normal);
            let result = self.fire_variable_event("CMD_DURATION", false, host);
            if pending != Flow::Normal {
                self.flow = pending;
            }
            result?;
        }
        self.lifecycle_event("fish_postexec", &[command.into()], host)
    }

    pub(in crate::native) fn finish_runtime(
        &mut self,
        status: i32,
        reader_started: bool,
        failed: bool,
        host: &mut impl Host,
    ) -> Result<()> {
        self.status = status;
        if failed {
            // Source-loading failures can occur before the evaluator has had
            // a chance to replace a preceding configuration pipeline's status.
            self.pipeline = vec![status];
        }
        let arguments = if reader_started {
            vec![]
        } else {
            vec![status.to_string()]
        };
        self.lifecycle_event("fish_exit", &arguments, host)
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
            Err(Error::new("unexpected external runtime test command"))
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
                self.0.extend_from_slice(bytes);
                Ok(vec![])
            }
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            vec![]
        }
        fn cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn prompt_events_can_define_the_prompt_and_preserve_status() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("function setup --on-event fish_prompt; function fish_prompt; echo prompt:$status; end; end; false", &mut host).unwrap();
        assert_eq!(
            session.prompt(&mut host).unwrap().as_deref(),
            Some("prompt:1\n")
        );
        assert_eq!(session.status, 1);
        assert_eq!(session.pipeline, [1]);
        assert!(host.0.is_empty());
    }

    #[test]
    fn postexec_runs_after_duration_handlers_even_with_exit_pending() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session
            .eval(
                r#"
            function duration --on-variable CMD_DURATION; echo duration:$CMD_DURATION:$status; end
            function post --on-event fish_postexec; echo post:$CMD_DURATION:$status:$argv; end
            exit 7
        "#,
                &mut host,
            )
            .unwrap();
        session
            .command_finished("exit 7", Duration::from_millis(123), &mut host)
            .unwrap();
        assert_eq!(host.0, b"duration:123:7\npost:123:7:exit 7\n");
        assert!(session.flow == Flow::Exit);
        assert_eq!(session.status, 7);
        assert_eq!(session.pipeline, [7]);
    }

    #[test]
    fn comment_and_whitespace_events_keep_the_previous_duration() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("set -g CMD_DURATION 999; function post --on-event fish_postexec; echo post:$CMD_DURATION; end", &mut host).unwrap();
        for source in ["   ", "# comment"] {
            session
                .command_finished(source, Duration::from_millis(1), &mut host)
                .unwrap();
        }
        assert_eq!(host.0, b"post:999\npost:999\n");
    }

    #[test]
    fn dimension_events_fire_only_when_values_change_and_keep_status() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("set -g COLUMNS 10; set -g LINES 20; function resized --on-variable COLUMNS --on-variable LINES; echo resize:$argv[-1]; end; false", &mut host).unwrap();
        session.update_terminal_size(90, 30, &mut host).unwrap();
        session.update_terminal_size(90, 30, &mut host).unwrap();
        assert_eq!(host.0, b"resize:COLUMNS\nresize:LINES\n");
        assert_eq!(session.status, 1);
        assert_eq!(session.pipeline, [1]);
    }

    #[test]
    fn evaluation_errors_replace_stale_pipeline_status_before_postexec() {
        let mut session = Session::new().unwrap();
        let mut host = Capture::default();
        session.eval("function post --on-event fish_postexec; echo post:$status:(string join , -- $pipestatus); end", &mut host).unwrap();
        session.pipeline = vec![9, 7];
        let error = session.eval("'unterminated", &mut host).unwrap_err();
        assert_eq!(session.pipeline, [error.status]);
        session
            .command_finished("'unterminated", Duration::ZERO, &mut host)
            .unwrap();
        assert_eq!(
            host.0,
            format!("post:{}:{}\n", error.status, error.status).as_bytes()
        );
    }

    #[test]
    fn cancellation_distinguishes_process_results_from_interpreter_work() {
        struct Interrupted {
            pipeline: Vec<i32>,
            cancelled: bool,
        }
        impl Host for Interrupted {
            fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
                self.cancelled = true;
                Ok(RunResult {
                    status: 130,
                    pipeline: self.pipeline.clone(),
                    stdout: vec![],
                    last_pid: None,
                })
            }
            fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
                unreachable!()
            }
            fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
                unreachable!()
            }
            fn write(&mut self, _: i32, _: &[u8], _: &[Redirection], _: bool) -> Result<Vec<u8>> {
                self.cancelled = true;
                Ok(vec![])
            }
            fn jobs(&self) -> Vec<Vec<u8>> {
                vec![]
            }
            fn cancelled(&self) -> bool {
                self.cancelled
            }
        }
        let mut session = Session::new().unwrap();
        let mut host = Interrupted {
            pipeline: vec![0, 143],
            cancelled: false,
        };
        assert_eq!(
            session.eval("command external_probe", &mut host).unwrap(),
            130
        );
        assert_eq!(session.pipeline, [0, 143]);
        host.cancelled = false;
        session.pipeline = vec![130, 0];
        assert_eq!(session.eval("echo interrupted", &mut host).unwrap(), 130);
        assert_eq!(session.pipeline, [130]);
    }
}
