//! Rash owns Bash language state in-process; Fish owns jobs and the terminal.

use crate::env::{EnvMode, Environment, Statuses};
use crate::parser::{Parser, ParserEnvSetMode};
use crate::prelude::*;
use fish_widestring::{bytes2wcstring, wcs2bytes};
use rash::host::{OpenMode, RedirectAction, Redirection, RunResult};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct State {
    pub fish_evaluation: bool,
    pub status: i32,
    pub cwd: WString,
    pub pipeline: Vec<i32>,
    pub scalars: BTreeMap<WString, (bool, WString)>,
    pub functions: Vec<WString>,
    pub builtins: BTreeMap<WString, WString>,
    pub aliases: BTreeMap<WString, WString>,
}

impl State {
    pub fn from_fish(vars: &dyn Environment) -> Self {
        Self {
            fish_evaluation: true,
            functions: crate::function::get_names(true, vars),
            builtins: crate::builtins::builtin_get_names()
                .map(|name| (name.to_owned(), WString::new()))
                .collect(),
            ..Self::default()
        }
    }
    pub fn completion_environment(&self, vars: &crate::env::EnvStack) -> crate::env::EnvStack {
        let env = vars.create_child(false);
        env.push(false);
        self.apply_scalars(&env);
        env
    }

    pub fn apply_scalars(&self, vars: &crate::env::EnvStack) {
        self.set_scalars(vars, false);
    }

    pub fn scoped_scalars<'a>(&self, vars: &'a crate::env::EnvStack) -> impl Drop + use<'a> {
        let saved = self
            .scalars
            .keys()
            .filter(|name| self.scalar_visible(name, vars))
            .map(|name| (name.clone(), vars.getf(name, EnvMode::GLOBAL)))
            .collect::<Vec<_>>();
        self.set_scalars(vars, true);
        fish_common::ScopeGuard::new(saved, move |saved| {
            for (name, value) in saved {
                match value {
                    Some(value) => {
                        vars.set(
                            &name,
                            crate::env::EnvSetMode::new(
                                EnvMode {
                                    global: true,
                                    export: Some(value.exports()),
                                    pathvar: Some(value.is_pathvar()),
                                    ..EnvMode::default()
                                },
                                false,
                            ),
                            value.as_list().to_vec(),
                        );
                    }
                    None => {
                        vars.remove(&name, crate::env::EnvSetMode::new(EnvMode::GLOBAL, false));
                    }
                }
            }
        })
    }

    fn scalar_visible(&self, name: &wstr, vars: &crate::env::EnvStack) -> bool {
        shared_name(name)
            && !self
                .scalars
                .get(name)
                .is_some_and(|(exported, _)| *exported)
            && !name.starts_with("BASH")
            && name != "IFS"
            && !vars
                .get(name)
                .is_some_and(|v| v.is_read_only() || v.is_pathvar() || v.exports())
    }

    pub fn commands(&self) -> Vec<crate::complete::Completion> {
        self.functions
            .iter()
            .map(|n| (n, L!("Bash function")))
            .chain(self.aliases.keys().map(|n| (n, L!("Bash alias"))))
            .chain(self.builtins.iter().map(|(n, d)| (n, d.as_utfstr())))
            .map(|(name, description)| {
                let mut c = crate::complete::Completion::from_completion(name.clone());
                c.description = description.to_owned();
                c
            })
            .collect()
    }

    fn set_scalars(&self, vars: &crate::env::EnvStack, global: bool) {
        for (name, (exported, value)) in &self.scalars {
            if !self.scalar_visible(name, vars) {
                continue;
            }
            vars.set_one(
                name,
                crate::env::EnvSetMode::new(
                    crate::env::EnvMode {
                        local: !global,
                        global,
                        export: Some(*exported),
                        pathvar: Some(false),
                        ..crate::env::EnvMode::default()
                    },
                    false,
                ),
                value.clone(),
            );
        }
    }
}

pub struct Session {
    engine: rash::Session,
    signals: crate::signal::BashSignals,
    pub state: State,
    fish_exports: BTreeMap<WString, WString>,
    fish_cwd: WString,
    pub exited: Option<i32>,
    pub startup_exited: bool,
    pub failed: bool,
}

fn shared_name(name: &wstr) -> bool {
    !name.starts_with("fish_")
        && !name.starts_with("FISH_")
        && ![L!("PWD"), L!("SHLVL"), L!("_"), L!("SHELL")].contains(&name)
}

fn exports(vars: &dyn Environment) -> BTreeMap<WString, WString> {
    vars.get_names(EnvMode::EXPORTED)
        .into_iter()
        .filter(|name| shared_name(name))
        .filter_map(|name| vars.get(&name).map(|v| (name, v.as_string())))
        .collect()
}

fn input_bytes(value: &wstr) -> io::Result<Vec<u8>> {
    let bytes = wcs2bytes(value);
    if bytes.contains(&0) || bytes.len() >= 1024 * 1024 - 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bash input contains NUL or exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

impl Session {
    pub fn start(
        parser: &mut Parser,
        startup: bool,
        login: bool,
        interactive: bool,
    ) -> io::Result<Self> {
        let vars = parser.vars();
        let env = vars
            .get_names(EnvMode::EXPORTED)
            .into_iter()
            .filter_map(|name| {
                vars.get(&name)
                    .map(|value| (wcs2bytes(&name), wcs2bytes(&value.as_string())))
            })
            .collect();
        let cwd = vars
            .get(L!("PWD"))
            .map_or_else(|| vars.get_pwd_slash(), |v| v.as_string());
        let mut session = Self {
            engine: rash::Session::new(env, wcs2bytes(&cwd), interactive, login),
            signals: crate::signal::BashSignals::new(interactive)?,
            state: State::default(),
            fish_exports: exports(vars),
            fish_cwd: vars.get_pwd_slash(),
            exited: None,
            startup_exited: false,
            failed: false,
        };
        if startup {
            if let Err(error) = session.engine.startup(&mut FishHost {
                parser,
                signals: &mut session.signals,
            }) {
                if !interactive {
                    return Err(io::Error::other(error));
                }
                session.engine.status = error.status;
                eprintf!("rash: %s\n", error.message);
            }
            session.startup_exited = session.engine.exited.is_some();
        }
        session.snapshot();
        Ok(session)
    }

    pub fn set_script_args(&mut self, args: &[WString]) {
        self.engine.arg0 = wcs2bytes(&args[0]);
        self.engine.positional = args[1..].iter().map(wcs2bytes).collect();
    }

    pub fn cancel_exit(&mut self) {
        self.exited = None;
        self.engine.exited = None;
    }

    pub fn startup(&mut self, parser: &mut Parser) -> io::Result<()> {
        self.engine
            .startup(&mut FishHost {
                parser,
                signals: &mut self.signals,
            })
            .map_err(io::Error::other)?;
        self.startup_exited = self.engine.exited.is_some();
        self.snapshot();
        Ok(())
    }

    pub fn finish(&mut self, parser: &mut Parser) -> io::Result<()> {
        if self.failed {
            self.exited = Some(125);
            return self.publish(parser);
        }
        if self.exited.is_none() {
            if let Err(error) = self.engine.finish(&mut FishHost {
                parser,
                signals: &mut self.signals,
            }) {
                self.engine.status = error.status;
                if crate::reader::reader_exit_signal() == 0 {
                    eprintf!("rash: %s\n", error.message);
                }
            }
            self.snapshot();
        }
        self.publish(parser)
    }

    fn snapshot(&mut self) {
        self.state = State {
            fish_evaluation: false,
            status: self.engine.status,
            cwd: bytes2wcstring(&self.engine.cwd),
            pipeline: self.engine.pipeline.clone(),
            scalars: self
                .engine
                .variables
                .iter()
                .filter_map(|(name, variable)| {
                    let rash::Value::Scalar(value) = &variable.value else {
                        return None;
                    };
                    Some((
                        bytes2wcstring(name),
                        (variable.exported, bytes2wcstring(value)),
                    ))
                })
                .collect(),
            functions: self
                .engine
                .functions
                .keys()
                .map(|name| bytes2wcstring(name))
                .collect(),
            aliases: self
                .engine
                .aliases
                .iter()
                .map(|(name, value)| (bytes2wcstring(name), bytes2wcstring(value)))
                .collect(),
            builtins: rash::session::BUILTINS
                .iter()
                .map(|name| (bytes2wcstring(name), L!("Bash builtin").to_owned()))
                .collect(),
        };
        self.exited = self.engine.exited;
    }

    pub fn eval(&mut self, input: &wstr, parser: &mut Parser) -> io::Result<()> {
        self.eval_bytes(&input_bytes(input)?, parser)
    }

    pub fn eval_bytes(&mut self, input: &[u8], parser: &mut Parser) -> io::Result<()> {
        self.synchronize(parser.vars())?;
        if let Err(error) = self.engine.eval(
            input,
            &mut FishHost {
                parser,
                signals: &mut self.signals,
            },
        ) {
            self.engine.status = error.status;
            self.engine.pipeline = vec![error.status];
            if crate::reader::reader_exit_signal() == 0 {
                eprintf!("rash: %s\n", error.message);
            }
        }
        self.snapshot();
        Ok(())
    }

    fn synchronize(&mut self, vars: &dyn Environment) -> io::Result<()> {
        let current = exports(vars);
        let cwd = vars.get_pwd_slash();
        if cwd != self.fish_cwd {
            self.engine.set_cwd(wcs2bytes(
                &cwd[..if cwd.len() > 1 {
                    cwd.len() - 1
                } else {
                    cwd.len()
                }],
            ));
        }
        for (name, value) in &current {
            if self.fish_exports.get(name) != Some(value) {
                self.engine
                    .set_export(wcs2bytes(name), Some(wcs2bytes(value)))
                    .map_err(io::Error::other)?;
            }
        }
        for name in self
            .fish_exports
            .keys()
            .filter(|name| !current.contains_key(*name))
        {
            self.engine
                .set_export(wcs2bytes(name), None)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    pub fn check_syntax(&mut self, source: &wstr) -> io::Result<(i32, WString)> {
        let (status, error) = self.engine.check_syntax(&input_bytes(source)?);
        Ok((status, WString::from(error)))
    }

    pub fn publish(&mut self, parser: &mut Parser) -> io::Result<()> {
        if let Some(status) = self.exited {
            parser.set_last_statuses(Statuses::just(status));
            parser.libdata_mut().exit_current_script = true;
            return Ok(());
        }
        let state = &self.state;
        let next_exports: BTreeMap<_, _> = state
            .scalars
            .iter()
            .filter(|(name, (exported, _))| *exported && shared_name(name))
            .map(|(name, (_, value))| (name.clone(), value.clone()))
            .collect();
        for name in self.fish_exports.keys() {
            if !next_exports.contains_key(name) {
                if let Some((_, value)) = state.scalars.get(name) {
                    parser.set_one(
                        name,
                        ParserEnvSetMode::new(EnvMode {
                            global: true,
                            ..EnvMode::UNEXPORT
                        }),
                        value.clone(),
                    );
                } else {
                    parser.remove_var(name, ParserEnvSetMode::new(EnvMode::GLOBAL));
                }
            }
        }
        for (name, value) in next_exports {
            if self.fish_exports.get(&name) != Some(&value) {
                parser.set_var_and_fire(
                    &name,
                    ParserEnvSetMode::new(EnvMode::GLOBAL_EXPORTED),
                    vec![value],
                );
            }
        }
        if !state.cwd.is_empty()
            && parser
                .vars()
                .get(L!("PWD"))
                .is_none_or(|v| v.as_string() != state.cwd)
        {
            crate::builtins::cd::try_chdir(parser, &state.cwd, false).map_err(io::Error::from)?;
        }
        parser.set_last_statuses(Statuses {
            status: state.status,
            kill_signal: None,
            pipestatus: state.pipeline.clone(),
        });
        self.fish_exports = exports(parser.vars());
        self.fish_cwd = parser.vars().get_pwd_slash();
        Ok(())
    }
}

/// Adapter for already-expanded Rash commands. No user shell source is evaluated by Fish.
struct FishHost<'a> {
    parser: &'a mut Parser,
    signals: &'a mut crate::signal::BashSignals,
}

fn fish_word(bytes: &[u8]) -> rash::Result<WString> {
    if bytes.contains(&0) {
        return Err(rash::Error::new(
            "NUL is not permitted in an argument or path",
        ));
    }
    Ok(fish_common::escape(&bytes2wcstring(bytes)))
}

fn job_pid(parser: &Parser, argument: &[u8]) -> rash::Result<Vec<u8>> {
    if !argument.starts_with(b"%") {
        return Ok(argument.to_vec());
    }
    let jobs = parser
        .jobs()
        .iter()
        .filter(|job| job.is_constructed() && !job.is_completed())
        .collect::<Vec<_>>();
    let job = match argument {
        b"%" | b"%%" | b"%+" => jobs.first().copied(),
        b"%-" => jobs.get(1).copied(),
        _ => {
            let id = std::str::from_utf8(&argument[1..])
                .ok()
                .and_then(|s| s.parse::<i64>().ok());
            jobs.iter()
                .copied()
                .find(|job| Some(job.job_id().as_num()) == id)
        }
    };
    job.and_then(|job| job.last_pid())
        .map(|pid| pid.to_string().into_bytes())
        .ok_or_else(|| rash::Error::new("no such job"))
}

fn temporary_input(bytes: &[u8]) -> rash::Result<fish_tempfile::TempFile> {
    let mut file = fish_tempfile::new_file()?;
    file.get()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.get_mut().write_all(bytes)?;
    file.get_mut().rewind()?;
    Ok(file)
}

impl rash::host::Host for FishHost<'_> {
    fn prepare_redirects(&mut self, redirects: &[Redirection]) -> rash::Result<Vec<Redirection>> {
        redirects
            .iter()
            .map(|redirect| {
                let action = match &redirect.action {
                    RedirectAction::File { .. } | RedirectAction::Here(_) => {
                        let mut targets = redirect_targets(std::slice::from_ref(redirect), false)?;
                        let Target::File(file) = targets.remove(&redirect.fd).unwrap() else {
                            unreachable!()
                        };
                        RedirectAction::Opened(file)
                    }
                    action => action.clone(),
                };
                Ok(Redirection {
                    fd: redirect.fd,
                    action,
                })
            })
            .collect()
    }

    fn run_pipeline(
        &mut self,
        stages: &[rash::host::Command],
        background: bool,
        capture: bool,
    ) -> rash::Result<RunResult> {
        if self.signals.in_subshell()
            && stages.iter().any(|stage| {
                stage.builtin
                    && stage.argv.first().is_some_and(|name| {
                        [b"jobs".as_slice(), b"fg", b"bg", b"wait", b"disown"]
                            .contains(&name.as_slice())
                    })
            })
        {
            return Err(rash::Error::new(
                "job control inside subshells requires process isolation and is not supported",
            ));
        }
        if let [stage] = stages {
            if stage.builtin && stage.argv.first().is_some_and(|arg| arg == b"kill") {
                let mut args = &stage.argv[1..];
                let mut signal = libc::SIGTERM;
                if let Some(option) = args.first() {
                    let name = if option == b"-s" {
                        args = &args[1..];
                        Some(
                            args.first()
                                .ok_or_else(|| rash::Error::new("kill: expected signal"))?
                                .as_slice(),
                        )
                    } else if option != b"--" {
                        option.strip_prefix(b"-")
                    } else {
                        None
                    };
                    if let Some(name) = name {
                        signal = if name == b"0" {
                            0
                        } else {
                            crate::signal::RawSignal::parse(&bytes2wcstring(name))
                                .ok_or_else(|| rash::Error::new("kill: invalid signal"))?
                                .code()
                        };
                        args = &args[1..];
                    }
                }
                if args.first().is_some_and(|arg| arg == b"--") {
                    args = &args[1..];
                }
                if args.is_empty() {
                    return Err(rash::Error::new("kill: expected process or job"));
                }
                let mut status = 0;
                for arg in args {
                    if self.signals.in_subshell() && arg.starts_with(b"%") {
                        return Err(rash::Error::new(
                            "job references inside subshells are not supported",
                        ));
                    }
                    let pid = job_pid(self.parser, arg)?;
                    let pid = std::str::from_utf8(&pid)
                        .ok()
                        .and_then(|pid| pid.parse::<i32>().ok())
                        .ok_or_else(|| rash::Error::new("kill: invalid process id"))?;
                    // The OS validates the requested signal and process/group id.
                    if unsafe { libc::kill(pid, signal) } != 0 {
                        status = 1;
                        let message = format!("kill: {}\n", io::Error::last_os_error());
                        self.write(2, message.as_bytes(), &stage.redirects, false)?;
                    }
                }
                return Ok(RunResult {
                    status,
                    pipeline: vec![status],
                    ..RunResult::default()
                });
            }
        }
        if stages.is_empty() {
            return Ok(RunResult::default());
        }
        if capture && background {
            return Err(rash::Error::new(
                "background commands in command substitutions are not supported yet",
            ));
        }
        for stage in stages {
            let Some(name) = stage.argv.first() else {
                return Err(rash::Error::new("empty external command"));
            };
            if stage.builtin
                && ![
                    b"jobs".as_slice(),
                    b"fg",
                    b"bg",
                    b"wait",
                    b"disown",
                    b"exec",
                ]
                .contains(&name.as_slice())
            {
                return Err(rash::Error::new("unsupported host builtin"));
            }
            if stage.builtin && (stages.len() != 1 || background) {
                return Err(rash::Error::new(
                    "job-control builtins cannot run in a pipeline or background command",
                ));
            }
            if stage.builtin
                && name == b"exec"
                && (stage.argv.len() < 2 || stage.argv[1].starts_with(b"-"))
            {
                return Err(rash::Error::new(
                    "exec without a command or with options is not supported yet",
                ));
            }
            for name in stage.env.keys() {
                if !crate::common::valid_var_name(&bytes2wcstring(name)) {
                    return Err(rash::Error::new(
                        "an environment name cannot be represented by the Fish launcher",
                    ));
                }
            }
            for redirect in &stage.redirects {
                if redirect.fd < 0
                    || matches!(
                        redirect.action,
                        RedirectAction::File {
                            mode: OpenMode::ReadWrite,
                            ..
                        }
                    )
                {
                    return Err(rash::Error::new(
                        "read-write or invalid descriptor redirection is not supported for external commands yet",
                    ));
                }
            }
        }
        let common: BTreeMap<_, _> = stages[0]
            .env
            .iter()
            .filter(|(name, value)| {
                stages
                    .iter()
                    .all(|stage| stage.env.get(*name) == Some(*value))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut source = WString::new();
        let mut display = WString::new();
        let mut here_files = Vec::new();
        let mut final_job = None;
        let mut wait_handle = None;
        for (index, stage) in stages.iter().enumerate() {
            if index != 0 {
                source.push_utfstr(L!(" | "));
                display.push_utfstr(L!(" | "));
            }
            if let Some(original) = &stage.display {
                display.push_utfstr(&bytes2wcstring(original));
            } else {
                display.push_utfstr(&fish_word(&stage.argv[0])?);
            }
            for (name, value) in &stage.env {
                if common.get(name) != Some(value) {
                    source.push_utfstr(&bytes2wcstring(name));
                    source.push('=');
                    source.push_utfstr(&fish_word(value)?);
                    source.push(' ');
                }
            }
            let is_exec = stage.builtin && stage.argv[0] == b"exec";
            source.push_utfstr(if is_exec {
                L!("exec ")
            } else if stage.builtin {
                L!("builtin ")
            } else {
                L!("command -- ")
            });
            let mut argv = stage.argv.clone();
            if is_exec {
                argv.remove(0);
            } else if stage.builtin {
                for arg in &mut argv[1..] {
                    *arg = job_pid(self.parser, arg)?;
                }
                let pid = argv
                    .get(1)
                    .and_then(|arg| std::str::from_utf8(arg).ok())
                    .and_then(|arg| arg.parse::<i32>().ok())
                    .filter(|pid| *pid > 0)
                    .map(crate::proc::Pid::new);
                if argv[0] == b"fg" {
                    final_job = if let Some(pid) = pid {
                        self.parser.job_get_from_pid(pid)
                    } else {
                        self.parser
                            .jobs()
                            .iter()
                            .find(|job| {
                                !job.is_completed() && (job.is_stopped() || !job.is_foreground())
                            })
                            .cloned()
                    };
                } else if argv[0] == b"wait" && argv.len() > 1 {
                    let last_pid = argv
                        .last()
                        .and_then(|arg| std::str::from_utf8(arg).ok())
                        .and_then(|arg| arg.parse::<i32>().ok())
                        .filter(|pid| *pid > 0)
                        .map(crate::proc::Pid::new);
                    if let Some(pid) = last_pid {
                        wait_handle = self.parser.wait_handles().get_by_pid(pid);
                        if wait_handle.is_none() {
                            for job in self.parser.jobs() {
                                for process in job.processes() {
                                    if process.pid() == Some(pid) {
                                        wait_handle = process.make_wait_handle(job.internal_job_id);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for (i, argument) in argv.iter().enumerate() {
                if i > 0 {
                    source.push(' ');
                }
                source.push_utfstr(&fish_word(argument)?);
            }
            for redirect in &stage.redirects {
                source.push(' ');
                source.push_utfstr(&redirect.fd.to_wstring());
                match &redirect.action {
                    RedirectAction::File { path, mode } => {
                        source.push_utfstr(match mode {
                            OpenMode::Read => L!("<"),
                            OpenMode::Write | OpenMode::Clobber => L!(">"),
                            OpenMode::Append => L!(">>"),
                            OpenMode::ReadWrite => unreachable!(),
                        });
                        source.push_utfstr(&fish_word(path)?);
                    }
                    RedirectAction::Dup(fd) => {
                        if *fd < 0 {
                            return Err(rash::Error::new("invalid duplicated descriptor"));
                        }
                        source.push_utfstr(L!(">&"));
                        source.push_utfstr(&fd.to_wstring());
                    }
                    RedirectAction::Close => source.push_utfstr(L!(">&-")),
                    RedirectAction::Opened(file) => {
                        source.push_utfstr(L!(">&"));
                        source.push_utfstr(&file.as_raw_fd().to_wstring());
                    }
                    RedirectAction::Here(bytes) => {
                        let file = temporary_input(bytes)?;
                        source.push('<');
                        source.push_utfstr(&fish_word(file.path().as_os_str().as_bytes())?);
                        here_files.push(file);
                    }
                }
            }
        }
        if background {
            source.push_utfstr(L!(" &"));
        }
        self.parser.vars().push(false);
        let mut scope =
            fish_common::ScopeGuard::new(&mut *self.parser, |parser| parser.vars().pop(false));
        let parser = &mut **scope;
        for name in parser.vars().get_names(EnvMode::EXPORTED) {
            if parser
                .vars()
                .get(&name)
                .is_some_and(|value| value.is_read_only())
            {
                continue;
            }
            if !common.contains_key(&wcs2bytes(&name)) {
                parser.vars().set_empty(
                    &name,
                    crate::env::EnvSetMode::new(
                        EnvMode {
                            local: true,
                            ..EnvMode::UNEXPORT
                        },
                        false,
                    ),
                );
            }
        }
        for (name, value) in common {
            let name = bytes2wcstring(&name);
            let pathvar = parser
                .vars()
                .get(&name)
                .is_some_and(|value| value.is_pathvar());
            if parser
                .vars()
                .get(&name)
                .is_some_and(|value| value.is_read_only())
            {
                continue;
            }
            parser.vars().set_one(
                &name,
                crate::env::EnvSetMode::new(
                    EnvMode {
                        local: true,
                        pathvar: Some(pathvar),
                        ..EnvMode::EXPORTED
                    },
                    false,
                ),
                bytes2wcstring(&value),
            );
        }
        let buffer = capture.then(crate::io::IoBufferfill::create).transpose()?;
        let mut chain = crate::io::IoChain::new();
        if let Some(buffer) = &buffer {
            chain.push(buffer.clone());
        }
        let old_jobs = parser.jobs().to_vec();
        parser.libdata_mut().bash_job_display = Some((source.clone(), display));
        let result = parser.eval_with(
            &source,
            &chain,
            None,
            if capture {
                crate::parser::BlockType::Subst
            } else {
                crate::parser::BlockType::Top
            },
        );
        parser.libdata_mut().bash_job_display = None;
        let mut statuses = parser.last_statuses();
        if !background {
            if let Some(job) = parser
                .jobs()
                .iter()
                .find(|job| job.is_stopped() && !old_jobs.iter().any(|old| Rc::ptr_eq(job, old)))
            {
                if let Some(stopped_statuses) = job.statuses() {
                    statuses = stopped_statuses;
                }
            }
        }
        if let Some(job) = final_job {
            if result.status.is_success() {
                if let Some(job_statuses) = job.statuses() {
                    statuses = job_statuses;
                }
            }
        }
        if result.status.is_success() {
            if let Some(status) = wait_handle.and_then(|handle| handle.status()) {
                statuses = Statuses::just(status);
            }
        }
        let last_pid = if background {
            parser
                .vars()
                .get(L!("last_pid"))
                .and_then(|v| v.as_string().to_string().parse().ok())
        } else {
            None
        };
        let stdout = buffer
            .map(crate::io::IoBufferfill::finish)
            .map(|buffer| buffer.newline_serialized())
            .unwrap_or_default();
        Ok(RunResult {
            status: statuses.status,
            pipeline: statuses.pipestatus,
            stdout,
            last_pid,
        })
    }

    fn change_dir(&mut self, path: &[u8]) -> rash::Result<Vec<u8>> {
        if path.contains(&0) {
            return Err(rash::Error::new("NUL in directory"));
        }
        let path = bytes2wcstring(path);
        let path =
            crate::path::path_apply_working_directory(&path, &self.parser.vars().get_pwd_slash());
        crate::builtins::cd::try_chdir(self.parser, &path, false).map_err(io::Error::from)?;
        Ok(wcs2bytes(
            &self.parser.vars().get(L!("PWD")).unwrap().as_string(),
        ))
    }

    fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        redirects: &[Redirection],
        capture: bool,
    ) -> rash::Result<Vec<u8>> {
        let targets = redirect_targets(redirects, capture)?;
        let target = targets.get(&fd).cloned().unwrap_or(Target::Fd(fd));
        match target {
            Target::Capture => Ok(bytes.to_vec()),
            Target::Closed if bytes.is_empty() => Ok(Vec::new()),
            Target::Closed => Err(io::Error::from_raw_os_error(libc::EBADF).into()),
            Target::File(file) => {
                (&*file).write_all(bytes)?;
                Ok(Vec::new())
            }
            Target::Fd(fd) => {
                let mut offset = 0;
                while offset < bytes.len() {
                    // The buffer is valid for its length; the OS validates the descriptor.
                    let count = unsafe {
                        libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset)
                    };
                    if count < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::Interrupted && !self.cancelled() {
                            continue;
                        }
                        return Err(error.into());
                    }
                    offset += count as usize;
                }
                Ok(Vec::new())
            }
        }
    }

    fn read_line(
        &mut self,
        redirects: &[Redirection],
        interrupt_for_traps: bool,
    ) -> rash::Result<rash::host::ReadResult> {
        let targets = redirect_targets(redirects, false)?;
        let target = targets.get(&0).cloned().unwrap_or(Target::Fd(0));
        let fd = match &target {
            Target::Fd(fd) => *fd,
            Target::File(file) => file.as_raw_fd(),
            Target::Closed | Target::Capture => {
                return Err(io::Error::from_raw_os_error(libc::EBADF).into());
            }
        };
        let mut line = Vec::new();
        loop {
            if interrupt_for_traps && self.signals.first_pending().is_some() {
                return Ok(rash::host::ReadResult::Interrupted(line));
            }
            if self.cancelled() {
                return Err(rash::Error {
                    message: "read interrupted".to_owned(),
                    status: 130,
                });
            }
            let mut byte = 0u8;
            // The one-byte destination is valid; the OS validates the descriptor.
            let count = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if count == 0 {
                break;
            }
            line.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        Ok(rash::host::ReadResult::Line(line))
    }

    fn cancelled(&self) -> bool {
        crate::signal::signal_check_cancel() != 0 || crate::reader::reader_exit_signal() != 0
    }

    fn set_signal(
        &mut self,
        name: &[u8],
        disposition: rash::host::SignalDisposition,
    ) -> rash::Result<Vec<u8>> {
        self.signals.set(name, disposition)
    }

    fn pending_signals(&mut self) -> Vec<Vec<u8>> {
        self.signals.pending()
    }

    fn push_signal_scope(&mut self) -> rash::Result<()> {
        self.signals.push_scope().map_err(Into::into)
    }

    fn pop_signal_scope(&mut self) -> rash::Result<()> {
        self.signals.pop_scope().map_err(Into::into)
    }

    fn jobs(&self) -> Vec<Vec<u8>> {
        self.parser
            .jobs()
            .iter()
            .filter(|job| job.is_constructed() && !job.is_completed())
            .map(|job| {
                let state = if job.is_stopped() {
                    "Stopped"
                } else {
                    "Running"
                };
                format!("[{}] {} {}", job.job_id(), state, job.command()).into_bytes()
            })
            .collect()
    }
}

#[derive(Clone)]
enum Target {
    Fd(i32),
    File(Arc<File>),
    Capture,
    Closed,
}

fn redirect_targets(
    redirects: &[Redirection],
    capture: bool,
) -> rash::Result<BTreeMap<i32, Target>> {
    let mut targets = BTreeMap::new();
    if capture {
        targets.insert(1, Target::Capture);
    }
    for redirect in redirects {
        if redirect.fd < 0 {
            return Err(rash::Error::new("invalid redirection descriptor"));
        }
        let target = match &redirect.action {
            RedirectAction::Close => Target::Closed,
            RedirectAction::Opened(file) => Target::File(file.clone()),
            RedirectAction::Dup(fd) => {
                let target = targets.get(fd).cloned().unwrap_or(Target::Fd(*fd));
                if let Target::Fd(fd) = target {
                    // fcntl only inspects this descriptor and reports EBADF for invalid input.
                    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                        return Err(io::Error::last_os_error().into());
                    }
                }
                if matches!(target, Target::Closed) {
                    return Err(io::Error::from_raw_os_error(libc::EBADF).into());
                }
                target
            }
            RedirectAction::File { path, mode } => {
                if path.contains(&0) {
                    return Err(rash::Error::new("NUL in redirection path"));
                }
                let mut options = OpenOptions::new();
                match mode {
                    OpenMode::Read => {
                        options.read(true);
                    }
                    OpenMode::Write | OpenMode::Clobber => {
                        options.write(true).create(true).truncate(true);
                    }
                    OpenMode::Append => {
                        options.write(true).create(true).append(true);
                    }
                    OpenMode::ReadWrite => {
                        options.read(true).write(true).create(true);
                    }
                }
                Target::File(Arc::new(options.open(std::ffi::OsStr::from_bytes(path))?))
            }
            RedirectAction::Here(bytes) => {
                let file = temporary_input(bytes)?;
                Target::File(Arc::new(file.get().try_clone()?))
            }
        };
        targets.insert(redirect.fd, target);
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{EnvSetMode, EnvStack};

    #[test]
    fn protocol_input_bounds() {
        assert!(input_bytes(L!("echo ü")).is_ok());
        assert_eq!(
            input_bytes(L!("a\0b")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(input_bytes(&WString::from("ü".repeat(512 * 1024))).is_err());
    }

    #[test]
    #[crate::tests::prelude::serial]
    fn completion_scalars_cross_function_scope_and_restore() {
        crate::tests::prelude::test_init();
        let vars = EnvStack::new();
        let name = L!("bash_completion_scope_test");
        let mode = EnvSetMode::new(EnvMode::GLOBAL, false);
        vars.set(
            name,
            mode,
            vec![L!("fish").to_owned(), L!("list").to_owned()],
        );
        let mut state = State::default();
        state
            .scalars
            .insert(name.to_owned(), (false, L!("bash scalar").to_owned()));
        {
            let _restore = state.scoped_scalars(&vars);
            vars.push(true);
            assert_eq!(vars.get(name).unwrap().as_string(), L!("bash scalar"));
            vars.pop(false);
        }
        assert_eq!(
            vars.get(name).unwrap().as_list(),
            &[L!("fish").to_owned(), L!("list").to_owned()]
        );
        vars.remove(name, mode);
    }
}
