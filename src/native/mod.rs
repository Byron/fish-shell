//! Initial native shell frontend. The POSIX frontend retains the full editor,
//! configuration, and job-control implementation while these are ported.

#[cfg(windows)]
pub mod fish;
pub mod host;
#[cfg(windows)]
pub mod syntax;
pub mod worker;

use rash::{Error, Result};
use std::ffi::OsString;
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Fish,
    Rash,
}

impl Language {
    fn name(self) -> &'static str {
        match self {
            Self::Fish => "fish",
            Self::Rash => "rash",
        }
    }
}

#[derive(Debug)]
struct Options {
    language: Language,
    command: Option<Vec<u8>>,
    script: Option<OsString>,
    args: Vec<OsString>,
    syntax_only: bool,
    interactive: bool,
    startup: bool,
    login: bool,
}

enum Invocation {
    Run(Options),
    Help(Language),
    Version(Language),
}

fn parse_options(
    language: Language,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Invocation> {
    let mut options = Options {
        language,
        command: None,
        script: None,
        args: vec![],
        syntax_only: false,
        interactive: false,
        startup: false,
        login: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Invocation::Help(options.language)),
            Some("-v" | "--version") => return Ok(Invocation::Version(options.language)),
            Some("-c" | "--command") => {
                if options.command.is_some() {
                    return Err(Error::new(
                        "multiple -c options are not supported by the native frontend",
                    ));
                }
                options.command = Some(rash::os::to_bytes(
                    &args
                        .next()
                        .ok_or_else(|| Error::new("-c requires a command"))?,
                )?);
            }
            Some("-n" | "--no-execute") => options.syntax_only = true,
            Some("-i" | "--interactive") => options.interactive = true,
            Some("-l" | "--login") => options.login = true,
            Some("-N" | "--no-config") => {}
            Some("--rash-shell" | "--script-language=bash") => options.language = Language::Rash,
            Some("--script-language=fish") => options.language = Language::Fish,
            Some("--bash-startup") => options.startup = true,
            Some("--") => {
                if options.command.is_none() {
                    options.script = args.next();
                }
                options.args.extend(args);
                break;
            }
            Some(arg) if arg.starts_with('-') && arg != "-" => {
                return Err(Error::new(format!("unsupported native option: {arg}")));
            }
            _ => {
                if options.command.is_none() {
                    options.script = Some(arg);
                } else {
                    options.args.push(arg);
                }
                options.args.extend(args);
                break;
            }
        }
    }
    if options.startup && options.language != Language::Rash {
        return Err(Error::new("--bash-startup requires Rash"));
    }
    if options.interactive
        && (options.script.is_some() || options.command.is_some() || options.syntax_only)
    {
        return Err(Error::new(
            "-i cannot be combined with a script, -c, or -n in the native frontend",
        ));
    }
    Ok(Invocation::Run(options))
}

/// Both interpreters need more stack than Windows' default main thread provides.
pub fn main(language: Language) -> i32 {
    let worker = std::thread::Builder::new()
        .name(language.name().to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || run(language));
    match worker {
        Ok(worker) => match worker.join() {
            Ok(status) => status,
            Err(panic) => std::panic::resume_unwind(panic),
        },
        Err(error) => report(language, error.into()),
    }
}

fn report(language: Language, error: Error) -> i32 {
    let _ = writeln!(io::stderr(), "{}: {}", language.name(), error.message);
    error.status
}

pub fn unsupported_tool(name: &str) -> i32 {
    let _ = writeln!(
        io::stderr(),
        "{name}: not yet supported by the native Windows backend"
    );
    2
}

fn run(language: Language) -> i32 {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        if let Some(worker_language) = arg
            .to_str()
            .and_then(|a| a.strip_prefix("--native-worker="))
        {
            return match (args.next(), args.next()) {
                (Some(path), None) => worker::run(worker_language, std::path::Path::new(&path))
                    .unwrap_or_else(|error| report(language, error)),
                _ => report(
                    language,
                    Error::new("invalid interpreter worker invocation"),
                ),
            };
        }
    }
    let invocation = match parse_options(language, std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) => return report(language, error),
    };
    match invocation {
        Invocation::Help(language) => {
            let _ = writeln!(
                io::stdout(),
                "Usage: {} [OPTIONS] [SCRIPT [ARG ...]]\n\
                 Options: -c COMMAND, -n, -i, -N, --rash-shell, --script-language=fish|bash\n\
                 Native backend: initial command/script support and a basic line prompt.\n\
                 See doc_internal/native-windows.md for supported syntax and limitations.",
                language.name()
            );
            0
        }
        Invocation::Version(language) => {
            let _ = writeln!(
                io::stdout(),
                "{}, version {} (native)",
                language.name(),
                crate::BUILD_VERSION
            );
            0
        }
        Invocation::Run(options) => {
            let language = options.language;
            execute(options).unwrap_or_else(|error| report(language, error))
        }
    }
}

enum Engine {
    Rash(rash::Session),
    #[cfg(windows)]
    Fish(fish::Session),
}

impl Engine {
    fn new(options: &Options, interactive: bool) -> Result<Self> {
        match options.language {
            Language::Rash => {
                let mut env = std::env::vars_os()
                    // Windows has hidden entries such as =C: for per-drive
                    // working directories. These are not shell variables.
                    .filter(|(name, _)| {
                        !cfg!(windows) || !name.as_encoded_bytes().starts_with(b"=")
                    })
                    .map(|(name, value)| {
                        let mut name = rash::os::to_bytes(&name)?;
                        // Windows treats environment names case-insensitively;
                        // shell variables are case-sensitive. Use the shell's
                        // spelling for PATH and prevent a stale `Path` export.
                        if cfg!(windows) && name.eq_ignore_ascii_case(b"PATH") {
                            name = b"PATH".to_vec();
                        }
                        Ok((name, rash::os::to_bytes(&value)?))
                    })
                    .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
                if !env.contains_key(b"HOME".as_slice()) {
                    if let Some(home) = env.get(b"USERPROFILE".as_slice()).cloned() {
                        env.insert(b"HOME".to_vec(), home);
                    }
                }
                let cwd = rash::os::to_bytes(std::env::current_dir()?.as_os_str())?;
                let mut session = rash::Session::new(env, cwd, interactive, options.login);
                let mut args = options.args.iter();
                session.arg0 = if let Some(script) = &options.script {
                    rash::os::to_bytes(script)?
                } else if options.command.is_some() {
                    args.next()
                        .map(|arg| rash::os::to_bytes(arg))
                        .transpose()?
                        .unwrap_or_else(|| b"rash".to_vec())
                } else {
                    b"rash".to_vec()
                };
                session.positional = args
                    .map(|arg| rash::os::to_bytes(arg))
                    .collect::<Result<_>>()?;
                Ok(Self::Rash(session))
            }
            #[cfg(windows)]
            Language::Fish => {
                let mut session = fish::Session::new()?;
                session.set_args(
                    options
                        .args
                        .iter()
                        .map(|arg| {
                            arg.to_str()
                                .map(str::to_owned)
                                .ok_or_else(|| Error::new("native Fish arguments must be Unicode"))
                        })
                        .collect::<Result<_>>()?,
                );
                Ok(Self::Fish(session))
            }
            #[cfg(not(windows))]
            Language::Fish => Err(Error::new(
                "use the fish executable for Fish syntax on this platform",
            )),
        }
    }

    fn evaluate(
        &mut self,
        source: &[u8],
        host: &mut host::NativeHost,
        syntax_only: bool,
    ) -> Result<i32> {
        match self {
            Self::Rash(session) => {
                if syntax_only {
                    let (status, message) = session.check_syntax(source);
                    if status == 0 {
                        Ok(0)
                    } else {
                        Err(Error::new(message))
                    }
                } else {
                    session.eval(source, host).map(|result| result.status)
                }
            }
            #[cfg(windows)]
            Self::Fish(session) => {
                let source = std::str::from_utf8(source)
                    .map_err(|_| Error::new("native Fish source must be UTF-8"))?;
                if syntax_only {
                    fish::check_syntax(source)?;
                    Ok(0)
                } else {
                    session.eval(source, host)
                }
            }
        }
    }

    fn exited(&self) -> bool {
        match self {
            Self::Rash(session) => session.exited.is_some(),
            #[cfg(windows)]
            Self::Fish(session) => session.exit_requested(),
        }
    }

    fn incomplete(&self, source: &[u8]) -> bool {
        match self {
            Self::Rash(session) => matches!(
                rash::syntax::check_with_aliases(source, &session.aliases),
                rash::syntax::SyntaxStatus::Incomplete
            ),
            #[cfg(windows)]
            Self::Fish(_) => fish::is_incomplete(std::str::from_utf8(source).unwrap_or_default()),
        }
    }
}

#[cfg_attr(not(windows), allow(irrefutable_let_patterns))]
fn execute(options: Options) -> Result<i32> {
    let interactive = options.interactive
        || (options.command.is_none()
            && options.script.is_none()
            && !options.syntax_only
            && io::stdin().is_terminal());
    let mut engine = Engine::new(&options, interactive)?;
    let mut host = host::NativeHost::new()?;
    if options.startup && !options.syntax_only {
        if let Engine::Rash(session) = &mut engine {
            session.startup(&mut host)?;
        }
    }
    let mut status = 0;
    if !engine.exited() {
        if interactive {
            let mut source = Vec::new();
            loop {
                host.clear_console_requests();
                for notice in host.poll_jobs()? {
                    writeln!(io::stderr(), "{notice}")?;
                }
                write!(
                    io::stdout(),
                    "{}> ",
                    if source.is_empty() {
                        options.language.name()
                    } else {
                        "..."
                    }
                )?;
                io::stdout().flush()?;
                let mut line = Vec::new();
                if io::stdin().lock().read_until(b'\n', &mut line)? == 0 {
                    if !source.is_empty() {
                        status = engine
                            .evaluate(&source, &mut host, false)
                            .unwrap_or_else(|error| report(options.language, error));
                    }
                    break;
                }
                if cfg!(windows) && line.ends_with(b"\r\n") {
                    line.remove(line.len() - 2);
                }
                source.extend(line);
                if engine.incomplete(&source) {
                    continue;
                }
                status = engine
                    .evaluate(&source, &mut host, false)
                    .unwrap_or_else(|error| report(options.language, error));
                source.clear();
                if engine.exited() {
                    break;
                }
            }
        } else {
            let source = if let Some(command) = options.command {
                command
            } else if let Some(path) = options.script.filter(|p| p != "-") {
                std::fs::read(path)?
            } else {
                let mut source = Vec::new();
                io::stdin().read_to_end(&mut source)?;
                source
            };
            status = engine.evaluate(&source, &mut host, options.syntax_only)?;
        }
    }
    if !options.syntax_only {
        if let Engine::Rash(session) = &mut engine {
            session.finish(&mut host)?;
            status = session.status;
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_arguments_stop_option_parsing() {
        let Invocation::Run(options) = parse_options(
            Language::Rash,
            ["-c", "echo $1", "name", "-n"].map(OsString::from),
        )
        .unwrap() else {
            panic!()
        };
        assert!(!options.syntax_only);
        assert_eq!(options.args, ["name", "-n"]);
    }

    #[test]
    fn invalid_options_fail_before_execution() {
        for args in [
            vec!["-c"],
            vec!["--unknown"],
            vec!["-i", "-c", "echo x"],
            vec!["--bash-startup"],
        ] {
            assert!(parse_options(Language::Fish, args.into_iter().map(OsString::from)).is_err());
        }
    }
}
