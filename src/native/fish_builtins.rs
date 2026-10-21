use super::*;

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
            | ":"
            | "false"
            | "echo"
            | "set"
            | "pwd"
            | "cd"
            | "count"
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
            | "string"
            | "contains"
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
            if let Some(body) = self.functions.get(name).cloned() {
                // Exported caller locals become inherited variables, while ordinary
                // caller locals remain hidden across the function boundary.
                let mut scope = BTreeMap::new();
                for locals in &self.scopes[self.scope_start()..] {
                    scope.extend(locals.clone());
                }
                scope.retain(|_, variable| variable.exported);
                scope.insert(
                    "argv".into(),
                    Variable {
                        values: args.to_vec(),
                        exported: false,
                    },
                );
                self.function_frames.push(self.scopes.len());
                self.scopes.push(scope);
                self.function_depth += 1;
                let saved_loops = std::mem::replace(&mut self.loops, 0);
                let saved_sources = std::mem::replace(&mut self.source_depth, 0);
                let result = self.eval_source(&body, host, output);
                self.source_depth = saved_sources;
                self.loops = saved_loops;
                self.function_depth -= 1;
                self.scopes.pop();
                self.function_frames.pop();
                if self.flow == Flow::Return {
                    self.flow = Flow::Normal;
                }
                return result;
            }
        }
        if decoration == StatementDecoration::Command {
            return self.external(name, args, host, output);
        }
        if self.extra_builtin(name, args, host, output)? {
            return Ok(());
        }
        match name {
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
            "set" => self.set(args, host, output)?,
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
                    },
                );
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
            "test" | "[" => {
                let args = if name == "[" {
                    if args.last().map(String::as_str) != Some("]") {
                        return Err(Error::new("[: missing ]"));
                    }
                    &args[..args.len() - 1]
                } else {
                    args
                };
                self.status = i32::from(!test(args)?);
            }
            "source" | "." => {
                let path = args
                    .first()
                    .ok_or_else(|| Error::new("source: expected a path"))?;
                let source = std::fs::read_to_string(path)?;
                let mut scope = BTreeMap::new();
                scope.insert(
                    "argv".into(),
                    Variable {
                        values: args[1..].to_vec(),
                        exported: false,
                    },
                );
                self.scopes.push(scope);
                self.source_depth += 1;
                let result = self.eval_source(&source, host, output);
                self.source_depth -= 1;
                self.scopes.pop();
                if self.flow == Flow::Return {
                    self.flow = Flow::Normal;
                }
                return result;
            }
            "eval" => return self.eval_source(&args.join(" "), host, output),
            // Keep Fish builtins reserved while their native implementations are pending.
            "printf" | "string" | "math" | "read" | "status" | "functions" | "type" | "command"
            | "builtin" | "argparse" | "contains" | "random" | "set_color" | "jobs" | "fg"
            | "bg" | "wait" | "disown" | "exec" | "ulimit" | "umask" | "bind" | "commandline"
            | "complete" | "emit" | "history" | "abbr" => {
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
        let mut env = BTreeMap::new();
        let mut merged = self.variables.clone();
        for scope in &self.scopes[self.scope_start()..] {
            merged.extend(scope.clone());
        }
        for (name, var) in merged {
            if var.exported {
                env.insert(
                    name.as_bytes().to_vec(),
                    var.values
                        .join(if name.ends_with("PATH") { ";" } else { " " })
                        .into_bytes(),
                );
            }
        }
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

    fn set(&mut self, args: &[String], host: &mut impl Host, output: &mut Output) -> Result<()> {
        let mut local = false;
        let mut global = false;
        let mut exported = None;
        let mut query = false;
        let mut erase = false;
        let mut append = false;
        let mut prepend = false;
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
                "--export" => exported = Some(true),
                "--unexport" => exported = Some(false),
                "--query" => query = true,
                "--erase" => erase = true,
                "--append" => append = true,
                "--prepend" => prepend = true,
                _ if !arg.starts_with("--") => {
                    for flag in arg[1..].chars() {
                        match flag {
                            'l' => local = true,
                            'g' => global = true,
                            'x' => exported = Some(true),
                            'u' => exported = Some(false),
                            'q' => query = true,
                            'e' => erase = true,
                            'a' => append = true,
                            'p' => prepend = true,
                            _ => return Err(unsupported(&format!("set -{flag}"))),
                        }
                    }
                }
                _ => return Err(unsupported(&format!("set {arg}"))),
            }
            index += 1;
        }
        if local && global {
            return Err(Error::new("set: local and global scopes conflict"));
        }
        let args = &args[index..];
        if query {
            if erase || append || prepend {
                return Err(Error::new("set: incompatible options"));
            }
            for name in args {
                valid_name(name)?;
            }
            self.status = args
                .iter()
                .filter(|name| {
                    let variable = if global {
                        self.variables.get(*name)
                    } else if local {
                        self.scopes[self.scope_start()..]
                            .iter()
                            .rev()
                            .find_map(|scope| scope.get(*name))
                    } else {
                        self.variable(name)
                    };
                    if !local
                        && exported != Some(true)
                        && matches!(name.as_str(), "status" | "fish_pid")
                    {
                        return false;
                    }
                    variable.is_none_or(|variable| {
                        exported.is_some_and(|exported| exported != variable.exported)
                    })
                })
                .count()
                .min(255) as i32;
            return Ok(());
        }
        if erase && (append || prepend || exported.is_some()) {
            return Err(Error::new("set: incompatible erase options"));
        }
        let Some(name) = args.first() else {
            let mut merged = self.variables.clone();
            for scope in &self.scopes[self.scope_start()..] {
                merged.extend(scope.clone());
            }
            for (name, var) in merged {
                self.write(
                    format!("{name} {}\n", var.values.join(" ")).as_bytes(),
                    host,
                    output,
                )?;
            }
            self.status = 0;
            return Ok(());
        };
        valid_name(name)?;
        if matches!(
            name.as_str(),
            "status" | "pipestatus" | "last_pid" | "fish_pid" | "PWD"
        ) {
            return Err(Error::new(format!("set: {name} is read-only")));
        }
        let mut values = args[1..].to_vec();
        let old = self.variable(name).cloned();
        if append || prepend {
            let mut combined = Vec::new();
            if prepend {
                combined.extend(values.clone());
            }
            combined.extend(old.as_ref().map(|v| v.values.clone()).unwrap_or_default());
            if append {
                combined.extend(values);
            }
            values = combined;
        }
        let visible_scope = self.scopes[self.scope_start()..]
            .iter()
            .rposition(|scope| scope.contains_key(name))
            .map(|index| index + self.scope_start());
        let function_scope = self.function_frames.last().copied();
        let scope = if global {
            &mut self.variables
        } else if local && erase {
            if let Some(index) = visible_scope {
                &mut self.scopes[index]
            } else {
                self.status = 1;
                return Ok(());
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
        if erase {
            if args.len() != 1 {
                return Err(Error::new("set --erase: expected one variable"));
            }
            self.status = i32::from(scope.remove(name).is_none());
        } else {
            scope.insert(
                name.clone(),
                Variable {
                    values,
                    exported: exported.unwrap_or_else(|| old.is_some_and(|v| v.exported)),
                },
            );
            // Fish assignment preserves the preceding command's status.
        }
        Ok(())
    }
}

fn test(args: &[String]) -> Result<bool> {
    let strings: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match strings.as_slice() {
        [] => false,
        [value] => !value.is_empty(),
        ["!", rest @ ..] => {
            return test(&rest.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).map(|v| !v);
        }
        ["-n", value] => !value.is_empty(),
        ["-z", value] => value.is_empty(),
        ["-e", path] => std::path::Path::new(path).exists(),
        ["-f", path] => std::path::Path::new(path).is_file(),
        ["-d", path] => std::path::Path::new(path).is_dir(),
        [left, "=" | "==", right] => left == right,
        [left, "!=", right] => left != right,
        [
            left,
            op @ ("-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"),
            right,
        ] => {
            let left = left
                .parse::<f64>()
                .map_err(|_| Error::new("test: expected a number"))?;
            let right = right
                .parse::<f64>()
                .map_err(|_| Error::new("test: expected a number"))?;
            match *op {
                "-eq" => left == right,
                "-ne" => left != right,
                "-lt" => left < right,
                "-le" => left <= right,
                "-gt" => left > right,
                _ => left >= right,
            }
        }
        _ => return Err(unsupported("this test expression")),
    })
}
