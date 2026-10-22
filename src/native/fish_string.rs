//! Common Fish string operations, including streamed input from native pipelines.
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
    let (bytes, status) = run_basic(args, host, redirects)?;
    Ok(StringOutput {
        bytes,
        status,
        ..Default::default()
    })
}

#[derive(Default)]
struct Options {
    quiet: bool,
    no_empty: bool,
    left: bool,
    right: bool,
    no_newline: bool,
    count: Option<usize>,
    start: Option<isize>,
    length: Option<usize>,
    chars: Option<String>,
}

fn run_basic(
    args: &[String],
    host: &mut impl Host,
    redirects: &[Redirection],
) -> Result<(Vec<u8>, i32)> {
    let command = args
        .first()
        .ok_or_else(|| Error::new("string: expected a subcommand"))?;
    let mut options = Options::default();
    let mut index = 1;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--" => {
                index += 1;
                break;
            }
            "-q" | "--quiet" => options.quiet = true,
            "-n" | "--no-empty" if command != "repeat" => options.no_empty = true,
            "-l" | "--left" if command == "trim" => options.left = true,
            "-r" | "--right" if command == "trim" || command == "split" => options.right = true,
            "-N" | "--no-newline" => options.no_newline = true,
            "-c" | "--chars" | "-m" | "--max" | "-s" | "--start" | "-l" | "--length" | "-n"
            | "--count" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| Error::new("string: missing option argument"))?;
                match arg.as_str() {
                    "-c" | "--chars" => options.chars = Some(value.clone()),
                    "-s" | "--start" => {
                        options.start = Some(
                            value
                                .parse()
                                .map_err(|_| Error::new("string: invalid start index"))?,
                        );
                    }
                    "-l" | "--length" => {
                        options.length = Some(
                            value
                                .parse()
                                .map_err(|_| Error::new("string: invalid length"))?,
                        );
                    }
                    "-m" | "--max" if command == "repeat" => {
                        options.length = Some(
                            value
                                .parse()
                                .map_err(|_| Error::new("string: invalid maximum length"))?,
                        );
                    }
                    _ => {
                        options.count = Some(
                            value
                                .parse()
                                .map_err(|_| Error::new("string: invalid count"))?,
                        );
                    }
                }
            }
            _ if arg.starts_with('-') => {
                return Err(Error::new(format!(
                    "string {command}: unknown option {arg}"
                )));
            }
            _ => break,
        }
        index += 1;
    }
    let operands = match command.as_str() {
        "join" | "split" => 1,
        "repeat" if options.count.is_none() => 1,
        _ => 0,
    };
    if args.len() < index + operands {
        return Err(Error::new(format!("string {command}: missing operand")));
    }
    let parameters = &args[index..index + operands];
    if command == "repeat" && options.count.is_none() {
        options.count = Some(
            parameters[0]
                .parse()
                .map_err(|_| Error::new("string repeat: invalid count"))?,
        );
    }
    index += operands;
    let mut inputs = args[index..].to_vec();
    if inputs.is_empty() {
        loop {
            let ReadResult::Line(bytes) = host.read_line(redirects, false)? else {
                return Err(Error::new("string: input interrupted"));
            };
            if bytes.is_empty() {
                break;
            }
            let value =
                String::from_utf8(bytes).map_err(|_| Error::new("string: input must be UTF-8"))?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            inputs.push(value.strip_suffix('\r').unwrap_or(value).to_owned());
        }
    }
    let (lines, success) = transform(
        command,
        &parameters.iter().map(String::as_str).collect::<Vec<_>>(),
        &inputs,
        &options,
    )?;
    let mut bytes = Vec::new();
    if !options.quiet {
        for line in lines {
            bytes.extend(line.as_bytes());
            if !options.no_newline {
                bytes.push(b'\n');
            }
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(Error::new("string: output limit exceeded"));
            }
        }
    }
    Ok((bytes, i32::from(!success)))
}

fn transform(
    command: &str,
    parameters: &[&str],
    inputs: &[String],
    options: &Options,
) -> Result<(Vec<String>, bool)> {
    let mut output = Vec::new();
    let mut success = false;
    if command == "join" {
        let inputs: Vec<_> = inputs
            .iter()
            .filter(|value| !options.no_empty || !value.is_empty())
            .cloned()
            .collect();
        if !inputs.is_empty() {
            output.push(inputs.join(parameters[0]));
        }
        return Ok((output, inputs.len() > 1));
    }
    for input in inputs {
        match command {
            "length" => {
                output.push(input.chars().count().to_string());
                success |= !input.is_empty();
            }
            "lower" | "upper" => {
                let value = if command == "lower" {
                    input.to_lowercase()
                } else {
                    input.to_uppercase()
                };
                success |= value != *input;
                output.push(value);
            }
            "trim" => {
                let predicate = |c: char| {
                    options
                        .chars
                        .as_ref()
                        .map_or_else(|| c.is_whitespace(), |chars| chars.contains(c))
                };
                let mut value = input.as_str();
                if options.left || !options.right {
                    value = value.trim_start_matches(predicate);
                }
                if options.right || !options.left {
                    value = value.trim_end_matches(predicate);
                }
                success |= value != input;
                output.push(value.to_owned());
            }
            "split" => {
                let mut values: Vec<String> = if parameters[0].is_empty() {
                    input.chars().map(|c| c.to_string()).collect()
                } else if options.right {
                    let mut values: Vec<_> = input
                        .rsplitn(
                            options.count.unwrap_or(usize::MAX).saturating_add(1),
                            parameters[0],
                        )
                        .map(str::to_owned)
                        .collect();
                    values.reverse();
                    values
                } else {
                    input
                        .splitn(
                            options.count.unwrap_or(usize::MAX).saturating_add(1),
                            parameters[0],
                        )
                        .map(str::to_owned)
                        .collect()
                };
                success |= values.len() > 1;
                if options.no_empty {
                    values.retain(|value| !value.is_empty());
                }
                output.extend(values);
            }
            "repeat" => {
                let count = options.count.unwrap_or(1);
                if input.len().saturating_mul(count) > 16 * 1024 * 1024 {
                    return Err(Error::new("string repeat: output limit exceeded"));
                }
                let value: String = input
                    .repeat(count)
                    .chars()
                    .take(options.length.unwrap_or(usize::MAX))
                    .collect();
                success |= !value.is_empty();
                output.push(value);
            }
            "sub" => {
                let chars: Vec<_> = input.chars().collect();
                let start = options.start.unwrap_or(1);
                if start == 0 {
                    return Err(Error::new("string sub: indexes start at one"));
                }
                let start = if start < 0 {
                    (chars.len() as isize + start).max(0) as usize
                } else {
                    (start - 1) as usize
                };
                let start = start.min(chars.len());
                let end = start
                    .saturating_add(options.length.unwrap_or(usize::MAX))
                    .min(chars.len());
                let value: String = chars[start..end].iter().collect();
                success = true;
                output.push(value);
            }
            _ => {
                return Err(Error::new(format!(
                    "native string: unsupported subcommand {command}"
                )));
            }
        }
    }
    Ok((output, success))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn common_fish_string_operations() {
        assert_eq!(
            transform("split", &[":"], &["a:b::c".into()], &Options::default())
                .unwrap()
                .0,
            ["a", "b", "", "c"]
        );
        assert_eq!(
            transform(
                "sub",
                &[],
                &["a文bc".into()],
                &Options {
                    start: Some(2),
                    length: Some(2),
                    ..Default::default()
                }
            )
            .unwrap()
            .0,
            ["文b"]
        );
        assert_eq!(
            transform("trim", &[], &["  word\t".into()], &Options::default())
                .unwrap()
                .0,
            ["word"]
        );
    }
}
