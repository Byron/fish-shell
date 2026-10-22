//! Native input/output adapter for shared terminal display transforms.
use super::{StringOutput, append, for_each_input, nonnegative};
use crate::string_display::{self, WidthSettings};
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
use fish_widestring::{ELLIPSIS_CHAR, decoded_width, prelude::*};
use rash::host::{Host, Redirection};
use rash::{Error, Result};

const INPUT_CHARS: usize = 4 * 1024 * 1024;
const INPUT_RECORDS: usize = 100_000;
const ELLIPSIS: &wstr = wstr::from_char_slice(&[ELLIPSIS_CHAR]);
const PAD_OPTIONS: &[WOption] = &[
    wopt(L!("char"), RequiredArgument, 'c'),
    wopt(L!("chars"), RequiredArgument, 'c'),
    wopt(L!("right"), NoArgument, 'r'),
    wopt(L!("center"), NoArgument, 'C'),
    wopt(L!("width"), RequiredArgument, 'w'),
];
const SHORTEN_OPTIONS: &[WOption] = &[
    wopt(L!("char"), RequiredArgument, 'c'),
    wopt(L!("chars"), RequiredArgument, 'c'),
    wopt(L!("max"), RequiredArgument, 'm'),
    wopt(L!("no-newline"), NoArgument, 'N'),
    wopt(L!("left"), NoArgument, 'l'),
    wopt(L!("quiet"), NoArgument, 'q'),
];
const LENGTH_OPTIONS: &[WOption] = &[
    wopt(L!("quiet"), NoArgument, 'q'),
    wopt(L!("visible"), NoArgument, 'V'),
];

#[derive(Default)]
struct Options<'a> {
    chars: Option<&'a wstr>,
    width: Option<usize>,
    left: bool,
    right: bool,
    center: bool,
    quiet: bool,
    no_newline: bool,
    visible: bool,
}

fn parse_options<'a>(command: &str, args: &mut [&'a wstr]) -> Result<(Options<'a>, usize)> {
    let (short, long) = match command {
        "pad" => (L!("c:rCw:"), PAD_OPTIONS),
        "shorten" => (L!("c:m:Nlq"), SHORTEN_OPTIONS),
        "length" => (L!("qV"), LENGTH_OPTIONS),
        _ => unreachable!(),
    };
    let original = args.to_vec();
    let mut parser = WGetopter::new(short, long, args);
    let mut options = Options::default();
    while let Some(option) = parser.next_opt() {
        match option {
            'c' => options.chars = parser.woptarg,
            'm' | 'w' => {
                options.width = Some(nonnegative(command, parser.woptarg.unwrap(), "width")?)
            }
            'l' => options.left = true,
            'r' => options.right = true,
            'C' => options.center = true,
            'q' => options.quiet = true,
            'N' => options.no_newline = true,
            'V' => options.visible = true,
            error @ (':' | ';' | '?') => {
                let detail = match error {
                    ':' => "requires an argument",
                    ';' => "does not take an argument",
                    _ => "unknown option",
                };
                return Err(Error::new(format!(
                    "string {command}: {}: {detail}",
                    original[parser.wopt_index - 1]
                )));
            }
            _ => unreachable!(),
        }
    }
    Ok((options, parser.wopt_index))
}

#[derive(Default)]
struct Inputs {
    lines: Vec<WString>,
    chars: usize,
}

impl Inputs {
    fn push(&mut self, input: &wstr, suffix: &wstr) -> Result<()> {
        self.chars = self
            .chars
            .saturating_add(input.len())
            .saturating_add(suffix.len());
        if self.chars > INPUT_CHARS || self.lines.len() >= INPUT_RECORDS {
            return Err(Error::new("string: display input limit exceeded"));
        }
        let mut value = input.to_owned();
        value.push_utfstr(suffix);
        self.lines.push(value);
        Ok(())
    }
}

pub(super) fn run(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    direct: bool,
    widths: WidthSettings,
) -> StringOutput {
    let mut output = StringOutput::default();
    if let Err(error) = run_impl(args, host, redirects, direct, widths, &mut output) {
        output.status = error.status;
        output.diagnostic = Some(error.message);
    }
    output
}

fn run_impl(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
    direct: bool,
    widths: WidthSettings,
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
    if args.len() > INPUT_RECORDS || args.iter().map(String::len).sum::<usize>() > INPUT_CHARS * 4 {
        return Err(Error::new("string: display input limit exceeded"));
    }
    let args: Vec<WString> = args.iter().map(|arg| WString::from(arg.as_str())).collect();
    let mut args: Vec<&wstr> = args.iter().map(WString::as_utfstr).collect();
    let (options, index) = parse_options(command, &mut args)?;
    let args = &args[index..];
    if direct && !args.is_empty() {
        return Err(Error::new(format!(
            "string {command}: too many arguments when reading from stdin"
        )));
    }
    if command == "length" {
        output.status = 1;
        return for_each_input(args, host, redirects, direct, |input, _| {
            if input.len() > INPUT_CHARS {
                return Err(Error::new("string: display input limit exceeded"));
            }
            let lengths: Box<dyn Iterator<Item = usize> + '_> = if options.visible {
                Box::new(widths.line_widths(input))
            } else {
                Box::new(std::iter::once(input.len()))
            };
            for length in lengths {
                if length > 0 {
                    output.status = 0;
                }
                if options.quiet {
                    if length > 0 {
                        return Ok(true);
                    }
                } else {
                    append(&mut output.bytes, &WString::from(length.to_string()), true)?;
                }
            }
            Ok(false)
        });
    }
    if command == "shorten" && options.width == Some(0) {
        return for_each_input(args, host, redirects, direct, |input, _| {
            append(&mut output.bytes, input, true)?;
            Ok(false)
        });
    }
    let pad_char = if command == "pad" {
        let arg = options.chars.unwrap_or(L!(" "));
        let [c] = arg.as_char_slice() else {
            return Err(Error::new(format!(
                "string pad: padding should be a character '{arg}'"
            )));
        };
        let width = widths
            .character_width(*c)
            .filter(|width| *width > 0)
            .ok_or_else(|| Error::new("string pad: invalid padding character of width zero"))?;
        Some((*c, width))
    } else {
        None
    };
    let ellipsis = options.chars.unwrap_or(ELLIPSIS);
    let mut inputs = Inputs::default();
    let mut newline = true;
    for_each_input(args, host, redirects, direct, |input, want_newline| {
        newline = want_newline;
        if command == "pad" {
            inputs.push(input, L!(""))?;
        } else if options.no_newline {
            let line = if options.left {
                input.split('\n').last()
            } else {
                input.split('\n').next()
            }
            .unwrap();
            // Preserve Fish's -N behavior: the selected segment gets an ellipsis
            // before measurement, even when the argument had no newline.
            inputs.push(line, ellipsis)?;
        } else {
            for line in input.split('\n') {
                inputs.push(line, L!(""))?;
            }
        }
        Ok(false)
    })?;
    if let Some((character, character_width)) = pad_char {
        let measured: Vec<usize> = inputs
            .lines
            .iter()
            .map(|line| widths.visible_width(line))
            .collect();
        let target = options
            .width
            .unwrap_or(0)
            .max(measured.iter().copied().max().unwrap_or(0));
        for (input, width) in inputs.lines.iter().zip(measured) {
            if input.len().saturating_add(target - width) > INPUT_CHARS {
                return Err(Error::new("string pad: output limit exceeded"));
            }
            let padded = string_display::pad(
                input,
                width,
                target,
                character,
                character_width,
                options.right,
                options.center,
            );
            append(&mut output.bytes, &padded, newline)?;
        }
        return Ok(());
    }
    let max = options.width.unwrap_or_else(|| {
        inputs
            .lines
            .iter()
            .map(|line| widths.visible_width(line))
            .filter(|width| *width > 0)
            .min()
            .unwrap_or(usize::MAX)
    });
    let ellipsis_width = if options.chars.is_some() {
        widths.visible_width(ellipsis)
    } else {
        decoded_width(ELLIPSIS)
    };
    let (ellipsis, ellipsis_width) = if ellipsis_width > max {
        (L!(""), 0)
    } else {
        (ellipsis, ellipsis_width)
    };
    output.status = 1;
    for line in inputs.lines {
        let range = widths.shorten_range(&line, max, ellipsis_width, options.left);
        let changed = range != (0..line.len());
        if changed {
            output.status = 0;
        }
        if options.quiet {
            if changed {
                break;
            }
            continue;
        }
        if changed && options.left {
            append(&mut output.bytes, ellipsis, false)?;
        }
        append(&mut output.bytes, &line[range], false)?;
        if changed && !options.left {
            append(&mut output.bytes, ellipsis, false)?;
        }
        append(&mut output.bytes, L!(""), true)?;
    }
    Ok(())
}
