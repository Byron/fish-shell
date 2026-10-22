use super::*;
use crate::variable_indices;

#[path = "fish_extra_builtins.rs"]
mod extra;
#[path = "fish_numeric.rs"]
pub(super) mod numeric;
#[path = "fish_printf.rs"]
mod printf;
#[path = "fish_string.rs"]
mod string;

pub(super) fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "true"
            | "__fish_git"
            | ":"
            | "false"
            | "echo"
            | "set"
            | "emit"
            | "pwd"
            | "cd"
            | "count"
            | "history"
            | "exit"
            | "return"
            | "break"
            | "continue"
            | "test"
            | "["
            | "source"
            | "."
            | "eval"
            | "printf"
            | "math"
            | "path"
            | "realpath"
            | "string"
            | "contains"
            | "argparse"
            | "read"
            | "status"
            | "type"
            | "command"
            | "builtin"
            | "functions"
            | "jobs"
            | "bg"
            | "fg"
            | "wait"
            | "kill"
            | "disown"
            | "complete"
            | "commandline"
            | "set_color"
            | "abbr"
            | "bind"
            | "fish_vi_key_bindings"
            | "fish_default_key_bindings"
            | "fish_input_mode"
    )
}

impl Session {
    pub(super) fn dispatch(
        &mut self,
        name: &str,
        args: &[String],
        decoration: StatementDecoration,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if decoration == StatementDecoration::None {
            self.ensure_function(name, host, output)?;
            if self.flow == Flow::Exit {
                return Ok(());
            }
            if let Some(function) = self.functions.get(name).cloned() {
                return self.invoke_function(name, function, args, host, output);
            }
        }
        if decoration == StatementDecoration::Command {
            return self.external(name, args, host, output);
        }
        if self.extra_builtin(name, args, host, output)? {
            return Ok(());
        }
        match name {
            "path" | "realpath" => self.path_builtin(name, args, host, output)?,
            "fish_input_mode" => {
                if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help") {
                    self.write(
                        b"fish_input_mode fish|bash: select the next Fish prompt's syntax\n",
                        host,
                        output,
                    )?;
                    self.status = 0;
                    return Ok(());
                }
                let input_language = match args {
                    [language] if language == "fish" => super::super::InputLanguage::Fish,
                    [language] if language == "bash" => super::super::InputLanguage::Bash,
                    _ => return Err(Error::new("fish_input_mode: expected fish|bash")),
                };
                if !self.interactive || self.input_mode_blocked {
                    return Err(Error {
                        message: "fish_input_mode: use interactive startup configuration or a submitted command, outside prompts and command substitutions".into(),
                        status: 1,
                    });
                }
                self.input_language = input_language;
                self.status = 0;
            }
            "abbr" | "bind" => {
                let (status, text) = if name == "abbr" {
                    self.abbreviations.run_builtin(args)
                } else {
                    self.bindings.run_builtin(args)
                }
                .map_err(Error::new)?;
                self.write(text.as_bytes(), host, output)?;
                self.status = status;
            }
            "fish_vi_key_bindings" | "fish_default_key_bindings" => {
                if !args.is_empty() {
                    return Err(unsupported("keymap command options"));
                }
                self.set(
                    &["-g".into(), "fish_key_bindings".into(), name.into()],
                    host,
                    output,
                )?;
            }
            "set_color" => {
                let color = crate::native::interactive_colors::set_color(args)
                    .map_err(|error| Error::new(format!("set_color: {error}")))?;
                self.write(color.as_bytes(), host, output)?;
                self.status = 0;
            }
            "complete" => self.complete_builtin(args, host, output)?,
            "commandline" => self.commandline_builtin(args, host, output)?,
            "history" => {
                if let Err(error) = self.history_builtin(args, host, output) {
                    self.status = error.status;
                    output.bytes.extend(host.write(
                        2,
                        format!("{}\n", error.message).as_bytes(),
                        &output.redirects,
                        output.capture,
                    )?);
                }
            }
            "true" | ":" => self.status = 0,
            "false" => self.status = 1,
            "echo" => {
                let mut index = 0;
                let mut newline = true;
                let mut escape = false;
                while let Some(arg) = args.get(index) {
                    if arg == "-n" {
                        newline = false;
                    } else if arg == "-e" {
                        escape = true;
                    } else if arg == "-E" {
                        escape = false;
                    } else {
                        break;
                    }
                    index += 1;
                }
                let mut value = args[index..].join(" ");
                if escape {
                    let (unescaped, stop) = expand::echo(&value)?;
                    value = unescaped;
                    newline &= !stop;
                }
                if newline {
                    value.push('\n');
                }
                self.write(value.as_bytes(), host, output)?;
                self.status = 0;
            }
            "set" => match self.set(args, host, output) {
                Ok(true) => (),
                Ok(false) => self.pipeline = vec![self.status],
                Err(error) => {
                    self.status = error.status;
                    self.pipeline = vec![self.status];
                    if !error.message.is_empty() {
                        output.bytes.extend(host.write(
                            2,
                            format!("{}\n", error.message).as_bytes(),
                            &output.redirects,
                            output.capture,
                        )?);
                    }
                }
            },
            "emit" => {
                if let Err(error) = self.emit(args, host, output) {
                    self.status = error.status;
                    output.bytes.extend(host.write(
                        2,
                        format!("{}\n", error.message).as_bytes(),
                        &output.redirects,
                        output.capture,
                    )?);
                }
            }
            "pwd" => {
                if !args.is_empty() {
                    return Err(unsupported("pwd options"));
                }
                self.write(
                    format!("{}\n", self.values("PWD").join(" ")).as_bytes(),
                    host,
                    output,
                )?;
                self.status = 0;
            }
            "cd" => {
                if args.len() > 1 {
                    return Err(Error::new("cd: too many arguments"));
                }
                let path = args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| self.values("HOME").join(" "));
                let cwd = host.change_dir(path.as_bytes())?;
                self.variables.insert(
                    "PWD".into(),
                    Variable {
                        values: vec![
                            String::from_utf8(cwd)
                                .map_err(|_| Error::new("working directory is not Unicode"))?,
                        ],
                        exported: true,
                        pathvar: false,
                    },
                );
                self.fire_variable_event("PWD", false, host)?;
                self.status = 0;
            }
            "count" => {
                self.write(format!("{}\n", args.len()).as_bytes(), host, output)?;
                self.status = i32::from(args.is_empty());
            }
            "exit" | "return" => {
                if args.len() > 1 {
                    return Err(Error::new(format!("{name}: too many arguments")));
                }
                self.status = args
                    .first()
                    .map(|arg| {
                        arg.parse::<i32>()
                            .map(|n| n.rem_euclid(256))
                            .map_err(|_| Error::new(format!("{name}: expected an integer")))
                    })
                    .transpose()?
                    .unwrap_or(self.status);
                if name == "return" && self.function_depth == 0 && self.source_depth == 0 {
                    return Err(Error::new("return: outside of a function or sourced file"));
                }
                self.flow = if name == "exit" {
                    Flow::Exit
                } else {
                    Flow::Return
                };
            }
            "break" | "continue" => {
                if !args.is_empty() || self.loops == 0 {
                    return Err(Error::new(format!(
                        "{name}: expected a loop and no arguments"
                    )));
                }
                self.flow = if name == "break" {
                    Flow::Break
                } else {
                    Flow::Continue
                };
                self.status = 0;
            }
            "test" | "[" => self.test_builtin(name, args, host, output)?,
            "source" | "." => {
                return self.source_builtin(args, host, output);
            }
            "eval" => return self.eval_source(&args.join(" "), host, output),
            // Keep Fish builtins reserved while their native implementations are pending.
            "printf" | "string" | "math" | "read" | "status" | "functions" | "type" | "command"
            | "builtin" | "argparse" | "contains" | "random" | "jobs" | "fg" | "bg" | "wait"
            | "disown" | "exec" | "ulimit" | "umask" => {
                return Err(unsupported(&format!("the {name} builtin")));
            }
            _ => {
                if decoration == StatementDecoration::Builtin {
                    return Err(Error::new(format!("builtin: unknown builtin {name}")));
                }
                return self.external(name, args, host, output);
            }
        }
        Ok(())
    }

    fn external(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let env = self.exports();
        let command = Command {
            argv: std::iter::once(name.as_bytes().to_vec())
                .chain(args.iter().map(|arg| arg.as_bytes().to_vec()))
                .collect(),
            display: None,
            env,
            redirects: output.redirects.clone(),
            builtin: false,
        };
        let result = host.run_pipeline(&[command], false, output.capture)?;
        self.status = result.status;
        output.bytes.extend(result.stdout);
        Ok(())
    }

    fn set_scope_variable(
        &self,
        name: &str,
        local: bool,
        global: bool,
        universal: bool,
    ) -> Option<&Variable> {
        if universal {
            self.universals.vars.get(name)
        } else if global {
            self.variables.get(name)
        } else if local {
            self.scopes[self.scope_start()..]
                .iter()
                .rev()
                .find_map(|scope| scope.get(name))
        } else {
            self.variable(name)
        }
    }

    /// Return whether this was an assignment, preserving the preceding status.
    fn set(&mut self, args: &[String], host: &mut impl Host, output: &mut Output) -> Result<bool> {
        let (mut local, mut global, mut universal) = (false, false, false);
        let (mut exported, mut pathvar) = (None, None);
        let (mut query, mut erase, mut append, mut prepend, mut no_event, mut names) =
            (false, false, false, false, false, false);
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                index += 1;
                break;
            }
            if !arg.starts_with('-') || arg == "-" {
                break;
            }
            match arg.as_str() {
                "--local" => local = true,
                "--global" => global = true,
                "--universal" => universal = true,
                "--export" => exported = Some(true),
                "--unexport" => exported = Some(false),
                "--path" => pathvar = Some(true),
                "--unpath" => pathvar = Some(false),
                "--query" => query = true,
                "--erase" => erase = true,
                "--append" => append = true,
                "--prepend" => prepend = true,
                "--no-event" => no_event = true,
                "--names" => names = true,
                _ if !arg.starts_with("--") => {
                    for flag in arg[1..].chars() {
                        match flag {
                            'l' => local = true,
                            'g' => global = true,
                            'U' => universal = true,
                            'x' => exported = Some(true),
                            'u' => exported = Some(false),
                            'q' => query = true,
                            'e' => erase = true,
                            'a' => append = true,
                            'p' => prepend = true,
                            'n' => names = true,
                            _ => return Err(unsupported(&format!("set -{flag}"))),
                        }
                    }
                }
                _ => return Err(unsupported(&format!("set {arg}"))),
            }
            index += 1;
        }
        if usize::from(local) + usize::from(global) + usize::from(universal) > 1 {
            return Err(Error::new(
                "set: local, global and universal scopes conflict",
            ));
        }
        let args = &args[index..];
        if query {
            if erase || append || prepend || names {
                return Err(Error::new("set: incompatible options"));
            }
            let mut missing = 0usize;
            for argument in args {
                let expression = WString::from(argument.as_str());
                let name = variable_indices::variable_name(&expression).to_string();
                valid_name(&name)?;
                let electric = (!local
                    && !universal
                    && matches!(
                        name.as_str(),
                        "status" | "pipestatus" | "last_pid" | "fish_pid"
                    ))
                .then(|| Variable {
                    values: self.values(&name),
                    ..Default::default()
                });
                let variable = self
                    .set_scope_variable(&name, local, global, universal)
                    .or(electric.as_ref())
                    .filter(|variable| {
                        !exported.is_some_and(|flag| flag != variable.exported)
                            && !pathvar.is_some_and(|flag| flag != variable.pathvar)
                    });
                let length = variable.map_or(0, |variable| variable.values.len());
                let indexes = variable_indices::parse(&expression, length)
                    .map_err(|error| index_error(error, 1))?;
                missing += if indexes.is_empty() {
                    usize::from(variable.is_none())
                } else {
                    indexes
                        .iter()
                        .filter(|&&index| index < 1 || index as usize > length)
                        .count()
                };
            }
            self.status = missing.min(255) as i32;
            return Ok(false);
        }
        if erase && (append || prepend || exported.is_some() || pathvar.is_some() || names) {
            return Err(Error::new("set: incompatible erase options"));
        }
        if args.is_empty() || names {
            let mut merged = BTreeMap::new();
            if universal || (!global && !local) {
                merged.extend(self.universals.vars.clone());
            }
            if global || (!universal && !local) {
                merged.extend(self.variables.clone());
            }
            if local || (!global && !universal) {
                for scope in &self.scopes[self.scope_start()..] {
                    merged.extend(scope.clone());
                }
            }
            for (name, variable) in merged {
                if exported.is_some_and(|flag| flag != variable.exported)
                    || pathvar.is_some_and(|flag| flag != variable.pathvar)
                {
                    continue;
                }
                let line = if names {
                    format!("{name}\n")
                } else {
                    format!("{name} {}\n", variable.values.join(" "))
                };
                self.write(line.as_bytes(), host, output)?;
            }
            self.status = 0;
            return Ok(false);
        }
        let expression = WString::from(args[0].as_str());
        let name_string = variable_indices::variable_name(&expression).to_string();
        let name = &name_string;
        let indexed = variable_indices::has_indexes(&expression);
        valid_name(name)?;
        if matches!(
            name.as_str(),
            "status" | "pipestatus" | "last_pid" | "fish_pid" | "PWD"
        ) {
            return Err(Error::new(format!("set: {name} is read-only")));
        }
        if erase && args.len() != 1 {
            return Err(Error::new("set --erase: expected one variable"));
        }
        let visible_scope = self.scopes[self.scope_start()..]
            .iter()
            .rposition(|scope| scope.contains_key(name))
            .map(|index| index + self.scope_start());
        let implicit_universal = !local
            && !global
            && !universal
            && visible_scope.is_none()
            && !self.variables.contains_key(name)
            && self.universals.vars.contains_key(name);
        // Explicit universal append/prepend starts with the visible list when
        // another scope shadows it, as on POSIX. Without a shadow the locked
        // transaction must fetch the latest universal list to avoid lost writes.
        let shadow_values = (universal
            && (visible_scope.is_some() || self.variables.contains_key(name)))
        .then(|| self.variable(name).unwrap().values.clone());
        let assign =
            |old: Option<&Variable>, selected: Option<&Variable>| -> Result<Option<Variable>> {
                let indexes = variable_indices::parse(
                    &expression,
                    selected.map_or(0, |value| value.values.len()),
                )
                .map_err(|error| index_error(error, if erase { 1 } else { 2 }))?;
                if erase && indexes.is_empty() {
                    return Ok(None);
                }
                if erase && selected.is_none() {
                    return Err(Error {
                        message: String::new(),
                        status: 1,
                    });
                }
                let pathvar = pathvar
                    .or_else(|| old.map(|var| var.pathvar))
                    .unwrap_or_else(|| name.ends_with("PATH"));
                let mut values = args[1..].to_vec();
                if !indexes.is_empty() {
                    let original = selected
                        .map(|value| value.values.clone())
                        .unwrap_or_default();
                    values = if erase {
                        variable_indices::erased_at_indexes(original, indexes)
                    } else {
                        if append || prepend {
                            return Err(Error::new(
                                "set: cannot append or prepend when assigning to a slice",
                            ));
                        }
                        if indexes.iter().any(|&index| index <= 0) {
                            return Err(Error::new("set: array index out of bounds"));
                        }
                        if indexes.len() != values.len() {
                            return Err(Error::new(format!(
                                "set: given {} indexes but {} values",
                                indexes.len(),
                                values.len()
                            )));
                        }
                        let mut result = original;
                        for (index, value) in indexes.into_iter().zip(values) {
                            let index = index as usize;
                            if index > variable_indices::MAX_INDEX_COUNT {
                                return Err(Error::new(
                                    "set: array index exceeds the native variable size limit",
                                ));
                            }
                            if index > result.len() {
                                result.resize(index, String::new());
                            }
                            result[index - 1] = value;
                        }
                        result
                    };
                } else if append || prepend {
                    let mut combined = Vec::new();
                    if prepend {
                        combined.extend(values.clone());
                    }
                    combined.extend(
                        shadow_values
                            .clone()
                            .or_else(|| old.map(|value| value.values.clone()))
                            .unwrap_or_default(),
                    );
                    if append {
                        combined.extend(values);
                    }
                    values = combined;
                }
                if pathvar {
                    values = values
                        .into_iter()
                        .flat_map(|value| value.split(';').map(str::to_owned).collect::<Vec<_>>())
                        .collect();
                }
                Ok(Some(Variable {
                    values,
                    exported: exported
                        .or_else(|| old.map(|var| var.exported))
                        .unwrap_or(false),
                    pathvar,
                }))
            };
        if implicit_universal || (universal && self.universals.enabled()) {
            let assigned = self.set_universal(
                name,
                |old| assign(old, old),
                erase && !indexed,
                no_event,
                host,
            )?;
            if erase && indexed {
                self.status = 0;
                return Ok(false);
            }
            return Ok(assigned);
        }
        if universal {
            // POSIX --no-config redirects universal assignments to globals;
            // explicit universal queries and erases still see the empty scope.
            if erase {
                // Validate slices even though --no-config has no universal scope.
                assign(None, None)?;
                self.status = 4;
                return Ok(false);
            }
            global = true;
        }
        let selected = self.set_scope_variable(name, local, global, false);
        let value = assign(self.variable(name), selected)?;
        let function_scope = self.function_frames.last().map(|frame| frame.local_scope);
        let scope = if global {
            &mut self.variables
        } else if local && erase && !indexed {
            if let Some(index) = visible_scope {
                &mut self.scopes[index]
            } else {
                self.status = 1;
                return Ok(false);
            }
        } else if local && !self.scopes.is_empty() {
            self.scopes.last_mut().unwrap()
        } else if let Some(index) = visible_scope {
            &mut self.scopes[index]
        } else if self.variables.contains_key(name) {
            &mut self.variables
        } else if let Some(index) = function_scope {
            &mut self.scopes[index]
        } else {
            &mut self.variables
        };
        let existed = if let Some(mut value) = value {
            // Export state belongs to the target scope. A new local/global
            // shadow does not inherit export merely because its parent did.
            value.exported = exported
                .or_else(|| scope.get(name).map(|variable| variable.exported))
                .unwrap_or(false);
            scope.insert(name.clone(), value);
            true
        } else {
            scope.remove(name).is_some()
        };
        if name == "fish_function_path" {
            self.refresh_function_paths();
        }
        if !no_event && existed {
            self.fire_variable_event(name, erase && !indexed, host)?;
        }
        if erase {
            self.status = i32::from(!existed);
        }
        Ok(!erase)
    }
}

fn index_error(error: variable_indices::InvalidIndex, status: i32) -> Error {
    Error {
        message: format!("set: Invalid index starting at '{}'", error.0),
        status,
    }
}
