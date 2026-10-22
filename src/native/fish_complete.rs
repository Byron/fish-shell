//! Declarative native completions, stored with the evaluator's session state.
use super::*;
use crate::native::interactive_completion::{CompletionContext, CompletionEngine, CompletionSpec};
use crate::tokenizer::{TokFlags, TokenType, Tokenizer};
use fish_wcstringutil::escape::{UnescapeFlags, UnescapeStringStyle, unescape_string};
use fish_wgetopt::{ArgType, WGetopter, WOption, wopt};
use fish_widestring::L;
use std::io::IsTerminal;

impl Session {
    pub(super) fn complete_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.refresh_completion_paths();
        const OPTIONS: &[WOption] = &[
            wopt(L!("exclusive"), ArgType::NoArgument, 'x'),
            wopt(L!("no-files"), ArgType::NoArgument, 'f'),
            wopt(L!("force-files"), ArgType::NoArgument, 'F'),
            wopt(L!("require-parameter"), ArgType::NoArgument, 'r'),
            wopt(L!("path"), ArgType::RequiredArgument, 'p'),
            wopt(L!("command"), ArgType::RequiredArgument, 'c'),
            wopt(L!("short-option"), ArgType::RequiredArgument, 's'),
            wopt(L!("long-option"), ArgType::RequiredArgument, 'l'),
            wopt(L!("old-option"), ArgType::RequiredArgument, 'o'),
            wopt(L!("description"), ArgType::RequiredArgument, 'd'),
            wopt(L!("arguments"), ArgType::RequiredArgument, 'a'),
            wopt(L!("erase"), ArgType::NoArgument, 'e'),
            wopt(L!("unauthoritative"), ArgType::NoArgument, 'u'),
            wopt(L!("authoritative"), ArgType::NoArgument, 'A'),
            wopt(L!("condition"), ArgType::RequiredArgument, 'n'),
            wopt(L!("wraps"), ArgType::RequiredArgument, 'w'),
            wopt(L!("do-complete"), ArgType::OptionalArgument, 'C'),
            wopt(L!("help"), ArgType::NoArgument, 'h'),
            wopt(L!("keep-order"), ArgType::NoArgument, 'k'),
            wopt(L!("escape"), ArgType::NoArgument, '\x01'),
            wopt(L!("color"), ArgType::RequiredArgument, '\x02'),
        ];
        let words: Vec<WString> = std::iter::once("complete")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut argv: Vec<&wstr> = words.iter().map(WString::as_utfstr).collect();
        let original = argv.clone();
        let mut parser = WGetopter::new(L!("a:c:p:s:l:o:d:fFrxeuAn:C::w:hk"), OPTIONS, &mut argv);
        let mut spec = CompletionSpec::default();
        let mut commands = Vec::new();
        let mut aliases = Vec::new();
        let mut wraps = Vec::new();
        let mut erase = false;
        let mut query = false;
        let mut query_line = None;
        let mut escape = false;
        let mut color = None;
        while let Some(option) = parser.next_opt() {
            let value = || parser.woptarg.unwrap().to_string();
            match option {
                'c' | 'p' => commands.push((value(), option == 'p')),
                's' | 'l' | 'o' => {
                    let value = value();
                    if value.is_empty() {
                        return Err(completion_usage(format!(
                            "-{option} requires a non-empty argument"
                        )));
                    }
                    if option == 's' {
                        aliases.extend(value.chars().map(|c| ('s', c.to_string())));
                    } else {
                        aliases.push((option, value));
                    }
                }
                'd' => spec.description = value(),
                'a' => spec.argument_sources = vec![value()],
                'n' => spec.conditions.push(value()),
                'w' => wraps.push(value()),
                'C' => {
                    query = true;
                    if parser.woptarg.is_some() {
                        query_line = Some(value());
                    }
                }
                'e' => erase = true,
                'f' => spec.no_files = true,
                'F' => spec.force_files = true,
                'r' => spec.requires_argument = true,
                'x' => {
                    spec.no_files = true;
                    spec.requires_argument = true;
                }
                'k' => spec.keep_order = true,
                'u' | 'A' => (),
                '\x01' => escape = true,
                '\x02' => {
                    color = match value().as_str() {
                        "always" => Some(true),
                        "never" => Some(false),
                        "auto" => None,
                        _ => {
                            return Err(completion_usage("--color expects always, never, or auto"));
                        }
                    };
                }
                'h' => {
                    self.write(
                        b"complete: see https://fishshell.com/docs/current/cmds/complete.html\n",
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
                    return Err(completion_usage(format!(
                        "{}: {detail}",
                        original[parser.wopt_index - 1]
                    )));
                }
                _ => unreachable!("complete option"),
            }
        }
        let index = parser.wopt_index;
        let positional = &argv[index..];
        if positional.len() == 1 && query && query_line.is_none() {
            query_line = Some(positional[0].to_string());
        } else if positional.len() == 1 && !query && !commands.iter().any(|(_, path)| !path) {
            commands.push((positional[0].to_string(), false));
        } else if !positional.is_empty() {
            return Err(completion_usage("too many arguments"));
        }
        if spec.no_files && spec.force_files {
            return Err(completion_usage(
                "--force-files cannot be combined with --no-files or --exclusive",
            ));
        }
        for condition in &spec.conditions {
            check_syntax(condition).map_err(completion_syntax_error)?;
        }
        for source in &spec.argument_sources {
            for word in argument_words(source).map_err(completion_syntax_error)? {
                expand::validate(&word).map_err(completion_syntax_error)?;
            }
        }
        if query {
            if query_line.is_none() && !self.interactive {
                return Err(completion_syntax_error(Error::new(
                    "complete: no commandline is available in non-interactive mode",
                )));
            }
            // An implicit query inside a callback must not re-enter itself.
            if query_line.is_none() && self.completion_depth > 0 {
                self.status = 0;
                return Ok(());
            }
            let context = query_line
                .map(|line| CompletionContext {
                    cursor: line.len(),
                    line,
                })
                .or_else(|| self.completion_line.clone())
                .ok_or_else(|| {
                    completion_syntax_error(Error::new("complete: no commandline is available"))
                })?;
            let mut engine = CompletionEngine::new("fish");
            let io = crate::native::interactive_runtime::CompletionIo::new()?;
            let (_, candidates) = self.completion_query_style(
                &context.line,
                context.cursor,
                &std::env::current_dir()?,
                &mut crate::native::interactive_runtime::CompletionHost::input_only(host, &io),
                &mut engine,
                true,
            )?;
            for candidate in candidates {
                let display = if escape {
                    let description = candidate.display.split_once('\t').map(|(_, text)| text);
                    let value = candidate
                        .replacement
                        .strip_suffix(' ')
                        .unwrap_or(&candidate.replacement);
                    description.map_or_else(|| value.to_owned(), |text| format!("{value}\t{text}"))
                } else {
                    candidate.display
                };
                self.write(format!("{display}\n").as_bytes(), host, output)?;
            }
        } else if !erase
            && aliases.is_empty()
            && wraps.is_empty()
            && !commands.iter().any(|(_, path)| *path)
            && spec.argument_sources.iter().all(String::is_empty)
            && spec.description.is_empty()
            && spec.conditions.is_empty()
            && !spec.no_files
            && !spec.force_files
            && !spec.requires_argument
        {
            let color = color.unwrap_or_else(|| {
                !output.capture && output.redirects.is_empty() && std::io::stdout().is_terminal()
            });
            let palette = crate::native::interactive_colors::Palette::from_variables(
                &self.editor_variables(),
            );
            let resolver = color.then(|| {
                let mut resolver = CompletionEngine::new("fish");
                let (commands, path, variables) = self.completion_context();
                resolver.set_context(commands, path, variables);
                resolver
            });
            let cwd = std::env::current_dir()?;
            for registration in self.completions.clone() {
                if registration.erased
                    || (!commands.is_empty()
                        && !commands
                            .iter()
                            .any(|(command, _)| *command == registration.command))
                {
                    continue;
                }
                let definition = completion_definition(&registration);
                let definition = if color {
                    crate::native::interactive_highlight::highlight_with_palette(
                        &definition,
                        definition.len(),
                        |name| resolver.as_ref().unwrap().command_exists(name, &cwd),
                        &palette,
                    )
                } else {
                    definition
                };
                self.write(format!("{definition}\n").as_bytes(), host, output)?;
            }
        } else {
            for (command, is_path) in commands {
                let matches_target = |existing: &CompletionSpec| {
                    existing.command == command && existing.is_path == is_path
                };
                if erase {
                    let all = aliases.is_empty() && wraps.is_empty();
                    if all
                        && !is_path
                        && !self
                            .completions
                            .iter()
                            .any(|entry| matches_target(entry) && !entry.erased)
                    {
                        self.prevent_completion_autoload(&command);
                    }
                    self.completions.retain(|existing| {
                        !matches_target(existing)
                            || if let Some(target) = &existing.wrap_target {
                                !all && !wraps.contains(target)
                            } else {
                                !all && !aliases.iter().any(|(kind, value)| match kind {
                                    's' => existing.short_option == value.chars().next(),
                                    'l' => existing.long_option.as_ref() == Some(value),
                                    'o' => existing.old_option.as_ref() == Some(value),
                                    _ => unreachable!(),
                                })
                            }
                    });
                    // Preserve explicit ownership even when the last entry was
                    // erased, so built-in fallback guesses stay erased too.
                    if !self
                        .completions
                        .iter()
                        .any(|entry| matches_target(entry) && entry.erased)
                    {
                        self.completions.push(CompletionSpec {
                            command,
                            is_path,
                            erased: true,
                            ..Default::default()
                        });
                    }
                } else {
                    spec.command = command;
                    spec.is_path = is_path;
                    if aliases.is_empty() {
                        self.completions.push(spec.clone());
                    } else {
                        for (kind, value) in &aliases {
                            let mut entry = spec.clone();
                            match kind {
                                's' => entry.short_option = value.chars().next(),
                                'l' => entry.long_option = Some(value.clone()),
                                'o' => entry.old_option = Some(value.clone()),
                                _ => unreachable!(),
                            }
                            self.completions.push(entry);
                        }
                    }
                    if !is_path {
                        for target in &wraps {
                            if !self.completions.iter().any(|entry| {
                                entry.command == spec.command
                                    && entry.wrap_target.as_ref() == Some(target)
                            }) {
                                self.completions.push(CompletionSpec {
                                    command: spec.command.clone(),
                                    wrap_target: Some(target.clone()),
                                    ..Default::default()
                                });
                            }
                        }
                    }
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

        let result = (|| {
            self.autoload_completions(line, pos, cwd, host, engine)?;
            if self.flow == Flow::Exit || host.cancelled() {
                return Ok((pos, Vec::new()));
            }
            // Registrations and wrapping relationships are fixed for this query;
            // evaluator mutations remain in the live session for subsequent queries.
            let (commands, path, variables) = self.completion_context();
            engine.set_context(commands, path, variables);
            engine.set_variable_values(self.completion_variables());
            engine.set_environment(self.completion_environment());
            engine.set_input_language(self.input_language);
            engine.set_registrations(self.completions.clone());
            engine.set_wrappers(self.completion_wrappers());
            let mut conditions = BTreeMap::new();
            engine.complete_evaluated(
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
                        if condition.is_empty() {
                            continue;
                        }
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
            )
        })();
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
    let mut errors = Vec::new();
    let tree = ast::parse_argument_list(&source, Default::default(), Some(&mut errors));
    if let Some(error) = errors.first() {
        return Err(completion_syntax_error(Error::new(
            error.describe(&source, false).to_string(),
        )));
    }
    let arguments: &ast::FreestandingArgumentList = tree.top();
    Ok(arguments
        .arguments
        .iter()
        .map(|argument| text(&source, argument))
        .collect())
}

fn completion_usage(message: impl Into<String>) -> Error {
    Error {
        message: format!("complete: {}", message.into()),
        status: 2,
    }
}

fn completion_syntax_error(error: Error) -> Error {
    Error { status: 1, ..error }
}

fn completion_definition(registration: &CompletionSpec) -> String {
    let mut words = vec![
        "complete".into(),
        if registration.is_path {
            "-p".into()
        } else {
            "-c".into()
        },
        quote(&registration.command),
    ];
    if let Some(target) = &registration.wrap_target {
        words.extend(["-w".into(), quote(target)]);
        return words.join(" ");
    }
    for (flag, value) in [
        ("-s", registration.short_option.map(|c| c.to_string())),
        ("-l", registration.long_option.clone()),
        ("-o", registration.old_option.clone()),
    ] {
        if let Some(value) = value {
            words.extend([flag.into(), quote(&value)]);
        }
    }
    for (enabled, flag) in [
        (registration.no_files, "-f"),
        (registration.force_files, "-F"),
        (registration.requires_argument, "-r"),
        (registration.keep_order, "-k"),
    ] {
        if enabled {
            words.push(flag.into());
        }
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
    for source in &registration.argument_sources {
        words.extend(["-a".into(), quote(source)]);
    }
    for condition in &registration.conditions {
        words.extend(["-n".into(), quote(condition)]);
    }
    words.join(" ")
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
