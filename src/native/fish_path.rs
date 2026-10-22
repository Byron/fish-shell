//! Portable lexical path operations with Windows filesystem resolution.
use super::*;
use crate::path_lexical::{windows_basename, windows_dirname, windows_normalize, windows_validate};
use fish_wgetopt::{ArgType::NoArgument, WGetopter, WOption, wopt};
use fish_widestring::L;
use rash::host::ReadResult;
use std::path::{Component, Path, PathBuf, Prefix};

#[derive(Default)]
struct Options {
    quiet: bool,
    null_in: bool,
    null_out: bool,
    no_extension: bool,
    no_symlinks: bool,
    help: bool,
}

fn error(message: impl Into<String>, status: i32) -> Error {
    Error {
        message: message.into(),
        status,
    }
}

fn validate(path: &str) -> Result<()> {
    windows_validate(&WString::from(path)).map_err(|reason| error(reason.to_string(), 2))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    validate(&path.to_string_lossy())?;
    Ok(path)
}

/// Canonicalization returns a verbatim prefix even for ordinary Windows files.
/// Only remove known drive/UNC prefixes and reject names whose meaning would
/// change without that prefix. These results can then feed other path helpers.
fn ordinary(path: PathBuf) -> Result<String> {
    let value = path
        .into_os_string()
        .into_string()
        .map_err(|_| error("resolved path is not Unicode", 2))?;
    let value = if let Some(tail) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{tail}")
    } else if let Some(tail) = value.strip_prefix("\\\\?\\") {
        let bytes = tail.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return Err(error(
                "resolved path requires an unsupported Windows namespace",
                2,
            ));
        }
        tail.to_owned()
    } else {
        value
    };
    validate(&value)?;
    for component in Path::new(&value).components() {
        if let Component::Normal(name) = component {
            let name = name.to_string_lossy();
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let device = matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|number| {
                    let mut chars = number.chars();
                    matches!(chars.next(), Some('1'..='9' | '¹' | '²' | '³'))
                        && chars.next().is_none()
                })
            });
            if name.ends_with(['.', ' ']) || device {
                return Err(error(
                    "resolved path requires an unsupported Windows namespace",
                    2,
                ));
            }
        }
    }
    Ok(value)
}

fn canonical(path: &Path) -> Result<String> {
    ordinary(std::fs::canonicalize(path)?)
}

fn realpath(path: &str, no_symlinks: bool) -> Result<String> {
    validate(path)?;
    if path.is_empty() {
        return Err(error("empty path", 1));
    }
    let input = Path::new(path);
    let path = if no_symlinks {
        // Fish resolves the working directory itself, while preserving every
        // symlink in the operand. A drive-relative name has its own native cwd.
        let cwd = canonical(Path::new("."))?;
        if input.has_root() {
            absolute(input)?
        } else if matches!(input.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
        {
            let base = canonical(Path::new(&format!("{}.", &path[..2])))?;
            absolute(&Path::new(&base).join(&path[2..]))?
        } else {
            absolute(&Path::new(&cwd).join(input))?
        }
    } else {
        absolute(input)?
    };
    if no_symlinks {
        return windows_normalize(&WString::from(path.to_string_lossy().as_ref()))
            .map(|path| path.to_string())
            .map_err(|reason| error(reason.to_string(), 1));
    }
    match canonical(&path) {
        Ok(value) => Ok(value),
        Err(problem) if problem.status == 2 => Err(problem),
        Err(original) => {
            let Some(parent) = path.parent() else {
                return Err(original);
            };
            let Some(name) = path.file_name() else {
                return Err(original);
            };
            if !parent.is_dir() {
                return Err(original);
            }
            let parent = canonical(parent)?;
            ordinary(Path::new(&parent).join(name))
        }
    }
}

fn resolve(path: &str) -> Result<String> {
    validate(path)?;
    let mut ancestor = absolute(Path::new(path))?;
    let mut tail = Vec::new();
    loop {
        match canonical(&ancestor) {
            Ok(parent) => {
                let mut result = PathBuf::from(parent);
                for component in tail.iter().rev() {
                    result.push(component);
                }
                return ordinary(result);
            }
            Err(problem) if problem.status == 2 => return Err(problem),
            Err(original) => {
                let Some(name) = ancestor.file_name().map(ToOwned::to_owned) else {
                    return Err(original);
                };
                tail.push(name);
                if !ancestor.pop() {
                    return Err(original);
                }
            }
        }
    }
}

impl Session {
    pub(super) fn path_builtin(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if let Err(problem) = self.path_builtin_impl(name, args, host, output) {
            self.status = problem.status;
            self.path_diagnostic(name, &problem.message, host, output)?;
        }
        Ok(())
    }

    fn path_diagnostic(
        &self,
        name: &str,
        message: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        output.bytes.extend(host.write(
            2,
            format!("{name}: {message}\n").as_bytes(),
            &output.redirects,
            output.capture,
        )?);
        Ok(())
    }

    fn path_builtin_impl(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let (operation, args) = if name == "realpath" {
            (name, args)
        } else {
            match args.split_first() {
                Some((command, _)) if matches!(command.as_str(), "-h" | "--help") => {
                    self.write(
                        b"path dirname|basename|normalize|resolve [-qzZ] [PATH ...]\n",
                        host,
                        output,
                    )?;
                    self.status = 0;
                    return Ok(());
                }
                Some((command, args))
                    if matches!(
                        command.as_str(),
                        "dirname" | "basename" | "normalize" | "resolve"
                    ) =>
                {
                    (command.as_str(), args)
                }
                Some((command, _)) => {
                    return Err(error(format!("unsupported subcommand {command}"), 2));
                }
                None => return Err(error("expected dirname, basename, normalize or resolve", 2)),
            }
        };
        let mut opts = Options::default();
        let wide: Vec<WString> = std::iter::once(operation)
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut args: Vec<&wstr> = wide.iter().map(WString::as_utfstr).collect();
        const LONG: &[WOption] = &[
            wopt(L!("quiet"), NoArgument, 'q'),
            wopt(L!("null-in"), NoArgument, 'z'),
            wopt(L!("null-out"), NoArgument, 'Z'),
            wopt(L!("no-extension"), NoArgument, 'E'),
            wopt(L!("no-symlinks"), NoArgument, 's'),
            wopt(L!("help"), NoArgument, 'h'),
        ];
        let short = if operation == "realpath" {
            L!("+hs")
        } else {
            L!("hqzZE")
        };
        let mut getopt = WGetopter::new(short, LONG, &mut args);
        while let Some(option) = getopt.next_opt() {
            match option {
                'h' => opts.help = true,
                's' if operation == "realpath" => opts.no_symlinks = true,
                'q' if operation != "realpath" => opts.quiet = true,
                'z' if operation != "realpath" => opts.null_in = true,
                'Z' if operation != "realpath" => opts.null_out = true,
                'E' if operation == "basename" => opts.no_extension = true,
                _ => {
                    return Err(error(
                        format!(
                            "{operation}: invalid option {}",
                            getopt.argv[getopt.wopt_index.saturating_sub(1)]
                        ),
                        2,
                    ));
                }
            }
        }
        let operands: Vec<String> = getopt.argv[getopt.wopt_index..]
            .iter()
            .map(|value| value.to_string())
            .collect();
        if opts.help {
            self.write(
                if operation == "realpath" {
                    b"realpath [-s|--no-symlinks] PATH ...\n"
                } else {
                    b"path dirname|basename|normalize|resolve [-qzZ] [PATH ...]\n"
                },
                host,
                output,
            )?;
            self.status = 0;
            return Ok(());
        }
        if operation == "realpath" && operands.is_empty() {
            return Err(error("expected at least one path", 2));
        }
        let from_stdin = operation != "realpath" && output.stdin_is_directly_redirected;
        if from_stdin && !operands.is_empty() {
            return Err(error(
                format!("{operation}: too many arguments when reading from stdin"),
                2,
            ));
        }
        let mut transformed = false;
        let mut failed = false;
        let mut inputs = operands.into_iter();
        let mut pending = std::collections::VecDeque::new();
        let mut buffered = Vec::new();
        let mut null_in = opts.null_in.then_some(true);
        let mut eof = false;
        loop {
            if host.cancelled() {
                self.status = 130;
                return Ok(());
            }
            let input = if from_stdin {
                loop {
                    if let Some(value) = pending.pop_front() {
                        break Some(value);
                    }
                    if eof {
                        break None;
                    }
                    let ReadResult::Line(bytes) = host.read_line(&output.redirects, false)? else {
                        return Err(error("input interrupted", 130));
                    };
                    eof = bytes.is_empty();
                    let null = *null_in.get_or_insert_with(|| bytes.contains(&0));
                    buffered.extend(bytes);
                    if buffered.len() > 16 * 1024 * 1024 {
                        return Err(error("path input limit exceeded", 1));
                    }
                    let separator = if null { 0 } else { b'\n' };
                    while let Some(end) = buffered.iter().position(|&byte| byte == separator) {
                        let remainder = buffered.split_off(end + 1);
                        buffered.pop();
                        pending.push_back(
                            String::from_utf8(std::mem::replace(&mut buffered, remainder))
                                .map_err(|_| error("path input must be UTF-8", 1))?,
                        );
                    }
                    if eof && !buffered.is_empty() {
                        pending.push_back(
                            String::from_utf8(std::mem::take(&mut buffered))
                                .map_err(|_| error("path input must be UTF-8", 1))?,
                        );
                    }
                }
            } else {
                inputs.next()
            };
            let Some(input) = input else {
                break;
            };
            if input.is_empty() && operation != "realpath" {
                continue;
            }
            if let Err(problem) = validate(&input) {
                failed = true;
                self.path_diagnostic(name, &format!("{input}: {}", problem.message), host, output)?;
                continue;
            }
            let wide = WString::from(input.as_str());
            let value = match operation {
                "realpath" => realpath(&input, opts.no_symlinks),
                "resolve" => resolve(&input),
                "dirname" => windows_dirname(&wide)
                    .map(|path| path.to_string())
                    .map_err(|reason| error(reason.to_string(), 1)),
                "basename" => windows_basename(&wide)
                    .map(|path| path.to_string())
                    .map_err(|reason| error(reason.to_string(), 1)),
                _ => windows_normalize(&wide)
                    .map(|path| path.to_string())
                    .map_err(|reason| error(reason.to_string(), 1)),
            };
            let mut value = match value {
                Ok(value) => value,
                Err(problem) => {
                    if operation != "resolve" || problem.status == 2 {
                        failed = true;
                        self.path_diagnostic(
                            name,
                            &format!("{input}: {}", problem.message),
                            host,
                            output,
                        )?;
                    }
                    continue;
                }
            };
            if opts.no_extension
                && !value.contains(['\\', '/'])
                && !matches!(value.as_str(), "." | "..")
            {
                if let Some(index) = value.rfind('.').filter(|&index| index != 0) {
                    value.truncate(index);
                }
            }
            if operation == "normalize" && value.starts_with('-') {
                value.insert_str(0, ".\\");
            }
            transformed |= value != input || matches!(operation, "resolve" | "realpath");
            if opts.quiet && transformed {
                break;
            }
            if !opts.quiet {
                self.write(value.as_bytes(), host, output)?;
                self.write(if opts.null_out { b"\0" } else { b"\n" }, host, output)?;
            }
        }
        self.status = i32::from(failed || !transformed);
        Ok(())
    }
}
