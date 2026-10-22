//! Declarative native completions, stored with the evaluator's session state.
use super::*;
use crate::native::interactive_completion::{CompletionContext, CompletionEngine, CompletionSpec};
use crate::tokenizer::{TokFlags, TokenType, Tokenizer};
use fish_wcstringutil::escape::{UnescapeFlags, UnescapeStringStyle, unescape_string};

impl Session {
    pub(super) fn complete_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let parsed_args = completion_options(args)?;
        let args = &parsed_args;
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
                "-a" | "--arguments" => spec.argument_sources = vec![value()?],
                "-n" | "--condition" => spec.conditions.push(value()?),
                "-C" | "--do-complete" => query = Some(value()?),
                "-e" | "--erase" => erase = true,
                "-f" | "--no-files" => spec.no_files = true,
                "-r" | "--require-parameter" => spec.requires_argument = true,
                "-x" | "--exclusive" => {
                    spec.no_files = true;
                    spec.requires_argument = true;
                }
                "--" if index == args.len() => (),
                _ => {
                    return Err(unsupported(&format!("complete option {option}")));
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
            let io = crate::native::interactive_runtime::CompletionIo::new()?;
            let result = self.completion_query_style(
                &line,
                line.len(),
                &std::env::current_dir()?,
                &mut crate::native::interactive_runtime::CompletionHost::input_only(host, &io),
                &mut engine,
                true,
            );
            let (_, candidates) = result?;
            for candidate in candidates {
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
                for source in registration.argument_sources {
                    words.extend(["-a".into(), quote(&source)]);
                }
                for condition in registration.conditions {
                    words.extend(["-n".into(), quote(&condition)]);
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

    pub(in crate::native) fn completion_query(
        &mut self,
        line: &str,
        pos: usize,
        cwd: &std::path::Path,
        host: &mut impl Host,
        engine: &mut CompletionEngine,
    ) -> Result<(usize, Vec<rustyline::completion::Pair>)> {
        self.completion_query_style(line, pos, cwd, host, engine, false)
    }

    fn completion_query_style(
        &mut self,
        line: &str,
        pos: usize,
        cwd: &std::path::Path,
        host: &mut impl Host,
        engine: &mut CompletionEngine,
        shell_output: bool,
    ) -> Result<(usize, Vec<rustyline::completion::Pair>)> {
        if self.completion_depth >= 24 {
            return Err(Error::new("maximum completion recursion depth exceeded"));
        }
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let flow = self.flow;
        let input_mode_blocked = std::mem::replace(&mut self.input_mode_blocked, true);
        let saved_context = self.completion_line.take();
        self.completion_depth += 1;
        self.flow = Flow::Normal;

        // Registrations and wrapping relationships are fixed for this query;
        // evaluator mutations remain in the live session for subsequent queries.
        let (commands, path, variables) = self.completion_context();
        engine.set_context(commands, path, variables);
        engine.set_environment(self.completion_environment());
        engine.set_input_language(self.input_language);
        engine.set_registrations(self.completions.clone());
        engine.set_wrappers(self.completion_wrappers());
        let mut conditions = BTreeMap::new();
        let result = engine.complete_evaluated(
            line,
            pos,
            cwd,
            shell_output,
            &mut |context, spec, arguments| {
                if host.cancelled() || self.flow == Flow::Exit {
                    return Ok(None);
                }
                self.completion_line = Some(context.clone());
                for condition in &spec.conditions {
                    let key = (context.clone(), condition.clone());
                    let enabled = if let Some(enabled) = conditions.get(&key) {
                        *enabled
                    } else {
                        let mut output = Output {
                            capture: true,
                            ..Default::default()
                        };
                        self.scopes.push(BTreeMap::new());
                        let result = self.eval_source(condition, host, &mut output);
                        self.scopes.pop();
                        let enabled = self.status == 0;
                        self.status = status;
                        self.pipeline = pipeline.clone();
                        result?;
                        conditions.insert(key, enabled);
                        enabled
                    };
                    if !enabled || self.flow == Flow::Exit || host.cancelled() {
                        return Ok(None);
                    }
                }
                let mut values = if arguments {
                    spec.arguments.clone()
                } else {
                    Vec::new()
                };
                if arguments {
                    for source in &spec.argument_sources {
                        for word in argument_words(source)? {
                            values.extend(self.expand(&word, host)?);
                            if values.len() > 100_000 {
                                return Err(Error::new("too many completion arguments"));
                            }
                            if self.flow == Flow::Exit || host.cancelled() {
                                return Ok(None);
                            }
                        }
                    }
                }
                self.status = status;
                self.pipeline = pipeline.clone();
                Ok(Some(values))
            },
        );
        self.status = status;
        self.pipeline = pipeline;
        if self.flow != Flow::Exit {
            self.flow = flow;
        }
        self.input_mode_blocked = input_mode_blocked;
        self.completion_line = saved_context;
        self.completion_depth -= 1;
        result.map(|(start, candidates)| {
            (
                start,
                if host.cancelled() {
                    Vec::new()
                } else {
                    candidates
                },
            )
        })
    }

    /// Query completion input using the same lexical extents as POSIX Fish.
    /// Editing the outer reader from a generator is deliberately unavailable.
    pub(super) fn commandline_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        use crate::commandline_extent::{get_job_extent, get_process_extent, get_token_extent};
        let mut part = 'b';
        let mut tokens = None;
        let mut cut = false;
        let mut cursor_query = false;
        let mut line_query = false;
        let mut column_query = false;
        let mut override_input = None;
        let mut index = 0;
        while index < args.len() {
            let option = &args[index];
            index += 1;
            let flags = match option.as_str() {
                "--current-buffer" => "b",
                "--current-job" => "j",
                "--current-process" => "p",
                "--current-token" => "t",
                "--cut-at-cursor" => "c",
                "--tokenize" => "o",
                "--tokens-expanded" => "x",
                "--tokens-raw" => "R",
                "--cursor" => "C",
                "--line" => "L",
                "--column" => "Q",
                "--input" | "-I" => {
                    override_input = Some(
                        args.get(index)
                            .ok_or_else(|| Error::new("commandline: --input requires an argument"))?
                            .clone(),
                    );
                    index += 1;
                    continue;
                }
                "--" if index == args.len() => continue,
                _ if option.starts_with('-') && !option.starts_with("--") => &option[1..],
                _ => {
                    return Err(unsupported(&format!(
                        "commandline editing or option {option}"
                    )));
                }
            };
            for flag in flags.chars() {
                match flag {
                    'b' | 'j' | 'p' | 't' => part = flag,
                    'o' | 'x' | 'R' => {
                        if tokens.is_some_and(|previous| previous != flag) {
                            return Err(Error::new(
                                "commandline: token output modes are mutually exclusive",
                            ));
                        }
                        tokens = Some(flag);
                    }
                    'c' => cut = true,
                    'C' => cursor_query = true,
                    'L' => line_query = true,
                    'Q' => column_query = true,
                    _ => return Err(unsupported(&format!("commandline option -{flag}"))),
                }
            }
        }
        let context = if let Some(line) = override_input {
            CompletionContext {
                cursor: line.len(),
                line,
            }
        } else {
            self.completion_line.clone().ok_or_else(|| Error {
                message: "commandline: no completion input is available".into(),
                status: 1,
            })?
        };
        let buffer = WString::from(context.line.as_str());
        let cursor = context.line[..context.cursor].chars().count();
        let range = match part {
            'j' => get_job_extent(&buffer, cursor, None),
            'p' => get_process_extent(&buffer, cursor, None),
            't' => get_token_extent(&buffer, cursor).0,
            _ => 0..buffer.len(),
        };
        let mut values = Vec::new();
        if cursor_query {
            values.push(cursor.saturating_sub(range.start).to_string());
        } else if line_query {
            values.push((buffer[..cursor].chars().filter(|c| *c == '\n').count() + 1).to_string());
        } else if column_query {
            values.push(
                buffer[..cursor]
                    .chars()
                    .rev()
                    .take_while(|c| *c != '\n')
                    .count()
                    .to_string(),
            );
        } else if let Some(mode) = tokens {
            let selected = &buffer[range.clone()];
            let mut add = |token: &wstr| {
                let raw = token.to_string();
                match mode {
                    'R' => values.push(raw),
                    'o' => values.push(
                        unescape_string(
                            token,
                            UnescapeStringStyle::Script(UnescapeFlags {
                                incomplete: true,
                                ..Default::default()
                            }),
                        )
                        .map_or(raw, |value| value.to_string()),
                    ),
                    'x' => {
                        let expanded = skip_substitutions(token).and_then(|word| {
                            expand::word_with(&word, |request| match request {
                                expand::Request::Variable(name) => Ok(self.values(name)),
                                expand::Request::VariableSeparator(name) => Ok(vec![
                                    if self.variable(name).is_some_and(|var| var.pathvar) {
                                        ";"
                                    } else {
                                        " "
                                    }
                                    .into(),
                                ]),
                                expand::Request::Command { .. } => Err(Error::new(
                                    "typed command substitution cannot be evaluated",
                                )),
                            })
                        });
                        match expanded {
                            Ok(words) if values.len().saturating_add(words.len()) <= 512 => {
                                values.extend(words)
                            }
                            _ => values.push(raw),
                        }
                    }
                    _ => unreachable!(),
                }
            };
            if part == 't' {
                add(if cut {
                    &selected[..cursor.saturating_sub(range.start)]
                } else {
                    selected
                });
            } else {
                let mut redirect = false;
                for token in Tokenizer::new(
                    selected,
                    TokFlags {
                        accept_unfinished: true,
                        ..Default::default()
                    },
                ) {
                    if cut && token.end() >= cursor.saturating_sub(range.start) {
                        break;
                    }
                    let target = redirect;
                    redirect = token.type_ == TokenType::Redirect;
                    if token.type_ == TokenType::String && !target {
                        add(&selected[token.offset()..token.end()]);
                    }
                }
            }
        } else {
            let end = if cut { cursor } else { range.end };
            values.push(buffer[range.start..end].to_string());
        }
        for value in values {
            self.write(format!("{value}\n").as_bytes(), host, output)?;
        }
        self.status = 0;
        Ok(())
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn argument_words(source: &str) -> Result<Vec<String>> {
    let source = WString::from(source);
    let mut values = Vec::new();
    for token in Tokenizer::new(
        &source,
        TokFlags {
            argument_list: true,
            ..Default::default()
        },
    ) {
        if token.type_ == TokenType::End {
            continue;
        }
        if token.type_ != TokenType::String {
            return Err(Error::new("complete: invalid argument source"));
        }
        let word =
            source[token.offset as usize..(token.offset + token.length) as usize].to_string();
        values.push(word);
    }
    Ok(values)
}

/// Expand grouped short options without evaluating their attached arguments.
fn completion_options(args: &[String]) -> Result<Vec<String>> {
    let mut parsed = Vec::new();
    let mut expects_value = false;
    for argument in args {
        if expects_value {
            parsed.push(argument.clone());
            expects_value = false;
        } else if let Some((name, value)) = argument
            .strip_prefix("--")
            .and_then(|arg| arg.split_once('='))
        {
            parsed.extend([format!("--{name}"), value.into()]);
        } else if argument.starts_with("--") || !argument.starts_with('-') || argument == "-" {
            expects_value = matches!(
                argument.as_str(),
                "--command"
                    | "--short-option"
                    | "--long-option"
                    | "--old-option"
                    | "--description"
                    | "--arguments"
                    | "--condition"
                    | "--do-complete"
            );
            parsed.push(argument.clone());
        } else {
            let mut options = argument[1..].char_indices().peekable();
            while let Some((_, option)) = options.next() {
                parsed.push(format!("-{option}"));
                if "cslodanC".contains(option) {
                    if let Some((offset, _)) = options.peek() {
                        parsed.push(argument[1 + offset..].into());
                    } else {
                        expects_value = true;
                    }
                    break;
                }
            }
        }
    }
    Ok(parsed)
}

/// Preserve substitutions as literal input while allowing safe variable/glob
/// expansion. The shared scanner handles nested and quoted substitutions.
fn skip_substitutions(source: &wstr) -> Result<String> {
    let mut cursor = 0;
    let mut copied = 0;
    let mut quoted = false;
    let mut result = String::new();
    loop {
        let mut dollar = false;
        let substitution = crate::commandline_extent::locate_cmdsubst_range(
            source,
            &mut cursor,
            false,
            Some(&mut quoted),
            Some(&mut dollar),
        )
        .map_err(|()| Error::new("incomplete command substitution"))?;
        let Some(substitution) = substitution else {
            break;
        };
        let start = substitution.opening_paren_offset() - usize::from(dollar);
        result.push_str(&source[copied..start].to_string());
        let literal = source[start..substitution.end()].to_string();
        if quoted {
            // POSIX's skip mode forwards the complete original token when a
            // quoted substitution prevents expansion, including its quotes.
            return Err(Error::new("quoted substitution remains raw"));
        }
        result.push_str(&quote(&literal));
        copied = substitution.end();
    }
    result.push_str(&source[copied..].to_string());
    Ok(result)
}
