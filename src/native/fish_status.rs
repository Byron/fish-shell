//! Native status introspection with shared build and embedded-file metadata.
use super::*;
use fish_feature_flags::{FeatureFlag, METADATA, feature_test};
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::L;

fn feature_enabled(flag: FeatureFlag) -> bool {
    match flag {
        FeatureFlag::StderrNoCaret
        | FeatureFlag::AmpersandNoBgInToken
        | FeatureFlag::StringReplaceBackslash
        | FeatureFlag::TestRequireArg => feature_test(flag),
        // These native paths have fixed semantics rather than the POSIX toggle.
        FeatureFlag::QuestionMarkNoGlob
        | FeatureFlag::RemovePercentSelf
        | FeatureFlag::IgnoreTerminfo
        | FeatureFlag::MarkPrompt => true,
        FeatureFlag::QueryTerm | FeatureFlag::OmitTermWorkarounds => false,
    }
}

impl Session {
    pub(super) fn status_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if let Err(error) = self.status_impl(args, host, output) {
            self.status = error.status;
            output.append(host.write(
                2,
                format!("{}\n", error.message).as_bytes(),
                &output.redirects,
                output.capture,
            )?)?;
        }
        Ok(())
    }

    fn status_impl(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        const LONG: &[WOption] = &[
            wopt(L!("help"), NoArgument, 'h'),
            wopt(L!("current-filename"), NoArgument, 'f'),
            wopt(L!("filename"), NoArgument, 'f'),
            wopt(L!("fish-path"), NoArgument, '\x01'),
            wopt(L!("is-block"), NoArgument, 'b'),
            wopt(L!("is-command-substitution"), NoArgument, 'c'),
            wopt(L!("is-full-job-control"), NoArgument, '\x02'),
            wopt(L!("is-interactive-job-control"), NoArgument, '\x03'),
            wopt(L!("is-no-job-control"), NoArgument, '\x04'),
            wopt(L!("is-interactive-read"), NoArgument, '\x05'),
            wopt(L!("is-interactive"), NoArgument, 'i'),
            wopt(L!("is-login"), NoArgument, 'l'),
            wopt(L!("job-control"), RequiredArgument, 'j'),
            wopt(L!("level"), RequiredArgument, 'L'),
            wopt(L!("line"), NoArgument, 'n'),
            wopt(L!("line-number"), NoArgument, 'n'),
            wopt(L!("current-line-number"), NoArgument, 'n'),
            wopt(L!("print-stack-trace"), NoArgument, 't'),
        ];
        let words: Vec<WString> = std::iter::once("status")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut argv: Vec<&wstr> = words.iter().map(WString::as_utfstr).collect();
        let original = argv.clone();
        let mut parser = WGetopter::new(L!("L:cbilfnhj:t"), LONG, &mut argv);
        let mut command = None;
        let mut level = 1usize;
        let mut job_mode = None;
        let mut help = false;
        let mut select = |query| -> Result<()> {
            if let Some(previous) = command.replace(query) {
                return Err(Error {
                    message: format!("status: {previous} and {query} are mutually exclusive"),
                    status: 1,
                });
            }
            Ok(())
        };
        while let Some(option) = parser.next_opt() {
            match option {
                'h' => help = true,
                'L' => {
                    let value = parser
                        .woptarg
                        .unwrap()
                        .to_string()
                        .trim()
                        .parse::<i32>()
                        .map_err(|_| Error::new("status: invalid function level"))?;
                    level = usize::try_from(value)
                        .map_err(|_| Error::new("status: invalid function level"))?;
                }
                'j' => {
                    select("job-control")?;
                    job_mode = Some(parser.woptarg.unwrap().to_string());
                }
                'c' => select("is-command-substitution")?,
                'b' => select("is-block")?,
                'i' => select("is-interactive")?,
                'l' => select("is-login")?,
                'f' => select("filename")?,
                'n' => select("line-number")?,
                't' => select("stack-trace")?,
                '\x01' => select("fish-path")?,
                '\x02' => select("is-full-job-control")?,
                '\x03' => select("is-interactive-job-control")?,
                '\x04' => select("is-no-job-control")?,
                '\x05' => select("is-interactive-read")?,
                error @ (':' | ';' | '?') => {
                    return Err(Error::new(format!(
                        "status: {}: {}",
                        original[parser.wopt_index - 1],
                        match error {
                            ':' => "requires an argument",
                            ';' => "does not take an argument",
                            _ => "unknown option",
                        }
                    )));
                }
                _ => unreachable!("status option"),
            }
        }
        drop(select);
        let mut arguments = &parser.argv[parser.wopt_index..];
        if help {
            self.write(
                b"status: see https://fishshell.com/docs/current/cmds/status.html\n",
                host,
                output,
            )?;
            self.status = 0;
            return Ok(());
        }
        let positional = arguments.first().map(ToString::to_string);
        if let Some(query) = positional.as_deref() {
            let query = match query {
                "current-filename" => "filename",
                "current-basename" => "basename",
                "current-dirname" => "dirname",
                "current-function" => "function",
                "current-line-number" => "line-number",
                "print-stack-trace" => "stack-trace",
                "buildinfo" => "build-info",
                "basename"
                | "build-info"
                | "current-command"
                | "current-commandline"
                | "dirname"
                | "features"
                | "filename"
                | "fish-path"
                | "function"
                | "get-file"
                | "is-block"
                | "is-breakpoint"
                | "is-command-substitution"
                | "is-full-job-control"
                | "is-interactive"
                | "is-interactive-job-control"
                | "is-interactive-read"
                | "is-login"
                | "is-native-windows"
                | "is-no-job-control"
                | "job-control"
                | "language"
                | "line-number"
                | "list-files"
                | "stack-trace"
                | "terminal"
                | "terminal-os"
                | "test-feature"
                | "test-terminal-feature" => query,
                _ => return Err(Error::new(format!("status: unknown subcommand '{query}'"))),
            };
            if command.is_some() {
                return Err(Error {
                    message: "status: multiple status queries".into(),
                    status: 1,
                });
            }
            command = Some(query);
            arguments = &arguments[1..];
        }
        let command = command.unwrap_or("");
        let require = |count| -> Result<()> {
            if arguments.len() != count {
                Err(Error::new(format!(
                    "status {command}: expected {count} arguments; got {}",
                    arguments.len()
                )))
            } else {
                Ok(())
            }
        };
        self.status = 0;
        match command {
            "test-feature" => {
                require(1)?;
                self.status = METADATA
                    .iter()
                    .find(|metadata| metadata.name == arguments[0])
                    .map_or(2, |metadata| i32::from(!feature_enabled(metadata.flag)));
            }
            "features" => self.write(
                crate::status_info::features(feature_enabled)
                    .to_string()
                    .as_bytes(),
                host,
                output,
            )?,
            "build-info" => {
                require(0)?;
                self.write(
                    crate::status_info::build_info().to_string().as_bytes(),
                    host,
                    output,
                )?;
            }
            "list-files" => {
                let files = crate::status_info::list_files(arguments);
                self.status = i32::from(files.is_empty());
                for path in files {
                    self.write(format!("{path}\n").as_bytes(), host, output)?;
                }
            }
            "get-file" => {
                require(1)?;
                match crate::status_info::get_file(&arguments[0].to_string()) {
                    Some(data) => self.write(&data, host, output)?,
                    None => self.status = 1,
                }
            }
            "filename" | "basename" | "dirname" => {
                require(0)?;
                let filename = self.source_origin.as_deref().unwrap_or("Standard input");
                let path = std::path::Path::new(filename);
                let value = match (self.source_origin.is_some(), command) {
                    (true, "basename") => path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    (true, "dirname") => path
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_string_lossy()
                        .into_owned(),
                    _ => filename.into(),
                };
                self.write(format!("{value}\n").as_bytes(), host, output)?;
            }
            "function" => {
                require(0)?;
                let name = level
                    .checked_sub(1)
                    .and_then(|level| self.function_frames.iter().rev().nth(level))
                    .map_or("Not a function", |frame| frame.name.as_str());
                self.write(format!("{name}\n").as_bytes(), host, output)?;
            }
            "fish-path" => {
                require(0)?;
                self.write(
                    format!("{}\n", std::env::current_exe()?.display()).as_bytes(),
                    host,
                    output,
                )?;
            }
            "is-interactive"
            | "is-login"
            | "is-native-windows"
            | "is-block"
            | "is-command-substitution"
            | "is-breakpoint" => {
                require(0)?;
                let value = match command {
                    "is-interactive" => self.interactive,
                    "is-login" => self.login,
                    "is-native-windows" => true,
                    "is-block" => self.scopes.len() > 1,
                    "is-command-substitution" => self.substitution_depth > 0,
                    _ => false,
                };
                self.status = i32::from(!value);
            }
            "current-command" => {
                require(0)?;
                self.write(b"fish\n", host, output)?;
            }
            "current-commandline" if !self.interactive => {
                require(0)?;
                self.write(b"\n", host, output)?;
            }
            "" => self.write(b"This is native Windows Fish.\n", host, output)?,
            "job-control"
            | "is-full-job-control"
            | "is-interactive-job-control"
            | "is-no-job-control"
            | "is-interactive-read"
            | "current-commandline"
            | "line-number"
            | "stack-trace"
            | "terminal"
            | "terminal-os"
            | "test-terminal-feature"
            | "language" => {
                match command {
                    "job-control" => require(usize::from(job_mode.is_none()))?,
                    "test-terminal-feature" => require(1)?,
                    "language" => (),
                    _ => require(0)?,
                }
                return Err(Error {
                    message: format!(
                        "status {command}: this query is not available in native Windows Fish"
                    ),
                    status: 1,
                });
            }
            _ => {
                return Err(Error::new(format!(
                    "status: unknown subcommand '{command}'"
                )));
            }
        }
        Ok(())
    }
}
