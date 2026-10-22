//! Declarative native completions, stored with the evaluator's session state.
use super::*;
use crate::native::interactive_completion::{CompletionEngine, CompletionSpec};
use crate::tokenizer::{TokFlags, TokenType, Tokenizer};

impl Session {
    pub(super) fn complete_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut spec = CompletionSpec::default();
        let mut commands = Vec::new();
        let mut erase = false;
        let mut query = None;
        let mut index = 0;
        while index < args.len() {
            let option = &args[index];
            index += 1;
            let mut value = || -> Result<String> {
                let value = args
                    .get(index)
                    .ok_or_else(|| Error::new(format!("complete: {option} requires an argument")))?
                    .clone();
                index += 1;
                Ok(value)
            };
            match option.as_str() {
                "-c" | "--command" => commands.push(value()?),
                "-s" | "--short-option" => {
                    let value = value()?;
                    let mut chars = value.chars();
                    spec.short_option = chars.next();
                    if spec.short_option.is_none() || chars.next().is_some() {
                        return Err(Error::new("complete: short option must be one character"));
                    }
                }
                "-l" | "--long-option" => spec.long_option = Some(value()?),
                "-o" | "--old-option" => spec.old_option = Some(value()?),
                "-d" | "--description" => spec.description = value()?,
                "-a" | "--arguments" => spec.arguments.extend(literal_arguments(&value()?)?),
                "-C" | "--do-complete" => query = Some(value()?),
                "-e" | "--erase" => erase = true,
                "-f" | "--no-files" => spec.no_files = true,
                "-r" | "--require-parameter" => spec.requires_argument = true,
                "-x" | "--exclusive" => {
                    spec.no_files = true;
                    spec.requires_argument = true;
                }
                "--" if index == args.len() => (),
                _ if option.starts_with('-')
                    && option.len() > 2
                    && option[1..].chars().all(|c| "efrx".contains(c)) =>
                {
                    erase |= option.contains('e');
                    spec.no_files |= option.contains(['f', 'x']);
                    spec.requires_argument |= option.contains(['r', 'x']);
                }
                _ => {
                    return Err(unsupported(&format!(
                        "complete option {option}; native completions accept literal choices without conditions or command substitutions"
                    )));
                }
            }
        }
        if let Some(line) = query {
            if !commands.is_empty() || erase {
                return Err(Error::new(
                    "complete: -C cannot be combined with a registration",
                ));
            }
            let mut engine = CompletionEngine::new("fish");
            let (commands, path, variables) = self.completion_context();
            engine.set_context(commands, path, variables);
            engine.set_environment(self.completion_environment());
            engine.set_input_language(self.input_language);
            engine.set_registrations(self.completions.clone());
            for candidate in engine
                .complete(&line, line.len(), &std::env::current_dir()?)
                .1
            {
                self.write(format!("{}\n", candidate.display).as_bytes(), host, output)?;
            }
        } else if commands.is_empty() {
            if !args.is_empty() {
                return Err(Error::new("complete: a registration requires -c COMMAND"));
            }
            for registration in self.completions.clone() {
                let mut words = vec!["complete".into(), "-c".into(), quote(&registration.command)];
                for (flag, value) in [
                    ("-s", registration.short_option.map(|c| c.to_string())),
                    ("-l", registration.long_option),
                    ("-o", registration.old_option),
                ] {
                    if let Some(value) = value {
                        words.extend([flag.into(), quote(&value)]);
                    }
                }
                if registration.no_files {
                    words.push("-f".into());
                }
                if registration.requires_argument {
                    words.push("-r".into());
                }
                if !registration.description.is_empty() {
                    words.extend(["-d".into(), quote(&registration.description)]);
                }
                if !registration.arguments.is_empty() {
                    words.extend([
                        "-a".into(),
                        quote(
                            &registration
                                .arguments
                                .iter()
                                .map(|word| quote(word))
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                    ]);
                }
                self.write(format!("{}\n", words.join(" ")).as_bytes(), host, output)?;
            }
        } else {
            for command in commands {
                if command.is_empty() {
                    return Err(Error::new("complete: command must not be empty"));
                }
                if erase {
                    self.completions.retain(|existing| {
                        existing.command != command
                            || (spec.short_option.is_some()
                                && existing.short_option != spec.short_option)
                            || (spec.long_option.is_some()
                                && existing.long_option != spec.long_option)
                            || (spec.old_option.is_some() && existing.old_option != spec.old_option)
                    });
                } else {
                    spec.command = command;
                    self.completions.push(spec.clone());
                }
            }
        }
        self.status = 0;
        Ok(())
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn literal_arguments(source: &str) -> Result<Vec<String>> {
    let source = WString::from(source);
    let mut values = Vec::new();
    for token in Tokenizer::new(&source, TokFlags::default()) {
        if token.type_ == TokenType::End {
            continue;
        }
        if token.type_ != TokenType::String {
            return Err(unsupported("nonliteral completion arguments"));
        }
        let word =
            source[token.offset as usize..(token.offset + token.length) as usize].to_string();
        if word.contains(['$', '(', ')', '`']) {
            return Err(unsupported(
                "dynamic completion arguments; register literal choices",
            ));
        }
        values.push(unescape_literal(&word)?);
    }
    Ok(values)
}
