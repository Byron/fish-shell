//! Common Fish string operations, including streamed input from native pipelines.
use fish_wcstringutil::split_about;
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::prelude::*;
use rash::host::{Host, ReadResult, Redirection};
use rash::{Error, Result};

#[path = "fish_string_matching.rs"]
mod matching;

#[derive(Default)]
pub(super) struct StringOutput {
    pub bytes: Vec<u8>,
    pub status: i32,
    pub captures: std::collections::HashMap<String, Vec<String>>,
    pub diagnostic: Option<String>,
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
    length: Option<usize>,
    chars: Option<WString>,
}

const QUIET_OPTIONS: &[WOption] = &[wopt(L!("quiet"), NoArgument, 'q')];
const JOIN_OPTIONS: &[WOption] = &[
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("no-empty"), NoArgument, 'n'),
];
const SPLIT_OPTIONS: &[WOption] = &[
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("right"), NoArgument, 'r'),
    wopt(L!("max"), RequiredArgument, 'm'),
    wopt(L!("no-empty"), NoArgument, 'n'),
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
    wopt(L!("quiet"), NoArgument, 'q'),
];

fn option_spec(command: &str) -> Result<(&'static wstr, &'static [WOption<'static>])> {
    Ok(match command {
        "length" | "lower" | "upper" => (L!("q"), QUIET_OPTIONS),
        "join" => (L!("qn"), JOIN_OPTIONS),
        "split" => (L!("qrm:n"), SPLIT_OPTIONS),
        "repeat" => (L!("n:m:qN"), REPEAT_OPTIONS),
        "trim" => (L!("c:lrq"), TRIM_OPTIONS),
        "sub" => (L!("l:qs:"), SUB_OPTIONS),
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
            's' => {
                let value = parser.woptarg.unwrap();
                let start = integer(command, value, "start index")?;
                if start == 0 {
                    return Err(Error::new("string sub: indexes start at one"));
                }
                options.start = Some(start);
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
    let parameter_count = match command.as_str() {
        "join" | "split" => 1,
        "repeat" if options.count.is_none() && options.max.is_none() => 1,
        _ => 0,
    };
    if args.len() < index + parameter_count {
        return Err(Error::new(format!("string {command}: missing operand")));
    }
    let parameter = args.get(index).copied();
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
    let mut final_newline = true;
    for_each_input(
        inputs,
        host,
        redirects,
        stdin_is_directly_redirected,
        |input, newline| {
            match command.as_str() {
                "join" => {
                    if options.no_empty && input.is_empty() {
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
                    let (lines, changed) = transform(command, parameter, input, &options)?;
                    if command == "split" {
                        split_inputs += 1;
                        split_fields += lines.len();
                    }
                    success |= changed;
                    if !options.quiet {
                        for line in lines {
                            append(
                                &mut output.bytes,
                                &line,
                                newline || matches!(command.as_str(), "length" | "split"),
                            )?;
                        }
                    }
                }
            }
            Ok(options.quiet && success)
        },
    )?;
    if !options.quiet && emitted > 0 && final_newline && !options.no_newline {
        append(&mut output.bytes, L!(""), true)?;
    }
    if command == "split" && !options.quiet {
        success = split_fields > split_inputs;
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
            let max = options.max.unwrap_or(usize::MAX);
            let values: Vec<WString> = if options.right {
                let reversed: WString = input.chars().rev().collect();
                let separator: WString = separator.chars().rev().collect();
                split_about(&reversed, &separator, max, options.no_empty)
                    .into_iter()
                    .rev()
                    .map(|value| value.chars().rev().collect())
                    .collect()
            } else {
                split_about(input, separator, max, options.no_empty)
                    .into_iter()
                    .map(wstr::to_owned)
                    .collect()
            };
            let changed = values.len() > 1;
            (values, changed)
        }
        "sub" => {
            let start = options.start.unwrap_or(1);
            let start = if start < 0 {
                input
                    .len()
                    .saturating_sub(start.unsigned_abs().min(usize::MAX as u64) as usize)
            } else {
                ((start - 1) as usize).min(input.len())
            };
            let end = start
                .saturating_add(options.length.unwrap_or(usize::MAX))
                .min(input.len());
            (vec![input[start..end].to_owned()], true)
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
