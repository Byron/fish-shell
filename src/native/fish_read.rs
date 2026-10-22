//! Native record input with shared Fish token and delimiter splitting.
use super::*;
use crate::read_fields::{self, SplitMode};
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::L;
use rash::host::ReadResult;

#[derive(Default)]
struct Options {
    scope: Option<char>,
    exported: Option<bool>,
    array: bool,
    delimiter: Option<WString>,
    tokens: Option<bool>,
    null: bool,
    lines: bool,
    nchars: Option<usize>,
    prompt: Option<String>,
}

impl Session {
    pub(super) fn read(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if let Err(error) = self.read_impl(args, host, output) {
            self.status = error.status;
            output.bytes.extend(host.write(
                2,
                format!("{}\n", error.message).as_bytes(),
                &output.redirects,
                output.capture,
            )?);
        }
        Ok(())
    }

    fn read_impl(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        const LONG: &[WOption] = &[
            wopt(L!("array"), NoArgument, 'a'),
            wopt(L!("list"), NoArgument, 'a'),
            wopt(L!("delimiter"), RequiredArgument, 'd'),
            wopt(L!("global"), NoArgument, 'g'),
            wopt(L!("local"), NoArgument, 'l'),
            wopt(L!("universal"), NoArgument, 'U'),
            wopt(L!("export"), NoArgument, 'x'),
            wopt(L!("unexport"), NoArgument, 'u'),
            wopt(L!("line"), NoArgument, 'L'),
            wopt(L!("null"), NoArgument, 'z'),
            wopt(L!("nchars"), RequiredArgument, 'n'),
            wopt(L!("tokenize"), NoArgument, 't'),
            wopt(L!("tokenize-raw"), NoArgument, '\x01'),
            wopt(L!("prompt-str"), RequiredArgument, 'P'),
            wopt(L!("help"), NoArgument, 'h'),
        ];
        let words: Vec<WString> = std::iter::once("read")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut argv: Vec<&wstr> = words.iter().map(WString::as_utfstr).collect();
        let original = argv.clone();
        let mut parser = WGetopter::new(L!("ad:glUxuzLn:tP:h"), LONG, &mut argv);
        let mut options = Options::default();
        while let Some(option) = parser.next_opt() {
            match option {
                'a' => options.array = true,
                'd' => options.delimiter = Some(parser.woptarg.unwrap().to_owned()),
                'g' | 'l' | 'U' => {
                    if options.scope.is_some_and(|scope| scope != option) {
                        return Err(Error::new("read: multiple variable scopes"));
                    }
                    options.scope = Some(option);
                }
                'x' | 'u' => {
                    let export = option == 'x';
                    if options.exported.is_some_and(|value| value != export) {
                        return Err(Error::new(
                            "read: --export and --unexport are mutually exclusive",
                        ));
                    }
                    options.exported = Some(export);
                }
                'z' => options.null = true,
                'L' => options.lines = true,
                'n' => {
                    let value = parser.woptarg.unwrap();
                    let count = value.to_string().trim().parse::<usize>().map_err(|_| {
                        Error::new(format!("read: invalid character count {value}"))
                    })?;
                    options.nchars = (count > 0).then_some(count);
                }
                't' | '\x01' => {
                    let unescape = option == 't';
                    if options.tokens.is_some_and(|value| value != unescape) {
                        return Err(Error::new(
                            "read: --tokenize and --tokenize-raw are mutually exclusive",
                        ));
                    }
                    options.tokens = Some(unescape);
                }
                'P' => options.prompt = Some(parser.woptarg.unwrap().to_string()),
                'h' => {
                    self.write(
                        b"read: see https://fishshell.com/docs/current/cmds/read.html\n",
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
                        "read: {}: {detail}",
                        original[parser.wopt_index - 1]
                    )));
                }
                _ => unreachable!("read option"),
            }
        }
        let names: Vec<String> = parser.argv[parser.wopt_index..]
            .iter()
            .map(ToString::to_string)
            .collect();
        if options.array && names.len() != 1 {
            return Err(Error::new("read --array: expected one variable"));
        }
        if options.lines
            && (options.null || options.delimiter.is_some() || options.tokens.is_some())
        {
            return Err(Error::new(
                "read: --line cannot be combined with --null, --delimiter or --tokenize",
            ));
        }
        if options.tokens.is_some() && options.delimiter.is_some() {
            return Err(Error::new(
                "read: --tokenize and --delimiter are mutually exclusive",
            ));
        }
        for name in &names {
            valid_name(name)?;
            if read_only_variable(name) {
                return Err(Error::new(format!("read: {name} is read-only")));
            }
        }
        if host.fd_is_terminal(0, &output.redirects, output.capture)? {
            if let Some(prompt) = &options.prompt {
                self.write(prompt.as_bytes(), host, output)?;
            }
        }
        let terminator = if options.null { b'\0' } else { b'\n' };
        let ifs = self.variable("IFS").map_or_else(WString::new, |variable| {
            WString::from(variable.values.join(" "))
        });
        let mut assigned = 0;
        let mut status = 0;
        let mut failure = None;
        loop {
            let bytes = match host.read_record(
                &output.redirects,
                Some(terminator),
                options.nchars,
                false,
            ) {
                Ok(ReadResult::Line(bytes)) => bytes,
                Ok(ReadResult::Interrupted(_)) => {
                    status = 130;
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            if bytes.is_empty() {
                status = 1;
                break;
            }
            let line = match String::from_utf8(bytes) {
                Ok(line) => line,
                Err(_) => {
                    failure = Some(Error {
                        message: "read: input is not UTF-8".into(),
                        status: 1,
                    });
                    break;
                }
            };
            let terminated = line.ends_with(terminator as char);
            let line = line.strip_suffix(terminator as char).unwrap_or(&line);
            // Normalize a newline record's Windows CRLF, while preserving CR
            // and embedded newlines when reading NUL-delimited data.
            let line = if !options.null && terminated {
                line.strip_suffix('\r').unwrap_or(line)
            } else {
                line
            };
            if names.is_empty() {
                self.write(line.as_bytes(), host, output)?;
                self.status = 0;
                return Ok(());
            }
            let wide = WString::from(line);
            let mode = if options.lines {
                SplitMode::Delimiter(L!("\n"))
            } else if let Some(unescape) = options.tokens {
                SplitMode::Tokens { unescape }
            } else if let Some(delimiter) = options.delimiter.as_deref() {
                SplitMode::Delimiter(delimiter)
            } else {
                SplitMode::Ifs(&ifs)
            };
            for values in read_fields::split(&wide, mode, options.array, names.len() - assigned) {
                self.assign_read(&names[assigned], values, &options, host, output)?;
                assigned += 1;
            }
            if !options.lines || assigned == names.len() {
                break;
            }
        }
        for name in &names[assigned..] {
            self.assign_read(name, Vec::new(), &options, host, output)?;
        }
        self.status = status;
        failure.map_or(Ok(()), Err)
    }

    fn assign_read(
        &mut self,
        name: &str,
        values: Vec<WString>,
        options: &Options,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut args = Vec::new();
        if let Some(scope) = options.scope {
            args.push(format!("-{scope}"));
        }
        if let Some(exported) = options.exported {
            args.push(if exported { "-x" } else { "-u" }.into());
        }
        args.push(name.to_owned());
        args.extend(values.iter().map(ToString::to_string));
        self.set(&args, host, output)?;
        Ok(())
    }
}
