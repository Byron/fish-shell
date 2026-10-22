//! Portable lexical path operations with Windows filesystem resolution.
use super::*;
use crate::path_lexical::{windows_basename, windows_dirname, windows_normalize, windows_validate};
use fish_wgetopt::{ArgType::*, WGetopter, WOption, wopt};
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
    relative: bool,
    reverse: bool,
    unique: bool,
    invert: bool,
    all: bool,
    key: Option<String>,
    has_types: bool,
    types: Vec<String>,
    has_permissions: bool,
    permissions: Vec<String>,
}

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SORT_PATHS: usize = 100_000;

const SUBCOMMANDS: &[&str] = &[
    "dirname",
    "basename",
    "normalize",
    "resolve",
    "extension",
    "change-extension",
    "filter",
    "is",
    "mtime",
    "sort",
];

fn extension_offset(path: &wstr) -> Result<Option<usize>> {
    let basename = windows_basename(path).map_err(|reason| error(reason.to_string(), 2))?;
    // Windows basenames preserve drive and UNC roots. Dots in a server or
    // share name are part of that root, not a file's extension.
    if !Path::new(&path.to_string())
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
    {
        return Ok(None);
    }
    Ok(crate::path_lexical::extension_offset(path, &basename))
}

fn modification_seconds(path: &str) -> Option<i64> {
    let time = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(value) => i64::try_from(value.as_secs()).ok()?,
        Err(value) => {
            let duration = value.duration();
            -i64::try_from(duration.as_secs()).ok()? - i64::from(duration.subsec_nanos() != 0)
        }
    })
}

fn filter_matches(path: &str, options: &Options) -> bool {
    let path = Path::new(path);
    let metadata = std::fs::metadata(path).ok();
    if !options.has_types && !options.has_permissions {
        return metadata.is_some();
    }
    if options.has_types
        && !options.types.iter().any(|kind| match kind.as_str() {
            "file" => metadata.as_ref().is_some_and(std::fs::Metadata::is_file),
            "dir" => metadata.as_ref().is_some_and(std::fs::Metadata::is_dir),
            "link" => {
                metadata.is_some()
                    && std::fs::symlink_metadata(path).is_ok_and(|value| value.is_symlink())
            }
            "char" => super::test_builtin::is_character_device(path),
            _ => false,
        })
    {
        return false;
    }
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::FILE_EXECUTE;
    let access = options.permissions.iter().fold(0, |mask, name| {
        mask | match name.as_str() {
            "read" => GENERIC_READ,
            "write" => GENERIC_WRITE,
            "exec" => FILE_EXECUTE,
            _ => unreachable!("permission validated during parsing"),
        }
    });
    access == 0 || super::test_builtin::open_for_access(path, access).is_ok()
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
        output.append(host.write(
            2,
            format!("{name}: {message}\n").as_bytes(),
            &output.redirects,
            output.capture,
        )?)?;
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
                        b"path dirname|basename|normalize|resolve|extension|change-extension|filter|is|mtime|sort [OPTIONS] [PATH ...]\n",
                        host,
                        output,
                    )?;
                    self.status = 0;
                    return Ok(());
                }
                Some((command, args)) if SUBCOMMANDS.contains(&command.as_str()) => {
                    (command.as_str(), args)
                }
                Some((command, _)) => {
                    return Err(error(format!("unsupported subcommand {command}"), 2));
                }
                None => return Err(error("expected a subcommand", 2)),
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
            wopt(L!("relative"), NoArgument, 'R'),
            wopt(L!("reverse"), NoArgument, 'r'),
            wopt(L!("unique"), NoArgument, 'u'),
            wopt(L!("key"), RequiredArgument, '\x01'),
            wopt(L!("type"), RequiredArgument, 't'),
            wopt(L!("perm"), RequiredArgument, 'p'),
            wopt(L!("invert"), NoArgument, 'v'),
            wopt(L!("all"), NoArgument, '\x02'),
        ];
        let filter = matches!(operation, "filter" | "is");
        let short = match operation {
            "realpath" => L!("+hs"),
            "filter" | "is" => L!("hqzZt:p:rwxfdlv"),
            "mtime" => L!("hqzZR"),
            "sort" => L!("hqzZru"),
            "basename" => L!("hqzZE"),
            _ => L!("hqzZ"),
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
                'R' if operation == "mtime" => opts.relative = true,
                'r' if operation == "sort" => opts.reverse = true,
                'u' if operation == "sort" => opts.unique = true,
                '\x01' if operation == "sort" => {
                    let key = getopt.woptarg.unwrap().to_string();
                    if !matches!(key.as_str(), "basename" | "dirname" | "path") {
                        return Err(error(format!("sort: invalid sort key {key}"), 2));
                    }
                    opts.key = Some(key);
                }
                'v' if filter => opts.invert = true,
                '\x02' if filter => opts.all = true,
                'f' | 'd' | 'l' if filter => {
                    opts.has_types = true;
                    opts.types.push(
                        match option {
                            'f' => "file",
                            'd' => "dir",
                            _ => "link",
                        }
                        .into(),
                    );
                }
                'r' | 'w' | 'x' if filter => {
                    opts.has_permissions = true;
                    opts.permissions.push(
                        match option {
                            'r' => "read",
                            'w' => "write",
                            _ => "exec",
                        }
                        .into(),
                    );
                }
                't' | 'p' if filter => {
                    if option == 't' {
                        opts.has_types = true;
                    } else {
                        opts.has_permissions = true;
                    }
                    for value in getopt
                        .woptarg
                        .unwrap()
                        .to_string()
                        .split(',')
                        .filter(|value| !value.is_empty())
                    {
                        if option == 't' {
                            if !matches!(
                                value,
                                "file" | "dir" | "link" | "block" | "char" | "fifo" | "socket"
                            ) {
                                return Err(error(format!("{operation}: invalid type {value}"), 2));
                            }
                            opts.types.push(value.into());
                        } else {
                            if matches!(value, "suid" | "sgid" | "user" | "group") {
                                return Err(error(
                                    format!(
                                        "{operation}: permission {value} requires POSIX ownership or mode bits"
                                    ),
                                    2,
                                ));
                            }
                            if !matches!(value, "read" | "write" | "exec") {
                                return Err(error(
                                    format!("{operation}: invalid permission {value}"),
                                    2,
                                ));
                            }
                            opts.permissions.push(value.into());
                        }
                    }
                }
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
        let mut operands: Vec<String> = getopt.argv[getopt.wopt_index..]
            .iter()
            .map(|value| value.to_string())
            .collect();
        if opts.help {
            self.write(
                if operation == "realpath" {
                    b"realpath [-s|--no-symlinks] PATH ...\n"
                } else {
                    b"path dirname|basename|normalize|resolve|extension|change-extension|filter|is|mtime|sort [OPTIONS] [PATH ...]\n"
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
        let replacement = if operation == "change-extension" {
            if operands.is_empty() {
                return Err(error("change-extension: missing replacement", 2));
            }
            Some(operands.remove(0))
        } else {
            None
        };
        opts.quiet |= operation == "is";
        let from_stdin = operation != "realpath" && output.stdin_is_directly_redirected;
        if from_stdin && !operands.is_empty() {
            return Err(error(
                format!("{operation}: too many arguments when reading from stdin"),
                2,
            ));
        }
        let mut transformed = false;
        let mut failed = false;
        let mut seen = 0;
        let mut matched = 0;
        let mut sorted: Vec<(WString, WString)> = Vec::new();
        let mut sort_bytes = 0usize;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |value| value.as_secs() as i64);
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
                    if buffered.len() > MAX_INPUT_BYTES {
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
            seen += 1;
            if input.is_empty()
                && matches!(operation, "dirname" | "basename" | "normalize" | "resolve")
            {
                continue;
            }
            if let Err(problem) = validate(&input) {
                failed = true;
                self.path_diagnostic(name, &format!("{input}: {}", problem.message), host, output)?;
                continue;
            }
            let wide = WString::from(input.as_str());
            if filter {
                if filter_matches(&input, &opts) == opts.invert {
                    continue;
                }
                matched += 1;
                transformed = true;
                if opts.all {
                    continue;
                }
                if opts.quiet {
                    break;
                }
                if input.starts_with('-') {
                    self.write(b"./", host, output)?;
                }
                self.write(input.as_bytes(), host, output)?;
                self.write(if opts.null_out { b"\0" } else { b"\n" }, host, output)?;
                continue;
            }
            if operation == "sort" {
                sort_bytes = sort_bytes.saturating_add(input.len());
                if sort_bytes > MAX_INPUT_BYTES || sorted.len() >= MAX_SORT_PATHS {
                    return Err(error("path sort input limit exceeded", 1));
                }
                let key = match opts.key.as_deref() {
                    Some("basename") => windows_basename(&wide),
                    Some("dirname") => windows_dirname(&wide),
                    _ => Ok(wide.clone()),
                }
                .map_err(|reason| error(reason.to_string(), 2))?;
                sorted.push((key, wide));
                continue;
            }
            let mut succeeded = None;
            let value = match operation {
                "mtime" => {
                    let Some(seconds) = modification_seconds(&input) else {
                        continue;
                    };
                    succeeded = Some(true);
                    Ok(if opts.relative {
                        now - seconds
                    } else {
                        seconds
                    }
                    .to_string())
                }
                "extension" => {
                    let offset = extension_offset(&wide)?;
                    succeeded = Some(offset.is_some());
                    Ok(offset.map_or_else(String::new, |at| wide[at..].to_string()))
                }
                "change-extension" => {
                    let mut value =
                        wide[..extension_offset(&wide)?.unwrap_or(wide.len())].to_owned();
                    let replacement = replacement.as_ref().unwrap();
                    if !replacement.is_empty() {
                        if !replacement.starts_with('.') {
                            value.push('.');
                        }
                        value.push_utfstr(&WString::from(replacement.as_str()));
                    }
                    succeeded = Some(true);
                    Ok(value.to_string())
                }
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
            transformed |=
                succeeded.unwrap_or(value != input || matches!(operation, "resolve" | "realpath"));
            if opts.quiet && transformed {
                break;
            }
            if !opts.quiet {
                self.write(value.as_bytes(), host, output)?;
                self.write(if opts.null_out { b"\0" } else { b"\n" }, host, output)?;
            }
        }
        if operation == "sort" {
            if host.cancelled() {
                self.status = 130;
                return Ok(());
            }
            sorted.sort_by(|left, right| {
                let order = fish_util::wcsfilecmp_glob(&left.0, &right.0);
                if opts.reverse { order.reverse() } else { order }
            });
            if opts.unique {
                sorted.dedup_by(|left, right| left.0 == right.0);
            }
            if host.cancelled() {
                self.status = 130;
                return Ok(());
            }
            if !opts.quiet {
                for (_, path) in sorted {
                    if host.cancelled() {
                        self.status = 130;
                        return Ok(());
                    }
                    self.write(path.to_string().as_bytes(), host, output)?;
                    self.write(if opts.null_out { b"\0" } else { b"\n" }, host, output)?;
                }
            }
            transformed = true;
        }
        if opts.all {
            transformed = seen > 0 && matched == seen;
        }
        self.status = i32::from(failed || !transformed);
        Ok(())
    }
}
