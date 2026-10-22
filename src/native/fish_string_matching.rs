//! Native I/O and variable adapters for shared Fish string operations.
use super::{StringOutput, *};
use crate::string_regex::{
    MatchOptions, RegexError, ReplaceOptions, StringMatcher, StringReplacer,
};
use fish_wcstringutil::escape::{
    EscapeFlags, EscapeStringStyle, UnescapeStringStyle, escape_string, unescape_string,
};
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::prelude::*;
use std::borrow::Cow;

const MATCH_OPTIONS: &[WOption] = &[
    wopt(L!("all"), NoArgument, 'a'),
    wopt(L!("entire"), NoArgument, 'e'),
    wopt(L!("groups-only"), NoArgument, 'g'),
    wopt(L!("ignore-case"), NoArgument, 'i'),
    wopt(L!("invert"), NoArgument, 'v'),
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("regex"), NoArgument, 'r'),
    wopt(L!("index"), NoArgument, 'n'),
    wopt(L!("max-matches"), RequiredArgument, 'm'),
];
const REPLACE_OPTIONS: &[WOption] = &[
    wopt(L!("all"), NoArgument, 'a'),
    wopt(L!("filter"), NoArgument, 'f'),
    wopt(L!("ignore-case"), NoArgument, 'i'),
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("regex"), NoArgument, 'r'),
    wopt(L!("max-matches"), RequiredArgument, 'm'),
];
const ESCAPE_OPTIONS: &[WOption] = &[
    wopt(L!("no-quoted"), NoArgument, 'n'),
    wopt(L!("style"), RequiredArgument, 's'),
];

#[derive(Default)]
struct Options {
    matching: MatchOptions,
    filter: bool,
    max_matches: Option<usize>,
    no_quoted: bool,
    style: Option<WString>,
}

pub(super) fn run(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
) -> StringOutput {
    let mut output = StringOutput::default();
    if let Err(error) = run_impl(
        args,
        host,
        redirects,
        stdin_is_directly_redirected,
        &mut output,
    ) {
        output.status = error.status;
        output.diagnostic = Some(error.message);
    }
    output
}

fn run_impl(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
    output: &mut StringOutput,
) -> Result<()> {
    let command = args[0].as_str();
    if args
        .get(1)
        .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        output.bytes = format!(
            "string {command}: see https://fishshell.com/docs/current/cmds/string-{command}.html\n"
        )
        .into_bytes();
        return Ok(());
    }
    let args: Vec<WString> = args.iter().map(|arg| WString::from(arg.as_str())).collect();
    let mut args: Vec<&wstr> = args.iter().map(WString::as_utfstr).collect();
    let (options, index) = parse_options(command, &mut args)?;
    let parameter_count = match command {
        "match" => 1,
        "replace" => 2,
        _ => 0,
    };
    if args.len() < index + parameter_count {
        return Err(Error::new(format!("string {command}: missing operand")));
    }
    let parameters = &args[index..index + parameter_count];
    let inputs = &args[index + parameter_count..];
    if !inputs.is_empty() && stdin_is_directly_redirected {
        return Err(Error::new(format!(
            "string {command}: too many arguments when reading from stdin"
        )));
    }
    match command {
        "match" => {
            let opts = options.matching;
            if opts.entire && opts.index {
                return Err(Error::new(
                    "string match: --entire and --index are mutually exclusive",
                ));
            }
            if opts.invert_match && opts.groups_only {
                return Err(Error::new(
                    "string match: --invert and --groups-only are mutually exclusive",
                ));
            }
            if opts.entire && opts.groups_only {
                return Err(Error::new(
                    "string match: --entire and --groups-only are mutually exclusive",
                ));
            }
            let mut matcher = StringMatcher::new(parameters[0], opts, |name| {
                matches!(
                    name.to_string().as_str(),
                    "status" | "pipestatus" | "last_pid" | "fish_pid" | "PWD"
                )
            })
            .map_err(|error| regex_error(command, error))?;
            for_each_input(inputs, host, redirects, |input, _| {
                let mut output_error = None;
                if let Err(error) = matcher.report_matches(input, |line| {
                    if output_error.is_none() {
                        output_error = append(&mut output.bytes, line, true).err();
                    }
                }) {
                    let diagnostic = format!(
                        "string match: regular expression error: {}",
                        error.error_message()
                    );
                    if let Some(previous) = &mut output.diagnostic {
                        previous.push('\n');
                        previous.push_str(&diagnostic);
                    } else {
                        output.diagnostic = Some(diagnostic);
                    }
                }
                if let Some(error) = output_error {
                    return Err(error);
                }
                let count = matcher.match_count();
                Ok((opts.quiet && count > 0) || options.max_matches == Some(count))
            })?;
            output.status = i32::from(matcher.match_count() == 0);
            output.captures = matcher
                .named_captures()
                .into_iter()
                .map(|(name, values)| {
                    (
                        name,
                        values.into_iter().map(|value| value.to_string()).collect(),
                    )
                })
                .collect();
        }
        "replace" => {
            let replacer = StringReplacer::new(
                parameters[0],
                parameters[1],
                ReplaceOptions {
                    all: options.matching.all,
                    ignore_case: options.matching.ignore_case,
                    regex: options.matching.regex,
                },
            )
            .map_err(|error| regex_error(command, error))?;
            let mut count = 0;
            for_each_input(inputs, host, redirects, |input, newline| {
                let (replaced, result) =
                    replacer.replace(Cow::Borrowed(input)).map_err(|error| {
                        Error::new(format!(
                            "string replace: regular expression substitute error: {}",
                            error.error_message()
                        ))
                    })?;
                count += usize::from(replaced);
                if !options.matching.quiet && (!options.filter || replaced) {
                    append(&mut output.bytes, &result, newline)?;
                }
                Ok((options.matching.quiet && count > 0) || options.max_matches == Some(count))
            })?;
            output.status = i32::from(count == 0);
        }
        "escape" => {
            let style = options
                .style
                .as_deref()
                .map(EscapeStringStyle::try_from)
                .transpose()
                .map_err(|_| Error::new("string escape: invalid escape style"))?
                .unwrap_or_default();
            let style = match style {
                EscapeStringStyle::Script(..) if options.no_quoted => {
                    EscapeStringStyle::Script(EscapeFlags {
                        no_quoted: true,
                        ..Default::default()
                    })
                }
                style => style,
            };
            let mut escaped_any = false;
            for_each_input(inputs, host, redirects, |input, newline| {
                append(&mut output.bytes, &escape_string(input, style), newline)?;
                escaped_any = true;
                Ok(false)
            })?;
            output.status = i32::from(!escaped_any);
        }
        "unescape" => {
            let style = options
                .style
                .as_deref()
                .map(UnescapeStringStyle::try_from)
                .transpose()
                .map_err(|_| Error::new("string unescape: invalid escape style"))?
                .unwrap_or_default();
            let mut unescaped_any = false;
            for_each_input(inputs, host, redirects, |input, newline| {
                if let Some(result) = unescape_string(input, style) {
                    append(&mut output.bytes, &result, newline)?;
                    unescaped_any = true;
                }
                Ok(false)
            })?;
            output.status = i32::from(!unescaped_any);
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn parse_options(command: &str, args: &mut [&wstr]) -> Result<(Options, usize)> {
    let (short, long) = match command {
        "match" => (L!("aegivqrnm:"), MATCH_OPTIONS),
        "replace" => (L!("afiqrm:"), REPLACE_OPTIONS),
        "escape" | "unescape" => (L!("n"), ESCAPE_OPTIONS),
        _ => unreachable!(),
    };
    let original_args = args.to_vec();
    let mut parser = WGetopter::new(short, long, args);
    let mut options = Options::default();
    while let Some(option) = parser.next_opt() {
        match option {
            'a' => options.matching.all = true,
            'e' => options.matching.entire = true,
            'g' => options.matching.groups_only = true,
            'i' => options.matching.ignore_case = true,
            'v' => options.matching.invert_match = true,
            'q' => options.matching.quiet = true,
            'r' => options.matching.regex = true,
            'f' => options.filter = true,
            'n' if command == "match" => options.matching.index = true,
            'n' => options.no_quoted = true,
            's' => options.style = Some(parser.woptarg.unwrap().to_owned()),
            'm' => {
                let value = parser.woptarg.unwrap().to_string();
                options.max_matches = Some(
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| {
                            Error::new(format!(
                                "string {command}: invalid maximum match count {value}"
                            ))
                        })?,
                );
            }
            error @ (':' | ';' | '?') => {
                let argument = original_args[parser.wopt_index - 1];
                let detail = match error {
                    ':' => "requires an argument",
                    ';' => "does not take an argument",
                    _ => "unknown option",
                };
                return Err(Error::new(format!(
                    "string {command}: {argument}: {detail}"
                )));
            }
            _ => unreachable!("invalid string option"),
        }
    }
    Ok((options, parser.wopt_index))
}

fn regex_error(command: &str, error: RegexError) -> Error {
    Error::new(match error {
        RegexError::Compile { pattern, error } => format!(
            "string {command}: regular expression compile error: {}\nstring {command}: {pattern}\nstring {command}: {}^",
            error.error_message(),
            " ".repeat(error.offset().unwrap_or(0).saturating_sub(1)),
        ),
        RegexError::InvalidCaptureGroupName { name } => format!(
            "string {command}: modification of read-only variable \"{name}\" is not allowed"
        ),
        RegexError::InvalidEscape { replacement } => {
            format!("string {command}: invalid escape sequence in pattern \"{replacement}\"")
        }
    })
}

fn append(output: &mut Vec<u8>, line: &wstr, newline: bool) -> Result<()> {
    let line = line.to_string();
    if output
        .len()
        .saturating_add(line.len())
        .saturating_add(usize::from(newline))
        > 16 * 1024 * 1024
    {
        return Err(Error::new("string: output limit exceeded"));
    }
    output.extend(line.as_bytes());
    if newline {
        output.push(b'\n');
    }
    Ok(())
}

/// Read incrementally so quiet and max-matches leave subsequent input unread.
fn for_each_input(
    args: &[&wstr],
    host: &mut impl Host,
    redirects: &[Redirection],
    mut consume: impl FnMut(&wstr, bool) -> Result<bool>,
) -> Result<()> {
    if !args.is_empty() {
        for arg in args {
            if consume(arg, true)? {
                break;
            }
        }
        return Ok(());
    }
    loop {
        let ReadResult::Line(mut bytes) = host.read_line(redirects, false)? else {
            return Err(Error {
                message: "string: input interrupted".into(),
                status: 130,
            });
        };
        if bytes.is_empty() {
            break;
        }
        let newline = bytes.last() == Some(&b'\n');
        if newline {
            bytes.pop();
        }
        let input =
            String::from_utf8(bytes).map_err(|_| Error::new("string: input must be UTF-8"))?;
        if consume(&WString::from(input), newline)? {
            break;
        }
    }
    Ok(())
}
