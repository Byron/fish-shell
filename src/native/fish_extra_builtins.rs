use super::*;

impl Session {
    pub(super) fn extra_builtin(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<bool> {
        match name {
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
            "string" => match string::run(
                args,
                host,
                &output.redirects,
                output.stdin_is_directly_redirected,
            ) {
                Ok(result) => {
                    self.write(&result.bytes, host, output)?;
                    for (name, values) in result.captures {
                        // Regex captures do not fire variable events in Fish.
                        let mut assignment = vec!["--no-event".into(), name];
                        assignment.extend(values);
                        self.set(&assignment, host, output)?;
                    }
                    self.status = result.status;
                    if let Some(diagnostic) = result.diagnostic {
                        output.bytes.extend(host.write(
                            2,
                            format!("{diagnostic}\n").as_bytes(),
                            &output.redirects,
                            output.capture,
                        )?);
                    }
                }
                Err(error) => {
                    self.status = error.status;
                    output.bytes.extend(host.write(
                        2,
                        format!("{}\n", error.message).as_bytes(),
                        &output.redirects,
                        output.capture,
                    )?);
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
                if index {
                    if let Some(index) = found {
                        self.write(format!("{}\n", index + 1).as_bytes(), host, output)?;
                    }
                }
                self.status = i32::from(found.is_none());
            }
            "read" => self.read(args, host, output)?,
            "type" | "command" | "builtin" => self.describe_command(name, args, host, output)?,
            "functions" => {
                self.refresh_function_paths();
                let mut args = args;
                let query = args
                    .first()
                    .is_some_and(|arg| matches!(arg.as_str(), "-q" | "--query"));
                let erase = args
                    .first()
                    .is_some_and(|arg| matches!(arg.as_str(), "-e" | "--erase"));
                let names_only = args
                    .first()
                    .is_some_and(|arg| matches!(arg.as_str(), "-n" | "--names"));
                if query || erase || names_only {
                    args = &args[1..];
                }
                if args.first().is_some_and(|arg| arg == "--") {
                    args = &args[1..];
                }
                if args.first().is_some_and(|arg| arg.starts_with('-')) {
                    return Err(unsupported("these functions options"));
                }
                let names: Vec<_> = if args.is_empty() {
                    if names_only {
                        self.function_names()
                    } else {
                        self.functions.keys().cloned().collect()
                    }
                } else {
                    args.to_vec()
                };
                self.status = 0;
                for name in names {
                    if names_only {
                        self.write(format!("{name}\n").as_bytes(), host, output)?;
                        continue;
                    }
                    if erase {
                        self.autoload_tombstones.insert(name.clone());
                        self.autoloaded_functions.remove(&name);
                        self.status |= i32::from(self.functions.remove(&name).is_none());
                    } else {
                        self.ensure_function(&name, host, output)?;
                        if self.flow == Flow::Exit {
                            return Ok(true);
                        }
                        if let Some(function) = self.functions.get(&name) {
                            if !query {
                                self.write(function.definition(&name).as_bytes(), host, output)?;
                            }
                        } else {
                            self.status = 1;
                        }
                    }
                }
            }
            "status" => {
                self.status = 0;
                match args.first().map_or("", String::as_str) {
                    "is-interactive" | "--is-interactive" | "-i" => {
                        self.status = i32::from(!self.interactive);
                    }
                    "is-login" | "--is-login" | "-l" => self.status = i32::from(!self.login),
                    "is-native-windows" => {
                        if args.len() != 1 {
                            return Err(Error::new(format!(
                                "status is-native-windows: expected 0 arguments; got {}",
                                args.len() - 1
                            )));
                        }
                    }
                    "is-block" | "--is-block" | "-b" => {
                        self.status = i32::from(self.scopes.len() <= 1)
                    }
                    "is-command-substitution" | "--is-command-substitution" | "-c" => {
                        self.status = i32::from(!output.capture)
                    }
                    "fish-path" | "--fish-path" => self.write(
                        format!("{}\n", std::env::current_exe()?.display()).as_bytes(),
                        host,
                        output,
                    )?,
                    "current-command" => self.write(b"fish\n", host, output)?,
                    "filename" | "current-filename" | "--filename" | "--current-filename"
                    | "-f" => {
                        if args.len() != 1 {
                            return Err(Error::new("status filename: expected no arguments"));
                        }
                        let filename = self.source_origin.as_deref().unwrap_or("Standard input");
                        self.write(format!("{filename}\n").as_bytes(), host, output)?;
                    }
                    "" => self.write(b"This is native Windows Fish.\n", host, output)?,
                    _ => return Err(unsupported("this status query")),
                }
            }
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
            builtin: true,
        };
        let result = host.run_pipeline(&[command], false, output.capture)?;
        self.status = result.status;
        output.bytes.extend(result.stdout);
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
        for name in &args[index..] {
            let mut found = Vec::new();
            if !paths_only && mode != "builtin" {
                self.ensure_function(name, host, output)?;
                if self.flow == Flow::Exit {
                    return Ok(());
                }
            }
            if !paths_only && self.functions.contains_key(name) && mode != "builtin" {
                found.push(("function", name.clone()));
            }
            if !paths_only && is_builtin(name) {
                found.push(("builtin", name.clone()));
            }
            if mode != "builtin" {
                for path in self.command_paths(name) {
                    found.push(("file", path));
                }
            }
            if found.is_empty() {
                self.status = if mode == "command" { 127 } else { 1 };
            }
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
                            _ => value,
                        }
                    )
                };
                self.write(line.as_bytes(), host, output)?;
            }
        }
        Ok(())
    }

    fn command_paths(&self, name: &str) -> Vec<String> {
        let explicit = name.contains(['/', '\\']) || std::path::Path::new(name).is_absolute();
        let directories = if explicit {
            vec![String::new()]
        } else {
            self.values("PATH")
        };
        let extensions = self.values("PATHEXT").join(";");
        let extensions = if extensions.is_empty() {
            ".COM;.EXE;.BAT;.CMD"
        } else {
            &extensions
        };
        let mut results = Vec::new();
        for directory in directories {
            let path = std::path::Path::new(&directory).join(name);
            for suffix in std::iter::once("").chain(extensions.split(';')) {
                let candidate = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
                if candidate.is_file() {
                    results.push(candidate.to_string_lossy().into_owned());
                    break;
                }
            }
        }
        results
    }

    fn read(&mut self, args: &[String], host: &mut impl Host, output: &mut Output) -> Result<()> {
        let mut scope = Vec::new();
        let mut array = false;
        let mut delimiter = None;
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            match arg.as_str() {
                "--" => {
                    index += 1;
                    break;
                }
                "-l" | "--local" | "-g" | "--global" | "-x" | "--export" | "-u" | "--unexport" => {
                    scope.push(arg.clone());
                }
                "-a" | "--array" => array = true,
                "-d" | "--delimiter" => {
                    index += 1;
                    delimiter = Some(
                        args.get(index)
                            .ok_or_else(|| Error::new("read: missing delimiter"))?
                            .clone(),
                    );
                }
                "-P" | "--prompt-str" => {
                    index += 1;
                    let prompt = args
                        .get(index)
                        .ok_or_else(|| Error::new("read: missing prompt"))?;
                    self.write(prompt.as_bytes(), host, output)?;
                }
                _ if arg.starts_with('-') => return Err(unsupported(&format!("read {arg}"))),
                _ => break,
            }
            index += 1;
        }
        let names = &args[index..];
        for name in names {
            valid_name(name)?;
        }
        if array && names.len() != 1 {
            return Err(Error::new("read --array: expected one variable"));
        }
        let rash::host::ReadResult::Line(bytes) = host.read_line(&output.redirects, false)? else {
            self.status = 130;
            return Ok(());
        };
        let read_status = i32::from(bytes.is_empty());
        let line = String::from_utf8(bytes).map_err(|_| Error::new("read: input is not UTF-8"))?;
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if names.is_empty() {
            self.write(format!("{line}\n").as_bytes(), host, output)?;
            self.status = read_status;
            return Ok(());
        }
        let fields: Vec<String> = if let Some(separator) = delimiter {
            if array {
                line.split(&separator).map(str::to_owned).collect()
            } else {
                line.splitn(names.len(), &separator)
                    .map(str::to_owned)
                    .collect()
            }
        } else if array {
            line.split_whitespace().map(str::to_owned).collect()
        } else {
            let mut rest = line.trim_start();
            let mut fields = Vec::new();
            for _ in 1..names.len() {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                fields.push(rest[..end].to_owned());
                rest = rest[end..].trim_start();
            }
            fields.push(rest.to_owned());
            fields
        };
        if array {
            let mut set_args = scope;
            set_args.push(names[0].clone());
            set_args.extend(fields);
            self.set(&set_args, host, output)?;
        } else {
            for (index, name) in names.iter().enumerate() {
                let mut set_args = scope.clone();
                set_args.push(name.clone());
                set_args.push(fields.get(index).cloned().unwrap_or_default());
                self.set(&set_args, host, output)?;
            }
        }
        self.status = read_status;
        Ok(())
    }
}
