//! Native evaluation and caller-local bindings for the shared argparse parser.
use super::*;
use crate::argparse::{self, Context, ParseError};

struct NativeContext<'a, H> {
    session: &'a mut Session,
    host: &'a mut H,
    output: &'a mut Output,
}

enum Failure {
    Status(i32),
    Host(Error),
}

impl<H: Host> Context for NativeContext<'_, H> {
    type Error = Failure;

    fn error(&mut self, name: &wstr, error: ParseError<'_>, status: i32) -> Failure {
        let message = match error {
            ParseError::MissingSeparator => "Missing -- separator".into(),
            ParseError::Exclusive(a, b) => format!("{a} {b}: options cannot be used together"),
            ParseError::InvalidExclusiveSet(flags) => {
                format!("exclusive flag string '{flags}' is not valid")
            }
            ParseError::InvalidExclusiveFlag(flag) => {
                format!("exclusive flag '{flag}' is not valid")
            }
            ParseError::ImplicitIntModifier(flag, modifier) => format!(
                "Implicit int short flag '{flag}' does not allow modifiers like '{modifier}'"
            ),
            ParseError::InvalidOptionSpec(spec, c) => {
                format!("Invalid option spec '{spec}' at char '{c}'")
            }
            ParseError::DuplicateShort(flag) => format!("Short flag '{flag}' already defined"),
            ParseError::DuplicateImplicitInt(flag) => {
                format!("Implicit int flag '{flag}' already defined")
            }
            ParseError::EmptyOptionSpec => {
                "An option spec must have at least a short or a long flag".into()
            }
            ParseError::InvalidShort(flag) => {
                format!("Short flag '{flag}' invalid, must be alphanum or '#'")
            }
            ParseError::DuplicateLong(flag) => format!("Long flag '{flag}' already defined"),
            ParseError::TooManyLongOptions => "Too many long-only options".into(),
            ParseError::InvalidUnknownArguments(value) => {
                format!("Invalid --unknown-arguments value '{value}'")
            }
            ParseError::InvalidMinArgs(value) => format!("Invalid --min-args value '{value}'"),
            ParseError::InvalidMaxArgs(value) => format!("Invalid --max-args value '{value}'"),
            ParseError::MissingArgument(option) => {
                let option = option.to_string();
                let option = if option.starts_with('-') && !option.starts_with("--") {
                    format!("-{}", option.chars().last().unwrap())
                } else {
                    option
                };
                format!("{option}: option requires an argument")
            }
            ParseError::MissingUnknownArgument(option) => {
                format!("{option}: option requires an argument")
            }
            ParseError::UnexpectedArgument(option) => {
                format!("{option}: option does not take an argument")
            }
            ParseError::UnknownOption(option) => format!("{option}: unknown option"),
            ParseError::MinArgs(min, actual) => {
                format!("expected >= {min} arguments; got {actual}")
            }
            ParseError::MaxArgs(max, actual) => {
                format!("expected <= {max} arguments; got {actual}")
            }
        };
        match self.host.write(
            2,
            format!("{name}: {message}\n").as_bytes(),
            &self.output.redirects,
            self.output.capture,
        ) {
            Ok(bytes) => match self.output.append(bytes) {
                Ok(()) => Failure::Status(status),
                Err(error) => Failure::Host(error),
            },
            Err(error) => Failure::Host(error),
        }
    }

    fn usage_error(&mut self, _: &wstr) {
        // The native evaluator does not yet retain source locations for stack traces.
    }

    fn validate(
        &mut self,
        name: &wstr,
        flag: &wstr,
        value: &wstr,
        command: &wstr,
    ) -> std::result::Result<(), Failure> {
        let mut scope = BTreeMap::new();
        for locals in &self.session.scopes[self.session.scope_start()..] {
            scope.extend(locals.clone());
        }
        scope.retain(|_, variable: &mut Variable| variable.exported);
        for (name, value) in [
            ("_argparse_cmd", name),
            ("_flag_name", flag),
            ("_flag_value", value),
        ] {
            scope.insert(
                name.into(),
                Variable {
                    values: vec![value.to_string()],
                    exported: true,
                    pathvar: false,
                },
            );
        }
        // POSIX argparse opens a new variable scope before exec_subshell:
        // only exported caller locals cross that boundary.
        let local_scope = self.session.scopes.len();
        let caller_name = self
            .session
            .function_frames
            .last()
            .map_or_else(|| "argparse".into(), |frame| frame.name.clone());
        self.session.function_frames.push(functions::FunctionFrame {
            name: caller_name,
            local_scope,
            visibility_start: local_scope,
        });
        self.session.scopes.push(scope);
        let status = self.session.status;
        let pipeline = self.session.pipeline.clone();
        let flow = std::mem::replace(&mut self.session.flow, Flow::Normal);
        let blocked = std::mem::replace(&mut self.session.input_mode_blocked, true);
        let mut output = Output {
            capture: true,
            ..Default::default()
        };
        let result = self
            .session
            .eval_source(&command.to_string(), self.host, &mut output);
        let validation_status = self.session.status;
        self.session.scopes.pop();
        self.session.function_frames.pop();
        self.session.status = status;
        self.session.pipeline = pipeline;
        self.session.flow = flow;
        self.session.input_mode_blocked = blocked;
        result.map_err(Failure::Host)?;
        for line in
            expand::split_separated_substitution(&output.bytes, false).map_err(Failure::Host)?
        {
            let bytes = self
                .host
                .write(
                    2,
                    format!("{line}\n").as_bytes(),
                    &self.output.redirects,
                    self.output.capture,
                )
                .map_err(Failure::Host)?;
            self.output.append(bytes).map_err(Failure::Host)?;
        }
        if validation_status == 0 {
            Ok(())
        } else {
            Err(Failure::Status(validation_status))
        }
    }
}

impl Session {
    pub(super) fn argparse(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let argv: Vec<WString> = std::iter::once("argparse")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut args: Vec<&wstr> = argv.iter().map(WString::as_utfstr).collect();
        let default_name = self
            .function_frames
            .last()
            .map_or("argparse", |frame| frame.name.as_str());
        let default_name = WString::from(default_name);
        let result = argparse::parse(
            &mut args,
            &default_name,
            &mut NativeContext {
                session: self,
                host,
                output,
            },
        );
        match result {
            Ok(parsed) => {
                if parsed.print_help {
                    self.write(
                        b"argparse [OPTIONS] OPTION_SPEC ... -- [ARGUMENTS ...]\n",
                        host,
                        output,
                    )?;
                } else {
                    // EnvStack::set with LOCAL preserves existing target export
                    // flags, does not emit events, and never changes outer argv.
                    for (name, values) in parsed.variables {
                        let name = name.to_string();
                        let exported = self
                            .scopes
                            .last()
                            .and_then(|scope| scope.get(&name))
                            .is_some_and(|variable| variable.exported);
                        let variable = self.binding(
                            &name,
                            values.into_iter().map(|value| value.to_string()).collect(),
                            exported,
                        );
                        self.scopes.last_mut().unwrap().insert(name, variable);
                    }
                }
                self.status = 0;
            }
            Err(Failure::Status(status)) => self.status = status,
            Err(Failure::Host(error)) => return Err(error),
        }
        Ok(())
    }
}
