//! Common Fish string operations, including streamed input from native pipelines.
use fish_wcstringutil::split_about_bounded;
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::prelude::*;
use rash::host::{Host, ReadResult, Redirection};
use rash::{Error, Result};

#[path = "fish_string_matching.rs"]
mod matching;

#[derive(Default)]
pub(super) struct StringOutput {
    pub bytes: Vec<u8>,
    pub fields: Vec<(Vec<u8>, bool)>,
    field_size: usize,
    pub status: i32,
    pub captures: std::collections::HashMap<String, Vec<String>>,
    pub diagnostic: Option<String>,
}

impl StringOutput {
    fn append_field(&mut self, value: &wstr, want_newline: bool) -> Result<()> {
        let value = value.to_string().into_bytes();
        self.field_size = self
            .field_size
            .saturating_add(value.len())
            .saturating_add(1);
        if self.field_size > 16 * 1024 * 1024
            || self.fields.len() >= super::super::capture::ELEMENT_LIMIT
        {
            return Err(Error::new("string: output limit exceeded"));
        }
        self.fields.push((value, want_newline));
        Ok(())
    }
}

pub(super) fn run(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
) -> Result<StringOutput> {
    if args.first().is_some_and(|command| {
        matches!(
            command.as_str(),
            "match" | "replace" | "escape" | "unescape"
        )
    }) {
        return Ok(matching::run(
            args,
            host,
            redirects,
            stdin_is_directly_redirected,
        ));
    }
    let mut output = StringOutput::default();
    if let Err(error) = run_basic(
        args,
        host,
        redirects,
        stdin_is_directly_redirected,
        &mut output,
    ) {
        output.status = error.status;
        output.diagnostic = Some(error.message);
    }
    Ok(output)
}

#[derive(Default)]
struct Options {
    quiet: bool,
    no_empty: bool,
    left: bool,
    right: bool,
    no_newline: bool,
    count: Option<usize>,
    max: Option<usize>,
    start: Option<i64>,
    end: Option<i64>,
    length: Option<usize>,
    chars: Option<WString>,
    fields: Vec<usize>,
    allow_empty: bool,
}

const QUIET_OPTIONS: &[WOption] = &[wopt(L!("quiet"), NoArgument, 'q')];
const COLLECT_OPTIONS: &[WOption] = &[
    wopt(L!("allow-empty"), NoArgument, 'a'),
    wopt(L!("no-trim-newlines"), NoArgument, 'N'),
];
const JOIN_OPTIONS: &[WOption] = &[
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("no-empty"), NoArgument, 'n'),
];
const SPLIT_OPTIONS: &[WOption] = &[
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("right"), NoArgument, 'r'),
    wopt(L!("max"), RequiredArgument, 'm'),
    wopt(L!("no-empty"), NoArgument, 'n'),
    wopt(L!("fields"), RequiredArgument, 'f'),
    wopt(L!("allow-empty"), NoArgument, 'a'),
];
const REPEAT_OPTIONS: &[WOption] = &[
    wopt(L!("count"), RequiredArgument, 'n'),
    wopt(L!("max"), RequiredArgument, 'm'),
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("no-newline"), NoArgument, 'N'),
];
const TRIM_OPTIONS: &[WOption] = &[
    wopt(L!("chars"), RequiredArgument, 'c'),
    wopt(L!("left"), NoArgument, 'l'),
    wopt(L!("right"), NoArgument, 'r'),
    wopt(L!("quiet"), NoArgument, 'q'),
];
const SUB_OPTIONS: &[WOption] = &[
    wopt(L!("length"), RequiredArgument, 'l'),
    wopt(L!("start"), RequiredArgument, 's'),
    wopt(L!("end"), RequiredArgument, 'e'),
    wopt(L!("quiet"), NoArgument, 'q'),
];

fn option_spec(command: &str) -> Result<(&'static wstr, &'static [WOption<'static>])> {
    Ok(match command {
        "length" | "lower" | "upper" => (L!("q"), QUIET_OPTIONS),
        "collect" => (L!("Na"), COLLECT_OPTIONS),
        "join" | "join0" => (L!("qn"), JOIN_OPTIONS),
        "split" | "split0" => (L!("qrm:nf:a"), SPLIT_OPTIONS),
        "repeat" => (L!("n:m:qN"), REPEAT_OPTIONS),
        "trim" => (L!("c:lrq"), TRIM_OPTIONS),
        "sub" => (L!("l:qs:e:"), SUB_OPTIONS),
        _ => {
            return Err(Error::new(format!(
                "native string: unsupported subcommand {command}"
            )));
        }
    })
}

fn integer(command: &str, value: &wstr, label: &str) -> Result<i64> {
    value
        .to_string()
        .trim()
        .parse()
        .map_err(|_| Error::new(format!("string {command}: invalid {label} {value}")))
}

fn nonnegative(command: &str, value: &wstr, label: &str) -> Result<usize> {
    integer(command, value, label)?
        .try_into()
        .map_err(|_| Error::new(format!("string {command}: invalid {label} {value}")))
}

fn parse_options(command: &str, args: &mut [&wstr]) -> Result<(Options, usize)> {
    let (short, long) = option_spec(command)?;
    let original_args = args.to_vec();
    let mut parser = WGetopter::new(short, long, args);
    let mut options = Options::default();
    while let Some(option) = parser.next_opt() {
        match option {
            'q' => options.quiet = true,
            'n' if command == "repeat" => {
                options.count = Some(nonnegative(command, parser.woptarg.unwrap(), "count")?);
            }
            'n' => options.no_empty = true,
            'l' if command == "trim" => options.left = true,
            'r' => options.right = true,
            'N' => options.no_newline = true,
            'c' => options.chars = Some(parser.woptarg.unwrap().to_owned()),
            'm' => options.max = Some(nonnegative(command, parser.woptarg.unwrap(), "maximum")?),
            'l' => options.length = Some(nonnegative(command, parser.woptarg.unwrap(), "length")?),
            's' | 'e' => {
                let value = parser.woptarg.unwrap();
                let position = integer(command, value, "character index")?;
                if position == 0 {
                    return Err(Error::new("string sub: indexes start at one"));
                }
                if option == 's' {
                    options.start = Some(position);
                } else {
                    options.end = Some(position);
                }
            }
            'f' => {
                let value = parser.woptarg.unwrap().to_string();
                options.fields =
                    crate::string_fields::parse_fields(&value, 100_000).map_err(|problem| {
                        use crate::string_fields::FieldError;
                        let detail = match problem {
                            FieldError::Number => "invalid integer",
                            FieldError::Range => "invalid field range",
                            FieldError::Field => "invalid field number",
                            FieldError::Limit => "field selection limit exceeded",
                        };
                        Error::new(format!("string split: {value}: {detail}"))
                    })?;
            }
            'a' => options.allow_empty = true,
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
            _ => unreachable!("invalid basic string option"),
        }
    }
    Ok((options, parser.wopt_index))
}

fn run_basic(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
    output: &mut StringOutput,
) -> Result<()> {
    let command = args
        .first()
        .ok_or_else(|| Error::new("string: expected a subcommand"))?;
    option_spec(command)?;
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
    let (mut options, mut index) = parse_options(command, &mut args)?;
    if options.length.is_some() && options.end.is_some() {
        return Err(Error::new(
            "string sub: --end and --length are mutually exclusive",
        ));
    }
    if command != "collect" && options.allow_empty && options.fields.is_empty() {
        return Err(Error::new(
            "string split: --allow-empty is only valid with --fields",
        ));
    }
    let parameter_count = match command.as_str() {
        "join" | "split" => 1,
        "repeat" if options.count.is_none() && options.max.is_none() => 1,
        _ => 0,
    };
    if args.len() < index + parameter_count {
        return Err(Error::new(format!("string {command}: missing operand")));
    }
    let parameter = if matches!(command.as_str(), "split0" | "join0") {
        Some(L!("\0"))
    } else {
        args.get(index).copied()
    };
    if command == "repeat" && parameter_count == 1 {
        options.count = Some(nonnegative(command, parameter.unwrap(), "count")?);
    }
    index += parameter_count;
    let inputs = &args[index..];
    if !inputs.is_empty() && stdin_is_directly_redirected {
        return Err(Error::new(format!(
            "string {command}: too many arguments when reading from stdin"
        )));
    }
    if command == "repeat" && options.count.unwrap_or(0) == 0 && options.max.unwrap_or(0) == 0 {
        output.status = 1;
        return Ok(());
    }

    let mut success = false;
    let mut emitted = 0;
    let mut split_inputs = 0;
    let mut split_fields = 0;
    let mut missing_field = false;
    let mut final_newline = true;
    for_each_input_mode(
        inputs,
        host,
        redirects,
        stdin_is_directly_redirected,
        matches!(command.as_str(), "collect" | "split0"),
        |input, newline| {
            match command.as_str() {
                "collect" => {
                    let input = if options.no_newline {
                        input
                    } else {
                        let end =
                            input.len() - input.chars().rev().take_while(|&c| c == '\n').count();
                        &input[..end]
                    };
                    success |= !input.is_empty();
                    output.append_field(input, newline)?;
                }
                "join" | "join0" => {
                    // Match POSIX join's quiet probe, including its third-input
                    // early return and counting empty inputs in quiet mode.
                    if options.quiet && emitted > 1 {
                        return Ok(true);
                    }
                    if !options.quiet && options.no_empty && input.is_empty() {
                        return Ok(false);
                    }
                    if !options.quiet {
                        if emitted > 0 {
                            append(&mut output.bytes, parameter.unwrap(), false)?;
                        }
                        append(&mut output.bytes, input, false)?;
                    }
                    emitted += 1;
                    final_newline = newline;
                    success = emitted > 1;
                }
                "repeat" => {
                    final_newline = newline;
                    if input.is_empty() {
                        return Ok(false);
                    }
                    success = true;
                    if !options.quiet {
                        let value = repeat(input, &options)?;
                        if emitted > 0 {
                            append(&mut output.bytes, L!(""), true)?;
                        }
                        append(&mut output.bytes, &value, false)?;
                    }
                    emitted += 1;
                }
                _ => {
                    let split = matches!(command.as_str(), "split" | "split0");
                    let operation = if split { "split" } else { command.as_str() };
                    let (mut lines, changed) = transform(operation, parameter, input, &options)?;
                    if split {
                        split_inputs += 1;
                        split_fields += lines.len();
                    }
                    if command == "split0" && lines.last().is_some_and(|line| line.is_empty()) {
                        lines.pop();
                    }
                    success |= changed;
                    if !options.quiet {
                        let selected: Box<dyn Iterator<Item = &WString>> = if split
                            && !options.fields.is_empty()
                        {
                            if !options.allow_empty
                                && options.fields.iter().any(|&field| field >= lines.len())
                            {
                                missing_field = true;
                                return Ok(true);
                            }
                            Box::new(options.fields.iter().filter_map(|&field| lines.get(field)))
                        } else {
                            Box::new(lines.iter())
                        };
                        for line in selected {
                            if split {
                                output.append_field(line, true)?;
                            } else {
                                append(&mut output.bytes, line, newline || command == "length")?;
                            }
                        }
                    }
                }
            }
            Ok(options.quiet && success && !matches!(command.as_str(), "join" | "join0"))
        },
    )?;
    if !options.quiet && emitted > 0 {
        if command == "join0" {
            append(&mut output.bytes, L!("\0"), false)?;
        } else if final_newline && !options.no_newline {
            append(&mut output.bytes, L!(""), true)?;
        }
    }
    if command == "collect" && options.allow_empty && !success {
        output.append_field(L!(""), true)?;
    }
    if matches!(command.as_str(), "split" | "split0") && !options.quiet {
        success = !missing_field && split_fields > split_inputs;
    }
    output.status = i32::from(!success);
    Ok(())
}

fn repeat(input: &wstr, options: &Options) -> Result<WString> {
    let max = options.max.filter(|max| *max > 0);
    let repeated_length = input.len().checked_mul(options.count.unwrap_or(0));
    let length = if options.count.unwrap_or(0) == 0 {
        max.unwrap_or(0)
    } else {
        match (repeated_length, max) {
            (Some(length), Some(max)) => length.min(max),
            (Some(length), None) => length,
            (None, Some(max)) => max,
            (None, None) => return Err(Error::new("string repeat: output limit exceeded")),
        }
    };
    // Bound allocation even when a large count is capped by a small --max value.
    if length > 16 * 1024 * 1024 {
        return Err(Error::new("string repeat: output limit exceeded"));
    }
    Ok(input.chars().cycle().take(length).collect())
}

fn transform(
    command: &str,
    parameter: Option<&wstr>,
    input: &wstr,
    options: &Options,
) -> Result<(Vec<WString>, bool)> {
    let (output, success) = match command {
        "length" => (
            vec![WString::from(input.len().to_string())],
            !input.is_empty(),
        ),
        "lower" | "upper" => {
            let value = if command == "lower" {
                input.to_lowercase()
            } else {
                input.to_uppercase()
            };
            let changed = value != input;
            (vec![value], changed)
        }
        "trim" => {
            let chars = options.chars.as_deref().unwrap_or(L!(" \x0C\n\r\t\x0B"));
            let mut start = 0;
            let mut end = input.len();
            if options.left || !options.right {
                start = input.chars().take_while(|c| chars.contains(*c)).count();
            }
            if options.right || !options.left {
                end -= input[start..]
                    .chars()
                    .rev()
                    .take_while(|c| chars.contains(*c))
                    .count();
            }
            (
                vec![input[start..end].to_owned()],
                start > 0 || end < input.len(),
            )
        }
        "split" => {
            let separator = parameter.unwrap();
            // Bound the split list before allocating owned fields, including
            // when a stream consists entirely of delimiters.
            let limit = super::super::capture::ELEMENT_LIMIT;
            let max = options.max.unwrap_or(usize::MAX);
            let values: Vec<WString> = if options.right {
                let reversed: WString = input.chars().rev().collect();
                let separator: WString = separator.chars().rev().collect();
                let fields =
                    split_about_bounded(&reversed, &separator, max, options.no_empty, limit)
                        .ok_or_else(|| Error::new("string split: field limit exceeded"))?;
                fields
                    .into_iter()
                    .rev()
                    .map(|value| value.chars().rev().collect())
                    .collect()
            } else {
                let fields = split_about_bounded(input, separator, max, options.no_empty, limit)
                    .ok_or_else(|| Error::new("string split: field limit exceeded"))?;
                fields.into_iter().map(wstr::to_owned).collect()
            };
            let changed = values.len() > 1;
            (values, changed)
        }
        "sub" => {
            let range = crate::string_fields::substring_range(
                input.len(),
                options.start,
                options.end,
                options.length,
            );
            (vec![input[range].to_owned()], true)
        }
        _ => unreachable!("invalid basic string operation"),
    };
    Ok((output, success))
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

/// Read only explicitly redirected stdin, incrementally so quiet operations leave input unread.
fn for_each_input(
    args: &[&wstr],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
    consume: impl FnMut(&wstr, bool) -> Result<bool>,
) -> Result<()> {
    for_each_input_mode(
        args,
        host,
        redirects,
        stdin_is_directly_redirected,
        false,
        consume,
    )
}

fn for_each_input_mode(
    args: &[&wstr],
    host: &mut impl Host,
    redirects: &[Redirection],
    stdin_is_directly_redirected: bool,
    whole_stream: bool,
    mut consume: impl FnMut(&wstr, bool) -> Result<bool>,
) -> Result<()> {
    if !stdin_is_directly_redirected {
        for arg in args {
            if consume(arg, true)? {
                break;
            }
        }
        return Ok(());
    }
    if whole_stream {
        let ReadResult::Line(bytes) = host.read_record(redirects, None, None, false)? else {
            return Err(Error {
                message: "string: input interrupted".into(),
                status: 130,
            });
        };
        if !bytes.is_empty() {
            let input =
                String::from_utf8(bytes).map_err(|_| Error::new("string: input must be UTF-8"))?;
            consume(&WString::from(input), false)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::{Command, RunResult};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct StringHost {
        input: VecDeque<Vec<u8>>,
        reads: usize,
    }

    impl Host for StringHost {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            unreachable!()
        }
        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            unreachable!()
        }
        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            self.reads += 1;
            Ok(ReadResult::Line(self.input.pop_front().unwrap_or_default()))
        }
        fn write(&mut self, _: i32, _: &[u8], _: &[Redirection], _: bool) -> Result<Vec<u8>> {
            unreachable!()
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            vec![]
        }
    }

    fn run_args(args: &[&str], host: &mut StringHost, direct: bool) -> StringOutput {
        run(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            host,
            &[],
            direct,
        )
        .unwrap()
    }

    #[test]
    fn inherited_input_is_never_read_when_arguments_are_empty() {
        let commands: &[&[&str]] = &[
            &["collect"],
            &["split0"],
            &["join0"],
            &["length"],
            &["lower"],
            &["upper"],
            &["trim"],
            &["split", ":"],
            &["join", ","],
            &["repeat", "-n2"],
            &["sub"],
            &["match", "x"],
            &["replace", "x", "y"],
            &["escape"],
            &["unescape"],
        ];
        for args in commands {
            let mut host = StringHost {
                input: [b"untouched\n".to_vec()].into(),
                ..StringHost::default()
            };
            let output = run_args(args, &mut host, false);
            assert_eq!(output.status, 1, "{args:?}: {:?}", output.diagnostic);
            assert!(output.bytes.is_empty(), "{args:?}");
            assert_eq!(host.reads, 0, "{args:?}");
        }
    }

    #[test]
    fn direct_input_and_operands_conflict_before_any_read() {
        let commands: &[&[&str]] = &[
            &["collect", "x"],
            &["split0", "x"],
            &["join0", "x"],
            &["length", "x"],
            &["trim", "x"],
            &["split", ":", "x"],
            &["join", ",", "x"],
            &["repeat", "2", "x"],
            &["repeat", "-m2", "x"],
            &["sub", "-s1", "x"],
            &["match", "x", "x"],
            &["replace", "x", "y", "x"],
            &["escape", "x"],
            &["unescape", "x"],
        ];
        for args in commands {
            let mut host = StringHost::default();
            let output = run_args(args, &mut host, true);
            assert_eq!(output.status, 2, "{args:?}");
            assert!(output.diagnostic.unwrap().contains("too many arguments"));
            assert_eq!(host.reads, 0, "{args:?}");
        }
    }

    #[test]
    fn quiet_basic_operations_leave_remaining_records_unread() {
        let commands: &[(&[&str], &[u8])] = &[
            (&["length", "-q"], b"first\n"),
            (&["lower", "-q"], b"FIRST\n"),
            (&["upper", "-q"], b"first\n"),
            (&["trim", "-q"], b" first\n"),
            (&["split", "-q", ":"], b"a:b\n"),
            (&["repeat", "-qn2"], b"first\n"),
            (&["sub", "-qs1"], b"\n"),
            (&["match", "-q", "*"], b"first\n"),
            (&["replace", "-q", "first", "FIRST"], b"first\n"),
        ];
        for (args, first) in commands {
            let mut host = StringHost {
                input: [first.to_vec(), b"unread\n".to_vec()].into(),
                ..StringHost::default()
            };
            let output = run_args(args, &mut host, true);
            assert_eq!(output.status, 0, "{args:?}: {:?}", output.diagnostic);
            assert!(output.bytes.is_empty(), "{args:?}");
            assert_eq!(host.reads, 1, "{args:?}");
            assert_eq!(host.input.pop_front().unwrap(), b"unread\n");
        }
        let mut host = StringHost {
            input: [
                b"\n".to_vec(),
                b"first\n".to_vec(),
                b"second\n".to_vec(),
                b"unread\n".to_vec(),
            ]
            .into(),
            ..StringHost::default()
        };
        assert_eq!(run_args(&["join", "-qn", ","], &mut host, true).status, 0);
        assert_eq!(host.reads, 3);
        assert_eq!(host.input.pop_front().unwrap(), b"unread\n");
    }

    #[test]
    fn common_fish_string_operations_use_wide_components() {
        assert_eq!(
            transform("split", Some(L!(":")), L!("a:b::c"), &Options::default())
                .unwrap()
                .0,
            ["a", "b", "", "c"]
        );
        assert_eq!(
            transform(
                "sub",
                None,
                L!("a文bc"),
                &Options {
                    start: Some(2),
                    length: Some(2),
                    ..Options::default()
                },
            )
            .unwrap()
            .0,
            ["文b"]
        );
        assert_eq!(
            transform("trim", None, L!("  word\t"), &Options::default())
                .unwrap()
                .0,
            ["word"]
        );
    }
}
