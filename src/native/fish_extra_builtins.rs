use super::*;
use fish_wgetopt::{ArgType::NoArgument, WGetopter, WOption, wopt};
use fish_widestring::prelude::*;

impl Session {
    pub(super) fn extra_builtin(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<bool> {
        match name {
            "__fish_native_utility" => self.native_utility(args, host, output)?,
            "_" => {
                // Native Fish has no message catalog yet. Match the untranslated
                // gettext behavior, including concatenation without a newline.
                for argument in args {
                    self.write(argument.as_bytes(), host, output)?;
                }
                self.status = 0;
            }
            "__fish_git" => {
                if args.first().is_some_and(|arg| arg == "--is-enabled") {
                    self.status = i32::from(!cfg!(feature = "git"));
                } else {
                    #[cfg(feature = "git")]
                    {
                        let environment = self.completion_environment();
                        let cwd = self.values("PWD");
                        let result = cwd.first().ok_or(1).and_then(|cwd| {
                            crate::git::query::query(std::path::Path::new(cwd), &environment, args)
                        });
                        match result {
                            Ok(bytes) => {
                                self.write(&bytes, host, output)?;
                                self.status = 0;
                            }
                            Err(status) => self.status = status,
                        }
                    }
                    #[cfg(not(feature = "git"))]
                    {
                        self.status = 125;
                    }
                }
            }
            "argparse" => self.argparse(args, host, output)?,
            "printf" => {
                self.write(&printf::format(args)?, host, output)?;
                self.status = 0;
            }
            "math" => {
                self.write(&numeric::math(args)?, host, output)?;
                self.status = 0;
            }
            "string" => match string::run_with_width(
                args,
                host,
                &output.redirects,
                output.stdin_is_directly_redirected,
                crate::string_display::WidthSettings::from_overrides(
                    self.variable("fish_ambiguous_width")
                        .map(|v| v.values.join(" "))
                        .as_deref(),
                    self.variable("fish_emoji_width")
                        .map(|v| v.values.join(" "))
                        .as_deref(),
                ),
            ) {
                Ok(result) => {
                    self.write(&result.bytes, host, output)?;
                    for (field, want_newline) in result.fields {
                        self.write_separated(&field, want_newline, host, output)?;
                    }
                    for (name, values) in result.captures {
                        // Regex captures do not fire variable events in Fish.
                        let mut assignment = vec!["--no-event".into(), name];
                        assignment.extend(values);
                        self.set(&assignment, host, output)?;
                    }
                    self.status = result.status;
                    if let Some(diagnostic) = result.diagnostic {
                        output.append(host.write(
                            2,
                            format!("{diagnostic}\n").as_bytes(),
                            &output.redirects,
                            output.capture,
                        )?)?;
                    }
                }
                Err(error) => {
                    self.status = error.status;
                    output.append(host.write(
                        2,
                        format!("{}\n", error.message).as_bytes(),
                        &output.redirects,
                        output.capture,
                    )?)?;
                }
            },
            "contains" => {
                let mut args = args;
                let index = args
                    .first()
                    .is_some_and(|arg| arg == "-i" || arg == "--index");
                if index {
                    args = &args[1..];
                }
                if args.first().is_some_and(|arg| arg == "--") {
                    args = &args[1..];
                }
                let needle = args
                    .first()
                    .ok_or_else(|| Error::new("contains: expected a value"))?;
                let found = args[1..].iter().position(|value| value == needle);
                if index && let Some(index) = found {
                    self.write(format!("{}\n", index + 1).as_bytes(), host, output)?;
                }
                self.status = i32::from(found.is_none());
            }
            "read" => self.read(args, host, output)?,
            "type" | "command" => self.describe_command(name, args, host, output)?,
            "builtin" => self.builtin_query(args, host, output)?,
            "functions" => {
                self.functions_builtin(args, host, output)?;
            }
            "status" => self.status_builtin(args, host, output)?,
            "jobs" | "bg" | "fg" | "wait" | "kill" | "disown" => {
                self.host_builtin(name, args, host, output)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn host_builtin(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let command = Command {
            argv: std::iter::once(name.as_bytes().to_vec())
                .chain(args.iter().map(|arg| arg.as_bytes().to_vec()))
                .collect(),
            env: BTreeMap::new(),
            redirects: output.redirects.clone(),
            display: None,
            pipe_fd: 1,
            builtin: true,
        };
        let result = host.run_pipeline(&[command], false, output.capture)?;
        if host.cancelled() {
            self.cancelled_pipeline = Some(result.pipeline.clone());
        }
        self.status = result.status;
        output.append(result.stdout)?;
        Ok(())
    }

    fn builtin_query(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        const OPTIONS: &[WOption] = &[
            wopt(L!("help"), NoArgument, 'h'),
            wopt(L!("names"), NoArgument, 'n'),
            wopt(L!("query"), NoArgument, 'q'),
        ];
        let arguments: Vec<WString> = std::iter::once(WString::from("builtin"))
            .chain(args.iter().map(|arg| WString::from(arg.as_str())))
            .collect();
        let mut arguments: Vec<&wstr> = arguments.iter().map(WString::as_utfstr).collect();
        let original = arguments.clone();
        let mut parser = WGetopter::new(L!("hnq"), OPTIONS, &mut arguments);
        let (mut query, mut names) = (false, false);
        while let Some(option) = parser.next_opt() {
            match option {
                'q' => query = true,
                'n' => names = true,
                'h' => {
                    self.write(
                        b"builtin: see https://fishshell.com/docs/current/cmds/builtin.html\n",
                        host,
                        output,
                    )?;
                    self.status = 0;
                    return Ok(());
                }
                error @ (':' | ';' | '?') => {
                    let detail = match error {
                        ':' => "requires an argument",
                        ';' => "does not take an argument",
                        _ => "unknown option",
                    };
                    return Err(Error::new(format!(
                        "builtin: {}: {detail}",
                        original[parser.wopt_index - 1]
                    )));
                }
                _ => unreachable!("invalid builtin option"),
            }
        }
        let index = parser.wopt_index;
        drop(parser);
        if query && names {
            return Err(Error::new(
                "builtin: --query and --names are mutually exclusive",
            ));
        }
        if query {
            self.status = i32::from(
                !arguments[index..]
                    .iter()
                    .any(|name| is_builtin(&name.to_string())),
            );
        } else if names {
            for name in BUILTIN_NAMES {
                self.write(format!("{name}\n").as_bytes(), host, output)?;
            }
            self.status = 0;
        } else {
            return Err(Error::new("builtin: expected --query or --names"));
        }
        Ok(())
    }

    fn describe_command(
        &mut self,
        mode: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut quiet = false;
        let mut paths_only = mode == "command";
        let mut kind_only = false;
        let mut all = false;
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            match arg.as_str() {
                "--" => {
                    index += 1;
                    break;
                }
                "-q" | "--query" => quiet = true,
                "--quiet" if mode == "command" => quiet = true,
                "-s" | "--search" | "-v" if mode == "command" => paths_only = true,
                "-p" | "--path" | "-P" | "--force-path" => paths_only = true,
                "-t" | "--type" => kind_only = true,
                "-a" | "--all" => all = true,
                _ if mode == "command"
                    && arg.starts_with('-')
                    && arg.len() > 2
                    && arg[1..].chars().all(|flag| "asqv".contains(flag)) =>
                {
                    quiet |= arg.contains('q');
                    all |= arg.contains('a');
                    paths_only = true;
                }
                _ if arg.starts_with('-') => {
                    return Err(Error::new(format!("{mode}: unknown option {arg}")));
                }
                _ => break,
            }
            index += 1;
        }
        self.status = 0;
        let mut any_found = false;
        for name in &args[index..] {
            let mut found = Vec::new();
            if !paths_only {
                self.ensure_function(name, host, output)?;
                if self.flow == Flow::Exit {
                    return Ok(());
                }
            }
            if !paths_only && self.functions.contains_key(name) {
                found.push(("function", name.clone()));
            }
            if !paths_only && is_builtin(name) {
                found.push(("builtin", name.clone()));
            }
            if found.is_empty() || all {
                let paths = self.command_paths(name, all)?;
                let has_path = !paths.is_empty();
                for path in paths {
                    found.push(("file", path));
                }
                if !has_path && (!paths_only || quiet) && crate::native::utilities::available(name)
                {
                    found.push(("utility", name.clone()));
                }
            }
            any_found |= !found.is_empty();
            for (kind, value) in found.into_iter().take(if all { usize::MAX } else { 1 }) {
                if quiet {
                    continue;
                }
                let line = if kind_only {
                    format!("{kind}\n")
                } else if paths_only {
                    format!("{value}\n")
                } else {
                    format!(
                        "{name} is {}\n",
                        match kind {
                            "function" => "a function".to_owned(),
                            "builtin" => "a builtin".to_owned(),
                            "utility" => "a bundled utility".to_owned(),
                            _ => value,
                        }
                    )
                };
                self.write(line.as_bytes(), host, output)?;
            }
        }
        self.status = if any_found {
            0
        } else if mode == "command" {
            127
        } else {
            1
        };
        Ok(())
    }

    fn command_paths(&self, name: &str, all: bool) -> Result<Vec<String>> {
        let (path, pathext) = self.command_search_environment()?;
        let name = std::ffi::OsStr::new(name);
        let paths = if all {
            crate::native::command_resolution::paths(name, path.as_deref(), pathext.as_deref())?
        } else {
            crate::native::command_resolution::first(name, path.as_deref(), pathext.as_deref())?
                .into_iter()
                .collect()
        };
        Ok(paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect())
    }
}
