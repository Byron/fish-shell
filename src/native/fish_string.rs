//! Common Fish string operations, including streamed input from native pipelines.
use rash::host::{Host, ReadResult, Redirection};
use rash::{Error, Result};

#[derive(Default)]
struct Options {
    quiet: bool,
    no_empty: bool,
    insensitive: bool,
    invert: bool,
    all: bool,
    left: bool,
    right: bool,
    no_newline: bool,
    count: Option<usize>,
    start: Option<isize>,
    length: Option<usize>,
    chars: Option<String>,
}

pub(super) fn wildcard_match(pattern: &str, value: &str, insensitive: bool) -> bool {
    let pattern = if insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_owned()
    };
    let value = if insensitive {
        value.to_lowercase()
    } else {
        value.to_owned()
    };
    let pattern: Vec<_> = pattern.chars().collect();
    let value: Vec<_> = value.chars().collect();
    let (mut p, mut v, mut star, mut backtrack) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            backtrack = v;
        } else if let Some(star) = star {
            backtrack += 1;
            v = backtrack;
            p = star + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

pub(super) fn run(
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
            "-i" | "--ignore-case" => options.insensitive = true,
            "-v" | "--invert" => options.invert = true,
            "-a" | "--all" => options.all = true,
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
            "-r" | "--regex" => {
                return Err(Error::new(
                    "native string: regular expression operations are not available yet",
                ));
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
        "join" | "split" | "match" => 1,
        "replace" => 2,
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
            "replace" => {
                if options.insensitive {
                    return Err(Error::new(
                        "native string replace: case-insensitive replacement is not available yet",
                    ));
                }
                let value = if options.all {
                    input.replace(parameters[0], parameters[1])
                } else {
                    input.replacen(parameters[0], parameters[1], 1)
                };
                success |= input.contains(parameters[0]);
                output.push(value);
            }
            "match" => {
                let matches =
                    wildcard_match(parameters[0], input, options.insensitive) != options.invert;
                if matches {
                    success = true;
                    output.push(input.clone());
                }
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
            "escape" => {
                let mut escaped = String::new();
                if input.is_empty() {
                    escaped.push_str("''");
                }
                for c in input.chars() {
                    if c.is_whitespace() || "\\'\"$();&|<>*?{}[]#~".contains(c) {
                        escaped.push('\\');
                    }
                    escaped.push(c);
                }
                success = true;
                output.push(escaped);
            }
            "unescape" => {
                output.push(super::super::expand::unescape_literal(input)?);
                success = true;
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
        assert!(wildcard_match("a*?", "alpha", false));
        assert!(!wildcard_match("a?", "alpha", false));
        assert_eq!(
            transform("trim", &[], &["  word\t".into()], &Options::default())
                .unwrap()
                .0,
            ["word"]
        );
    }
}
