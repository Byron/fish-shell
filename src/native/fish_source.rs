//! Sourced input and filename context around the native evaluator.
use super::*;
use fish_wgetopt::{ArgType, WGetopter, WOption, wopt};
use fish_widestring::L;
use rash::host::ReadResult;

impl Session {
    pub(super) fn source_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let wide: Vec<WString> = std::iter::once("source")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut argv: Vec<&wstr> = wide.iter().map(WString::as_utfstr).collect();
        const OPTIONS: &[WOption] = &[wopt(L!("help"), ArgType::NoArgument, 'h')];
        let mut options = WGetopter::new(L!("+h"), OPTIONS, &mut argv);
        let mut help = false;
        while let Some(option) = options.next_opt() {
            if option == 'h' {
                help = true;
            } else {
                let argument = options.argv[options.wopt_index.saturating_sub(1)];
                return self.source_failure(
                    2,
                    &format!("{argument}: invalid source option"),
                    host,
                    output,
                );
            }
        }
        let args = &args[options.wopt_index - 1..];
        if help {
            self.write(
                b"source [FILE|-] [ARGUMENTS ...]: evaluate a file or standard input\n",
                host,
                output,
            )?;
            self.status = 0;
            return Ok(());
        }
        let input = args.first().map(String::as_str);
        let filename = input.unwrap_or("-");
        let source = if input.is_none() || input == Some("-") {
            if input.is_none() {
                match host.fd_is_terminal(0, &output.redirects, output.capture) {
                    Ok(true) => {
                        return self.source_failure(
                            1,
                            "missing filename argument or input redirection",
                            host,
                            output,
                        );
                    }
                    Ok(false) => (),
                    Err(error) => return self.source_failure(1, &error.message, host, output),
                }
            }
            let mut bytes = Vec::new();
            loop {
                if host.cancelled() {
                    self.status = 130;
                    self.pipeline = vec![130];
                    return Ok(());
                }
                match host.read_line(&output.redirects, false) {
                    Ok(ReadResult::Line(line)) if line.is_empty() => break,
                    Ok(ReadResult::Line(line) | ReadResult::Interrupted(line)) => {
                        bytes.extend(line)
                    }
                    Err(error) => return self.source_failure(1, &error.message, host, output),
                }
            }
            String::from_utf8(bytes).map_err(|_| Error::new("input is not UTF-8"))
        } else {
            std::fs::read_to_string(filename).map_err(Error::from)
        };
        let source = match source {
            Ok(source) => source,
            Err(error) => {
                return self.source_failure(
                    1,
                    &format!("{filename}: {}", error.message),
                    host,
                    output,
                );
            }
        };
        // Match POSIX source: consume the complete input before parsing or
        // executing it. A trailing syntax error cannot run an earlier command.
        let arguments = if input.is_some() { &args[1..] } else { &[] };
        match self.eval_sourced(&source, filename.into(), arguments, false, host, output) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.source_failure(1, &format!("{filename}: {}", error.message), host, output)
            }
        }
    }

    fn source_failure(
        &mut self,
        status: i32,
        message: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.status = status;
        self.pipeline = vec![status];
        output.bytes.extend(host.write(
            2,
            format!("source: {message}\n").as_bytes(),
            &output.redirects,
            output.capture,
        )?);
        Ok(())
    }

    /// Configuration and autoload deliberately preserve their existing ability
    /// to stop this native shell; an explicit source returns from its own input.
    pub(super) fn eval_sourced(
        &mut self,
        source: &str,
        origin: String,
        arguments: &[String],
        preserve_exit: bool,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.scopes.push(BTreeMap::from([(
            "argv".into(),
            Variable {
                values: arguments.to_vec(),
                exported: false,
                pathvar: false,
            },
        )]));
        self.source_depth += 1;
        let saved_origin = self.source_origin.replace(origin);
        // The source command's stdin is inherited by commands in the script;
        // it is not a direct pipe/redirection of each argument-taking builtin.
        let pipeline_input = std::mem::replace(&mut output.pipeline_input, false);
        let directly_redirected =
            std::mem::replace(&mut output.stdin_is_directly_redirected, false);
        let result = self.eval_source(source, host, output);
        output.pipeline_input = pipeline_input;
        output.stdin_is_directly_redirected = directly_redirected;
        self.source_origin = saved_origin;
        self.source_depth -= 1;
        self.scopes.pop();
        if self.flow == Flow::Return || self.flow == Flow::Exit && !preserve_exit {
            self.flow = Flow::Normal;
        }
        result
    }

    /// A top-level script retains its ordinary argument and scope semantics.
    pub(in crate::native) fn eval_with_origin(
        &mut self,
        source: &str,
        origin: Option<String>,
        host: &mut impl Host,
    ) -> Result<i32> {
        let saved_origin = std::mem::replace(&mut self.source_origin, origin);
        let result = self.eval(source, host);
        self.source_origin = saved_origin;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::RunResult;

    #[derive(Default)]
    struct SourceHost {
        terminal: bool,
        cancel_after_read: Option<usize>,
        input: std::collections::VecDeque<Vec<u8>>,
        reads: usize,
        output: Vec<u8>,
    }

    impl Host for SourceHost {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            unreachable!()
        }

        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            unreachable!()
        }

        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            self.reads += 1;
            Ok(ReadResult::Line(self.input.pop_front().unwrap_or_default()))
        }

        fn fd_is_terminal(&mut self, fd: i32, _: &[Redirection], _: bool) -> Result<bool> {
            assert_eq!(fd, 0);
            Ok(self.terminal)
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
                self.output.extend(bytes);
                Ok(vec![])
            }
        }

        fn cancelled(&self) -> bool {
            self.cancel_after_read
                .is_some_and(|count| self.reads >= count)
        }

        fn jobs(&self) -> Vec<Vec<u8>> {
            vec![]
        }
    }

    #[test]
    fn only_explicit_stdin_source_can_read_a_terminal() {
        let mut session = Session::new().unwrap();
        let mut host = SourceHost {
            terminal: true,
            input: [b"echo explicit\n".to_vec()].into(),
            ..SourceHost::default()
        };
        assert_eq!(session.eval("source", &mut host).unwrap(), 1);
        assert_eq!(host.reads, 0);
        host.output.clear();
        assert_eq!(session.eval("source -", &mut host).unwrap(), 0);
        assert_eq!(host.reads, 2);
        assert_eq!(host.output, b"explicit\n");
    }

    #[test]
    fn cancellation_does_not_evaluate_partially_read_source() {
        let mut session = Session::new().unwrap();
        let mut host = SourceHost {
            cancel_after_read: Some(1),
            input: [b"set -g changed yes\n".to_vec()].into(),
            ..SourceHost::default()
        };
        let mut output = Output::default();
        session
            .source_builtin(&["-".into()], &mut host, &mut output)
            .unwrap();
        assert_eq!(session.status, 130);
        assert_eq!(session.pipeline, [130]);
        assert!(session.values("changed").is_empty());
        assert_eq!(host.reads, 1);
    }

    #[test]
    fn failed_source_and_function_restore_the_callers_origin_and_scope() {
        let mut session = Session::new().unwrap();
        let mut host = SourceHost::default();
        session.source_origin = Some("caller.fish".into());
        let scope_count = session.scopes.len();
        let mut output = Output {
            pipeline_input: true,
            stdin_is_directly_redirected: true,
            ..Output::default()
        };
        assert!(
            session
                .eval_sourced(
                    "set -l transient hidden; math 'invalid('",
                    "failed.fish".into(),
                    &["temporary".into()],
                    false,
                    &mut host,
                    &mut output
                )
                .is_err()
        );
        assert_eq!(session.source_origin.as_deref(), Some("caller.fish"));
        assert_eq!(session.scopes.len(), scope_count);
        assert_eq!(session.source_depth, 0);
        assert!(output.pipeline_input && output.stdin_is_directly_redirected);
        assert!(session.values("transient").is_empty());
        session
            .eval_with_origin(
                "function failed_function; math 'invalid('; end",
                Some("definition.fish".into()),
                &mut host,
            )
            .unwrap();
        assert!(session.eval("failed_function", &mut host).is_err());
        assert_eq!(session.source_origin.as_deref(), Some("caller.fish"));
        assert_eq!(session.scopes.len(), scope_count);
        assert!(session.function_frames.is_empty());
    }
}
