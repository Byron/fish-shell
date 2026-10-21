// Copyright (C) 1989-2006 Free Software Foundation, Inc.
// Rust adaptation: Copyright (C) 2026 fish-shell contributors.
// Source-guided from GNU Bash 3.2.57 eval.c, execute_cmd.c and variables.c.
// GPL-2.0-or-later; see ../COPYING. No warranty is provided.

//! Persistent Bash execution state embedded in the host shell.
//!
//! The execution order follows GNU Bash 3.2's eval.c/execute_cmd.c. Host calls
//! receive expanded arguments only; parsing and expansion stay in this crate.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Component, PathBuf};

use crate::arithmetic::{self, Variables};
use crate::expand::{self, ExpansionContext, Mode};
use crate::host::{self, Host, OpenMode, RedirectAction, Redirection, RunResult};
use crate::syntax::{self, Command, CommandKind, List, LogicalOp, Pipeline, RedirectKind, Word};
use crate::{Error, Result, os};

pub const BUILTINS: &[&[u8]] = &[
    b":",
    b"true",
    b"false",
    b"echo",
    b"printf",
    b"pwd",
    b"cd",
    b"export",
    b"unset",
    b"set",
    b"shopt",
    b"alias",
    b"unalias",
    b"declare",
    b"typeset",
    b"local",
    b"readonly",
    b"source",
    b".",
    b"eval",
    b"return",
    b"exit",
    b"exec",
    b"break",
    b"continue",
    b"shift",
    b"let",
    b"test",
    b"[",
    b"read",
    b"command",
    b"builtin",
    b"type",
    b"trap",
    b"jobs",
    b"fg",
    b"bg",
    b"wait",
    b"disown",
    b"kill",
];

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Scalar(Vec<u8>),
    Indexed(BTreeMap<i64, Vec<u8>>),
    Associative(BTreeMap<Vec<u8>, Vec<u8>>),
}

impl Value {
    pub fn scalar(&self) -> Option<&[u8]> {
        match self {
            Self::Scalar(value) => Some(value),
            Self::Indexed(values) => values.get(&0).map(Vec::as_slice),
            Self::Associative(values) => values.get(b"0".as_slice()).map(Vec::as_slice),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Variable {
    pub value: Value,
    pub exported: bool,
    pub readonly: bool,
    pub nameref: bool,
}

impl Variable {
    fn scalar(value: Vec<u8>) -> Self {
        Self {
            value: Value::Scalar(value),
            exported: false,
            readonly: false,
            nameref: false,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct Options {
    pub pipefail: bool,
    pub errexit: bool,
    pub nounset: bool,
    pub noglob: bool,
    pub xtrace: bool,
    pub verbose: bool,
    pub allexport: bool,
    pub extglob: bool,
    pub nullglob: bool,
    pub failglob: bool,
    pub nocaseglob: bool,
    pub expand_aliases: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Session {
    pub variables: BTreeMap<Vec<u8>, Variable>,
    pub aliases: BTreeMap<Vec<u8>, Vec<u8>>,
    pub functions: BTreeMap<Vec<u8>, Command>,
    pub cwd: Vec<u8>,
    pub status: i32,
    pub pipeline: Vec<i32>,
    pub positional: Vec<Vec<u8>>,
    pub arg0: Vec<u8>,
    pub last_pid: Option<u32>,
    pub exited: Option<i32>,
    pub options: Options,
    pub interactive: bool,
    pub login: bool,
    traps: BTreeMap<Vec<u8>, Vec<u8>>,
    locals: Vec<BTreeMap<Vec<u8>, Option<Variable>>>,
    startup_ran: bool,
    source_depth: usize,
    function_depth: usize,
    subshell_depth: usize,
}

impl Session {
    pub fn new(
        env: BTreeMap<Vec<u8>, Vec<u8>>,
        cwd: Vec<u8>,
        interactive: bool,
        login: bool,
    ) -> Self {
        let mut variables = env
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    Variable {
                        exported: true,
                        ..Variable::scalar(value)
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        variables.insert(
            b"PWD".to_vec(),
            Variable {
                exported: true,
                ..Variable::scalar(cwd.clone())
            },
        );
        variables.insert(b"IFS".to_vec(), Variable::scalar(b" \t\n".to_vec()));
        variables.insert(
            b"BASH_VERSION".to_vec(),
            Variable::scalar(b"3.2.57-rash".to_vec()),
        );
        variables.insert(b"BASH".to_vec(), Variable::scalar(b"fish".to_vec()));
        Self {
            variables,
            aliases: BTreeMap::new(),
            functions: BTreeMap::new(),
            cwd,
            status: 0,
            pipeline: vec![0],
            positional: vec![],
            arg0: b"fish".to_vec(),
            last_pid: None,
            exited: None,
            options: Options {
                expand_aliases: interactive,
                ..Options::default()
            },
            interactive,
            login,
            traps: BTreeMap::new(),
            locals: vec![],
            startup_ran: false,
            source_depth: 0,
            function_depth: 0,
            subshell_depth: 0,
        }
    }

    pub fn set_export(&mut self, name: Vec<u8>, value: Option<Vec<u8>>) -> Result<()> {
        match value {
            Some(value) => {
                self.set_value(&name, value)?;
                let target = self.resolve_name(&name)?;
                if let Some(variable) = self.variables.get_mut(&target) {
                    variable.exported = true;
                }
            }
            None => {
                if self.variables.get(&name).is_some_and(|v| v.readonly) {
                    return Err(Error::new("cannot unset a readonly Bash export"));
                }
                self.variables.remove(&name);
            }
        }
        Ok(())
    }

    pub fn set_cwd(&mut self, cwd: Vec<u8>) {
        self.cwd.clone_from(&cwd);
        self.variables.insert(
            b"PWD".to_vec(),
            Variable {
                exported: true,
                ..Variable::scalar(cwd)
            },
        );
    }

    pub fn exports(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.variables
            .iter()
            .filter_map(|(name, var)| {
                var.exported
                    .then(|| {
                        var.value
                            .scalar()
                            .map(|value| (name.clone(), value.to_vec()))
                    })
                    .flatten()
            })
            .collect()
    }

    fn resolve_name(&self, name: &[u8]) -> Result<Vec<u8>> {
        let mut name = name.to_vec();
        for _ in 0..64 {
            match self.variables.get(&name) {
                Some(var) if var.nameref => name = var.value.scalar().unwrap_or_default().to_vec(),
                _ => return Ok(name),
            }
        }
        Err(Error::new("circular name reference"))
    }

    pub fn value(&self, name: &[u8]) -> Result<Option<Vec<u8>>> {
        let name = self.resolve_name(name)?;
        if let Some((base, index)) = array_reference(&name) {
            return Ok(self.variables.get(base).and_then(|var| match &var.value {
                Value::Indexed(values) => {
                    if index == b"@" || index == b"*" {
                        Some(join(values.values().cloned(), b" "))
                    } else {
                        parse_number(index)
                            .ok()
                            .and_then(|index| values.get(&index).cloned())
                    }
                }
                Value::Associative(values) => {
                    if index == b"@" || index == b"*" {
                        Some(join(values.values().cloned(), b" "))
                    } else {
                        values.get(index).cloned()
                    }
                }
                Value::Scalar(value) if index == b"0" => Some(value.clone()),
                _ => None,
            }));
        }
        Ok(self
            .variables
            .get(&name)
            .and_then(|v| v.value.scalar().map(<[u8]>::to_vec)))
    }

    pub fn set_value(&mut self, name: &[u8], value: Vec<u8>) -> Result<()> {
        let name = self.resolve_name(name)?;
        let (base, index) =
            array_reference(&name).map_or((name.as_slice(), None), |(a, b)| (a, Some(b)));
        if !valid_name(base) {
            return Err(Error::new(format!("{}: invalid identifier", display(base))));
        }
        let variable = self
            .variables
            .entry(base.to_vec())
            .or_insert_with(|| Variable::scalar(vec![]));
        if variable.readonly {
            return Err(Error::new(format!("{}: readonly variable", display(base))));
        }
        variable.exported |= self.options.allexport;
        match (&mut variable.value, index) {
            (Value::Associative(values), Some(index)) => {
                values.insert(index.to_vec(), value);
            }
            (Value::Indexed(values), index) => {
                values.insert(index.map(parse_number).transpose()?.unwrap_or(0), value);
            }
            (slot @ Value::Scalar(_), Some(index)) => {
                let index = parse_number(index)?;
                let mut values = BTreeMap::new();
                if let Value::Scalar(old) = slot {
                    values.insert(0, std::mem::take(old));
                }
                values.insert(index, value);
                *slot = Value::Indexed(values);
            }
            (Value::Associative(values), None) => {
                values.insert(b"0".to_vec(), value);
            }
            (Value::Scalar(old), None) => *old = value,
        }
        Ok(())
    }

    fn subshell(&self) -> Result<Self> {
        if self.subshell_depth >= 32 {
            return Err(Error::new("maximum subshell depth exceeded"));
        }
        let mut child = self.clone();
        child.subshell_depth += 1;
        Ok(child)
    }

    /// Decode the private snapshot used by native pipeline workers.
    pub fn decode_worker(bytes: &[u8]) -> Result<(Self, Command)> {
        use bincode::Options as _;
        bincode::DefaultOptions::new()
            .with_limit(64 * 1024 * 1024)
            .deserialize(bytes)
            .map_err(|e| Error::new(format!("interpreter snapshot: {e}")))
    }

    pub fn run_worker((mut session, command): (Self, Command), host: &mut dyn Host) -> Result<i32> {
        let mut runner = Runner::new(&mut session, host, false);
        let flow = runner.command(&command)?;
        if let Flow::Return(status) | Flow::Exit(status) = flow {
            runner.set_status(status);
        }
        runner.trap(b"EXIT")?;
        Ok(runner.session.status)
    }

    pub fn check_syntax(&self, source: &[u8]) -> (i32, String) {
        let syntax = if self.options.expand_aliases {
            syntax::check_with_aliases(source, &self.aliases)
        } else {
            syntax::check(source)
        };
        match syntax {
            syntax::SyntaxStatus::Complete => (0, String::new()),
            syntax::SyntaxStatus::Incomplete => (2, "unexpected end of file".into()),
            syntax::SyntaxStatus::Invalid(error) => (1, format!("{error}")),
        }
    }

    pub fn eval(&mut self, source: &[u8], host: &mut dyn Host) -> Result<RunResult> {
        let mut runner = Runner::new(self, host, false);
        let flow = runner.source(source);
        match flow {
            Ok(Flow::Exit(status)) => runner.session.exited = Some(status),
            Ok(Flow::Return(_)) => {
                return Err(Error::new(
                    "return: can only return from a function or sourced script",
                ));
            }
            Ok(Flow::Break(_) | Flow::Continue(_)) => {
                return Err(Error::new("loop control used outside a loop"));
            }
            Ok(Flow::Normal) => {}
            Err(error) => {
                runner.session.status = error.status;
                runner.session.pipeline = vec![error.status];
                return Err(error);
            }
        }
        let result = RunResult {
            status: runner.session.status,
            pipeline: runner.session.pipeline.clone(),
            stdout: std::mem::take(&mut runner.stdout),
            last_pid: runner.session.last_pid,
        };
        if runner.session.exited.is_some() {
            runner.logout()?;
            runner.trap(b"EXIT")?;
        } else if runner.session.interactive {
            runner.prompt_command()?;
        }
        Ok(result)
    }

    pub fn startup(&mut self, host: &mut dyn Host) -> Result<()> {
        self.startup_ran = true;
        let mut runner = Runner::new(self, host, false);
        let home = runner.session.value(b"HOME")?.unwrap_or_default();
        let mut files = vec![];
        let posix = runner.session.value(b"POSIXLY_CORRECT")?.is_some();
        if posix {
            if runner.session.interactive {
                if let Some(path) = runner.session.value(b"ENV")? {
                    if !path.is_empty() {
                        files.push(expand::expand_scalar(&path, &mut runner)?);
                    }
                }
            }
        } else {
            if runner.session.login {
                files.push(b"/etc/profile".to_vec());
                for name in [b".bash_profile".as_slice(), b".bash_login", b".profile"] {
                    let path = path_join(&home, name)?;
                    let file = PathBuf::from(os::from_bytes(&path)?);
                    if file.is_file() && std::fs::File::open(&file).is_ok() {
                        files.push(path);
                        break;
                    }
                }
            } else if runner.session.interactive {
                files.push(path_join(&home, b".bashrc")?);
            }
            if !runner.session.interactive {
                if let Some(path) = runner.session.value(b"BASH_ENV")? {
                    if !path.is_empty() {
                        files.push(expand::expand_scalar(&path, &mut runner)?);
                    }
                }
            }
        }
        for path in files {
            match std::fs::read(os::from_bytes(&path)?) {
                Ok(source) => {
                    runner.session.source_depth += 1;
                    let result = runner.source(&source);
                    runner.session.source_depth -= 1;
                    if let Flow::Exit(status) = result? {
                        runner.session.exited = Some(status);
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if runner.session.exited.is_none() && runner.session.interactive {
            runner.prompt_command()?;
        }
        Ok(())
    }

    pub fn finish(&mut self, host: &mut dyn Host) -> Result<()> {
        if self.exited.is_none() {
            let mut runner = Runner::new(self, host, false);
            runner.session.exited = Some(runner.session.status);
            if runner.session.interactive && !runner.host.cancelled() {
                runner.logout()?;
            }
            runner.trap(b"EXIT")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum Flow {
    Normal,
    Return(i32),
    Exit(i32),
    Break(usize),
    Continue(usize),
}

type CommandWords = (Vec<Vec<u8>>, BTreeMap<Vec<u8>, Vec<u8>>);

struct Runner<'a> {
    session: &'a mut Session,
    host: &'a mut dyn Host,
    capture: bool,
    command_display: Option<Vec<u8>>,
    stdout: Vec<u8>,
    redirects: Vec<Redirection>,
    trap_active: bool,
    suppress_errexit: bool,
    last_substitution_status: Option<i32>,
    depth: usize,
}

impl<'a> Runner<'a> {
    fn new(session: &'a mut Session, host: &'a mut dyn Host, capture: bool) -> Self {
        Self {
            session,
            host,
            capture,
            command_display: None,
            stdout: vec![],
            redirects: vec![],
            trap_active: false,
            suppress_errexit: false,
            last_substitution_status: None,
            depth: 0,
        }
    }

    fn source(&mut self, source: &[u8]) -> Result<Flow> {
        if source.contains(&0) {
            return Err(Error::new("NUL in shell input"));
        }
        self.depth += 1;
        if self.depth > 256 {
            self.depth -= 1;
            return Err(Error::new("maximum evaluation depth exceeded"));
        }
        let result = (|| {
            let mut offset = 0;
            while offset < source.len() {
                if let Some(status) = self.interrupt()? {
                    return Ok(Flow::Exit(status));
                }
                let unit = if self.session.options.expand_aliases {
                    syntax::parse_one_with_aliases(&source[offset..], &self.session.aliases)
                } else {
                    syntax::parse_one(&source[offset..])
                }
                .map_err(|e| Error::new(e.to_string()))?;
                if unit.consumed == 0 {
                    break;
                }
                if self.session.options.verbose {
                    self.emit(2, &source[offset..offset + unit.consumed])?;
                }
                offset += unit.consumed;
                let result = self.list(&unit.body)?;
                if !matches!(result, Flow::Normal) {
                    return Ok(result);
                }
            }
            if let Some(status) = self.interrupt()? {
                return Ok(Flow::Exit(status));
            }
            Ok(Flow::Normal)
        })();
        self.depth -= 1;
        result
    }

    fn interrupt(&mut self) -> Result<Option<i32>> {
        if !self.trap_active {
            for name in self.host.pending_signals() {
                self.trap(&name)?;
                if self.session.exited.is_some() {
                    break;
                }
            }
            if self.session.exited.is_some() {
                return Ok(self.session.exited);
            }
        }
        if self.host.cancelled() {
            Err(Error {
                message: "interrupted".into(),
                status: 130,
            })
        } else {
            Ok(None)
        }
    }

    fn set_status(&mut self, status: i32) {
        self.session.status = status;
        self.session.pipeline = vec![status];
    }

    fn emit(&mut self, fd: i32, bytes: &[u8]) -> Result<()> {
        self.stdout
            .extend(self.host.write(fd, bytes, &self.redirects, self.capture)?);
        Ok(())
    }

    fn trap(&mut self, name: &[u8]) -> Result<()> {
        if self.trap_active {
            return Ok(());
        }
        let Some(source) = self.session.traps.get(name).cloned() else {
            return Ok(());
        };
        let status = self.session.status;
        let pipeline = self.session.pipeline.clone();
        self.trap_active = true;
        let result = self.source(&source);
        self.trap_active = false;
        match result? {
            Flow::Exit(code) => {
                self.session.exited = Some(code);
                self.set_status(code);
            }
            _ => {
                self.session.status = status;
                self.session.pipeline = pipeline;
            }
        }
        Ok(())
    }

    fn logout(&mut self) -> Result<()> {
        if !self.session.startup_ran || !self.session.login {
            return Ok(());
        }
        let status = self.session.status;
        let exiting = self.session.exited.take();
        let result = (|| {
            let home = self.session.value(b"HOME")?.unwrap_or_default();
            match std::fs::read(os::from_bytes(&path_join(&home, b".bash_logout")?)?) {
                Ok(source) => {
                    if let Flow::Exit(code) = self.source(&source)? {
                        self.session.exited = Some(code);
                        self.set_status(code);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })();
        if self.session.exited.is_none() {
            self.session.exited = exiting;
            self.set_status(status);
        }
        result
    }

    fn prompt_command(&mut self) -> Result<()> {
        if let Some(source) = self.session.value(b"PROMPT_COMMAND")? {
            let status = self.session.status;
            let pipeline = self.session.pipeline.clone();
            if let Flow::Exit(code) = self.source(&source)? {
                self.session.exited = Some(code);
            }
            self.session.status = status;
            self.session.pipeline = pipeline;
        }
        Ok(())
    }

    fn list(&mut self, list: &List) -> Result<Flow> {
        for item in list {
            if let Some(status) = self.interrupt()? {
                return Ok(Flow::Exit(status));
            }
            if item.background && !item.chain.rest.is_empty() && self.host.supports_workers() {
                // An asynchronous and/or list is one job; evaluate its conditions
                // inside that job rather than using the launch status in the parent.
                let mut body = item.clone();
                body.background = false;
                let command = Command {
                    kind: CommandKind::Group(vec![body]),
                    redirects: vec![],
                    span: 0..0,
                };
                self.pipeline(
                    &Pipeline {
                        negate: false,
                        commands: vec![command],
                    },
                    true,
                )?;
                continue;
            }
            let old_suppression = self.suppress_errexit;
            self.suppress_errexit |= !item.chain.rest.is_empty() || item.chain.first.negate;
            let mut flow = self.pipeline(&item.chain.first, item.background)?;
            for (index, (op, pipe)) in item.chain.rest.iter().enumerate() {
                if !matches!(flow, Flow::Normal) {
                    break;
                }
                let run = match op {
                    LogicalOp::And => self.session.status == 0,
                    LogicalOp::Or => self.session.status != 0,
                };
                if run {
                    self.suppress_errexit =
                        old_suppression || index + 1 < item.chain.rest.len() || pipe.negate;
                    flow = self.pipeline(pipe, item.background)?;
                }
            }
            let ignored_failure = self.suppress_errexit;
            self.suppress_errexit = old_suppression;
            if !matches!(flow, Flow::Normal) {
                return Ok(flow);
            }
            if self.session.options.errexit && self.session.status != 0 && !ignored_failure {
                return Ok(Flow::Exit(self.session.status));
            }
        }
        Ok(Flow::Normal)
    }

    fn pipeline(&mut self, pipeline: &Pipeline, background: bool) -> Result<Flow> {
        if pipeline.commands.len() == 1 && !background {
            let flow = self.command(&pipeline.commands[0])?;
            if pipeline.negate {
                self.session.status = crate::status::invert_status(self.session.status);
            }
            return Ok(flow);
        }
        let mut stages = vec![];
        if self.host.supports_workers() {
            use bincode::Options as _;
            for command in &pipeline.commands {
                let mut child = self.session.subshell()?;
                child.traps.retain(|_, source| source.is_empty());
                child.interactive = false;
                child.login = false;
                child.exited = None;
                let state = bincode::DefaultOptions::new()
                    .with_limit(64 * 1024 * 1024)
                    .serialize(&(child, command))
                    .map_err(|e| Error::new(format!("interpreter snapshot: {e}")))?;
                let display = match &command.kind {
                    CommandKind::Simple(words) => join(words.iter().map(|w| w.raw.clone()), b" "),
                    _ => b"compound command".to_vec(),
                };
                let mut stage =
                    self.host
                        .prepare_worker("rash", state, self.session.exports(), display)?;
                stage.redirects.clone_from(&self.redirects);
                stages.push(stage);
            }
        } else {
            if pipeline
                .commands
                .iter()
                .any(|command| !matches!(command.kind, CommandKind::Simple(_)))
            {
                return Err(Error::new(
                    "rash: compound commands in pipelines/background jobs are not implemented",
                ));
            }
            for command in &pipeline.commands {
                let mut child = self.session.subshell()?;
                child.traps.retain(|_, source| source.is_empty());
                self.host.push_signal_scope()?;
                let mut runner = Runner::new(&mut child, self.host, self.capture);
                runner.redirects.clone_from(&self.redirects);
                let stage = runner.pipeline_stage(command);
                self.host.pop_signal_scope()?;
                stages.push(stage?);
            }
        }
        let result = self.host.run_pipeline(&stages, background, self.capture)?;
        self.stdout.extend(result.stdout);
        if background {
            self.session.last_pid = result.last_pid;
        }
        self.session.pipeline = result.pipeline;
        let mut status = if self.session.options.pipefail {
            self.session
                .pipeline
                .iter()
                .rev()
                .copied()
                .find(|&s| s != 0)
                .unwrap_or(0)
        } else {
            result.status
        };
        if pipeline.negate {
            status = crate::status::invert_status(status);
        }
        self.session.status = status;
        Ok(Flow::Normal)
    }

    fn pipeline_stage(&mut self, command: &Command) -> Result<host::Command> {
        let CommandKind::Simple(words) = &command.kind else {
            unreachable!()
        };
        let (mut argv, assignments) = self.simple_words(words, false)?;
        if argv.is_empty() {
            return Err(Error::new(
                "rash: assignment-only pipeline stage is not implemented",
            ));
        }
        // shortcut: these builtin stages currently use system utilities; use child
        // Rash execution when compound/stateful pipeline stages are implemented.
        if argv[0] == b":" {
            argv[0] = b"true".to_vec();
        }
        if BUILTINS.contains(&argv[0].as_slice())
            && ![
                b"echo".as_slice(),
                b"printf",
                b"true",
                b"false",
                b"pwd",
                b"test",
                b"[",
            ]
            .contains(&argv[0].as_slice())
        {
            return Err(Error::new(
                "rash: stateful builtin in pipeline/background job is not implemented",
            ));
        }
        if self.session.functions.contains_key(&argv[0]) {
            return Err(Error::new(
                "rash: function pipeline stage is not implemented",
            ));
        }
        let mut env = self.session.exports();
        env.extend(assignments);
        let mut redirects = self.redirects.clone();
        redirects.extend(self.expand_redirects(&command.redirects)?);
        Ok(host::Command {
            display: Some(join(words.iter().map(|word| word.raw.clone()), b" ")),
            argv,
            env,
            redirects,
            builtin: false,
        })
    }

    fn expand(&mut self, word: &Word, mode: Mode) -> Result<Vec<Vec<u8>>> {
        expand::expand_word(&word.raw, self, mode)
    }

    fn one(&mut self, word: &Word) -> Result<Vec<u8>> {
        let words = self.expand(word, Mode::Assignment)?;
        Ok(join(words, b" "))
    }

    fn expand_redirects(&mut self, redirects: &[syntax::Redirect]) -> Result<Vec<Redirection>> {
        let mut result = vec![];
        for redirect in redirects {
            let input = matches!(
                redirect.kind,
                RedirectKind::Read
                    | RedirectKind::DupRead
                    | RedirectKind::ReadWrite
                    | RedirectKind::HereDoc
                    | RedirectKind::HereDocStrip
                    | RedirectKind::HereString
            );
            let fd = redirect.fd.unwrap_or(if input { 0 } else { 1 });
            let action = match redirect.kind {
                RedirectKind::HereDoc | RedirectKind::HereDocStrip => {
                    let doc = redirect
                        .heredoc
                        .as_ref()
                        .ok_or_else(|| Error::new("missing here-document body"))?;
                    let bytes = if doc.expand {
                        let mut quoted = vec![b'"'];
                        for (index, &c) in doc.body.iter().enumerate() {
                            if c == b'"' || c == b'\\' && doc.body.get(index + 1) == Some(&b'"') {
                                quoted.push(b'\\');
                            }
                            quoted.push(c);
                        }
                        quoted.push(b'"');
                        expand::expand_scalar(&quoted, self)?
                    } else {
                        doc.body.clone()
                    };
                    RedirectAction::Here(bytes)
                }
                RedirectKind::HereString => {
                    let mut value = self.one(&redirect.target)?;
                    value.push(b'\n');
                    RedirectAction::Here(value)
                }
                RedirectKind::DupRead | RedirectKind::DupWrite => {
                    let value = self.one(&redirect.target)?;
                    if value == b"-" {
                        RedirectAction::Close
                    } else {
                        RedirectAction::Dup(parse_number(&value)? as i32)
                    }
                }
                _ => {
                    let values = self.expand(&redirect.target, Mode::Redirect)?;
                    if values.len() != 1 {
                        return Err(Error::new("ambiguous redirect"));
                    }
                    let path = path_join(&self.session.cwd, &values[0])?;
                    let mode = match redirect.kind {
                        RedirectKind::Read => OpenMode::Read,
                        RedirectKind::Append => OpenMode::Append,
                        RedirectKind::ReadWrite => OpenMode::ReadWrite,
                        RedirectKind::Clobber => OpenMode::Clobber,
                        _ => OpenMode::Write,
                    };
                    RedirectAction::File { path, mode }
                }
            };
            result.push(Redirection { fd, action });
            if matches!(redirect.kind, RedirectKind::Both) {
                result.push(Redirection {
                    fd: 2,
                    action: RedirectAction::Dup(1),
                });
            }
        }
        Ok(result)
    }

    fn command(&mut self, command: &Command) -> Result<Flow> {
        if let Some(status) = self.interrupt()? {
            return Ok(Flow::Exit(status));
        }
        let saved_redirects = self.redirects.len();
        let redirects = self.expand_redirects(&command.redirects)?;
        let redirects = self.host.prepare_redirects(&redirects)?;
        self.redirects.extend(redirects);
        let display = if let CommandKind::Simple(words) = &command.kind {
            Some(join(words.iter().map(|word| word.raw.clone()), b" "))
        } else {
            None
        };
        let saved_display = std::mem::replace(&mut self.command_display, display);
        let result = self.command_kind(&command.kind);
        self.command_display = saved_display;
        self.redirects.truncate(saved_redirects);
        result
    }

    fn command_kind(&mut self, command: &CommandKind) -> Result<Flow> {
        match command {
            CommandKind::Simple(words) => self.simple(words),
            CommandKind::Group(list) => self.list(list),
            CommandKind::Subshell(list) => {
                let mut child = self.session.subshell()?;
                child.traps.retain(|_, source| source.is_empty());
                let cwd = self.session.cwd.clone();
                self.host.push_signal_scope()?;
                let mut runner = Runner::new(&mut child, self.host, self.capture);
                runner.redirects.clone_from(&self.redirects);
                let flow = runner.list(list);
                self.stdout.extend(runner.stdout);
                self.host.pop_signal_scope()?;
                self.host.change_dir(&cwd)?;
                flow?;
                self.set_status(child.status);
                Ok(Flow::Normal)
            }
            CommandKind::Function { name, body } => {
                self.session
                    .functions
                    .insert(name.raw.clone(), (**body).clone());
                self.set_status(0);
                Ok(Flow::Normal)
            }
            CommandKind::If {
                branches,
                else_body,
            } => {
                for (condition, body) in branches {
                    let old = self.suppress_errexit;
                    self.suppress_errexit = true;
                    let flow = self.list(condition);
                    self.suppress_errexit = old;
                    let flow = flow?;
                    if !matches!(flow, Flow::Normal) {
                        return Ok(flow);
                    }
                    if self.session.status == 0 {
                        return self.list(body);
                    }
                }
                self.set_status(0);
                self.list(else_body)
            }
            CommandKind::For { name, words, body } => {
                let mut values = vec![];
                if let Some(words) = words {
                    for word in words {
                        values.extend(self.expand(word, Mode::Word)?);
                    }
                } else {
                    values.clone_from(&self.session.positional);
                }
                self.set_status(0);
                for value in values {
                    if let Some(status) = self.interrupt()? {
                        return Ok(Flow::Exit(status));
                    }
                    self.session.set_value(&name.raw, value)?;
                    match self.list(body)? {
                        Flow::Break(1) => break,
                        Flow::Continue(1) | Flow::Normal => {}
                        Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                        Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                        flow => return Ok(flow),
                    }
                }
                Ok(Flow::Normal)
            }
            CommandKind::While {
                until,
                condition,
                body,
            } => {
                let mut last = 0;
                loop {
                    if let Some(status) = self.interrupt()? {
                        return Ok(Flow::Exit(status));
                    }
                    let old = self.suppress_errexit;
                    self.suppress_errexit = true;
                    let flow = self.list(condition);
                    self.suppress_errexit = old;
                    if !matches!(flow?, Flow::Normal) {
                        return Err(Error::new("invalid control flow in loop condition"));
                    }
                    if (self.session.status == 0) == *until {
                        break;
                    }
                    match self.list(body)? {
                        Flow::Break(1) => {
                            last = self.session.status;
                            break;
                        }
                        Flow::Continue(1) | Flow::Normal => {}
                        Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                        Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                        flow => return Ok(flow),
                    }
                    last = self.session.status;
                }
                self.set_status(last);
                Ok(Flow::Normal)
            }
            CommandKind::Arithmetic(word) => {
                let expression = expand::expand_scalar(&word.raw, self)?;
                let value = arithmetic::eval(&expression, self)?;
                self.set_status(i32::from(value == 0));
                Ok(Flow::Normal)
            }
            CommandKind::Conditional(words) => {
                let yes = self.conditional(words)?;
                self.set_status(i32::from(!yes));
                Ok(Flow::Normal)
            }
            CommandKind::Case { word, arms } => {
                let value = self.one(word)?;
                for arm in arms {
                    for pattern in &arm.patterns {
                        let pattern = expand::expand_pattern(&pattern.raw, self)?;
                        if expand::pattern_matches_context(&pattern, &value, self)? {
                            return self.list(&arm.body);
                        }
                    }
                }
                self.set_status(0);
                Ok(Flow::Normal)
            }
        }
    }

    fn simple_words(&mut self, words: &[Word], assign: bool) -> Result<CommandWords> {
        let prefix = words
            .iter()
            .take_while(|word| assignment(&word.raw).is_some())
            .count();
        let mut argv: Vec<Vec<u8>> = vec![];
        for word in &words[prefix..] {
            if argv.first().is_some_and(|name| {
                [
                    b"declare".as_slice(),
                    b"typeset",
                    b"export",
                    b"readonly",
                    b"local",
                ]
                .contains(&name.as_slice())
            }) {
                if let Some((name, raw, append)) = assignment(&word.raw) {
                    if raw.starts_with(b"(") && raw.ends_with(b")") {
                        argv.push(word.raw.clone());
                    } else {
                        let value = expand::expand_scalar(raw, self)?;
                        argv.push(
                            [name, if append { b"+=".as_slice() } else { b"=" }, &value].concat(),
                        );
                    }
                    continue;
                }
            }
            argv.extend(self.expand(word, Mode::Word)?);
        }
        // Bash expands command arguments before prefix assignments; assignment RHSs see
        // earlier assignments in the same prefix, without changing the caller's scope.
        let mut saved = BTreeMap::new();
        let result = (|| {
            let mut assignments = BTreeMap::new();
            for word in &words[..prefix] {
                let (name, raw, append) = assignment(&word.raw).unwrap();
                if raw.starts_with(b"(") && raw.ends_with(b")") {
                    if !assign {
                        return Err(Error::new(
                            "array assignment before a command is unsupported",
                        ));
                    }
                    self.assign_array(name, raw, append)?;
                    continue;
                }
                let mut value = join(expand::expand_word(raw, self, Mode::Assignment)?, b" ");
                if append {
                    let mut old = self.get(name)?.unwrap_or_default();
                    old.extend(value);
                    value = old;
                }
                let target = self.variable_name(name)?;
                if !assign {
                    if array_reference(&target).is_some() {
                        return Err(Error::new("array element prefix assignment is unsupported"));
                    }
                    saved
                        .entry(target.clone())
                        .or_insert_with(|| self.session.variables.get(&target).cloned());
                }
                self.session.set_value(&target, value.clone())?;
                assignments.insert(target, value);
            }
            Ok((argv, assignments))
        })();
        for (name, old) in saved {
            if let Some(old) = old {
                self.session.variables.insert(name, old);
            } else {
                self.session.variables.remove(&name);
            }
        }
        result
    }

    fn assign_array(&mut self, name: &[u8], raw: &[u8], append: bool) -> Result<()> {
        let source = [b"__array ".as_slice(), &raw[1..raw.len() - 1]].concat();
        let unit = syntax::parse_one(&source).map_err(|e| Error::new(e.to_string()))?;
        let words = match unit.body.as_slice() {
            [item] if item.chain.rest.is_empty() && item.chain.first.commands.len() == 1 => {
                match &item.chain.first.commands[0].kind {
                    CommandKind::Simple(words) => words[1..].to_vec(),
                    _ => return Err(Error::new("invalid array assignment")),
                }
            }
            _ => return Err(Error::new("invalid array assignment")),
        };
        let associative = self
            .session
            .variables
            .get(name)
            .is_some_and(|v| matches!(v.value, Value::Associative(_)));
        if self.session.variables.get(name).is_some_and(|v| v.readonly) {
            return Err(Error::new("readonly array"));
        }
        let mut values = if append {
            self.session.variables.get(name).map(|v| v.value.clone())
        } else {
            None
        }
        .unwrap_or_else(|| {
            if associative {
                Value::Associative(BTreeMap::new())
            } else {
                Value::Indexed(BTreeMap::new())
            }
        });
        let mut next = match &values {
            Value::Indexed(v) => v.keys().next_back().map_or(Some(0), |n| n.checked_add(1)),
            _ => Some(0),
        };
        for word in words {
            let indexed = word
                .raw
                .starts_with(b"[")
                .then(|| word.raw.windows(2).position(|s| s == b"]="))
                .flatten();
            let (key, raw) = if let Some(close) = indexed {
                (Some(&word.raw[1..close]), &word.raw[close + 2..])
            } else {
                (None, word.raw.as_slice())
            };
            let expanded = expand::expand_word(
                raw,
                self,
                if key.is_some() {
                    Mode::Assignment
                } else {
                    Mode::Word
                },
            )?;
            match &mut values {
                Value::Associative(values) => {
                    let key =
                        key.ok_or_else(|| Error::new("associative array element requires a key"))?;
                    values.insert(expand::expand_scalar(key, self)?, join(expanded, b" "));
                }
                Value::Indexed(values) => {
                    if let Some(key) = key {
                        next = Some(arithmetic::eval(key, self)?);
                    }
                    for value in expanded {
                        let index = next.ok_or_else(|| Error::new("array index overflow"))?;
                        values.insert(index, value);
                        next = index.checked_add(1);
                    }
                }
                Value::Scalar(_) => return Err(Error::new("invalid array append")),
            }
        }
        let var = self
            .session
            .variables
            .entry(name.to_vec())
            .or_insert_with(|| Variable::scalar(vec![]));
        var.value = values;
        Ok(())
    }

    fn simple(&mut self, words: &[Word]) -> Result<Flow> {
        self.trap(b"DEBUG")?;
        if let Some(status) = self.session.exited.filter(|_| !self.trap_active) {
            return Ok(Flow::Exit(status));
        }
        self.last_substitution_status = None;
        let only_assignments = words.iter().all(|word| assignment(&word.raw).is_some());
        let (argv, assignments) = self.simple_words(words, only_assignments)?;
        if argv.is_empty() {
            if !only_assignments {
                for (name, value) in assignments {
                    self.set(&name, value)?;
                }
            }
            if !self.redirects.is_empty() {
                self.emit(1, b"")?;
            }
            self.set_status(self.last_substitution_status.unwrap_or(0));
            return Ok(Flow::Normal);
        }
        if self.session.options.xtrace {
            self.emit(
                2,
                &[b"+ ", join(argv.clone(), b" ").as_slice(), b"\n"].concat(),
            )?;
        }
        let saved = assignments
            .keys()
            .map(|name| (name.clone(), self.session.variables.get(name).cloned()))
            .collect::<Vec<_>>();
        for (name, value) in &assignments {
            self.session.set_value(name, value.clone())?;
            self.session.variables.get_mut(name).unwrap().exported = true;
        }
        let result = self.invoke(argv);
        for (name, old) in saved {
            if let Some(old) = old {
                self.session.variables.insert(name, old);
            } else {
                self.session.variables.remove(&name);
            }
        }
        if self.session.status != 0 && !self.suppress_errexit {
            self.trap(b"ERR")?;
        }
        result
    }

    fn invoke(&mut self, argv: Vec<Vec<u8>>) -> Result<Flow> {
        let name = argv[0].as_slice();
        if let Some(body) = self.session.functions.get(name).cloned() {
            if self.session.function_depth >= 256 {
                return Err(Error::new("maximum function depth exceeded"));
            }
            let args = std::mem::replace(&mut self.session.positional, argv[1..].to_vec());
            self.session.locals.push(BTreeMap::new());
            self.session.function_depth += 1;
            let result = self.command(&body);
            self.session.function_depth -= 1;
            self.session.positional = args;
            for (name, old) in self.session.locals.pop().unwrap() {
                if let Some(old) = old {
                    self.session.variables.insert(name, old);
                } else {
                    self.session.variables.remove(&name);
                }
            }
            self.trap(b"RETURN")?;
            return match result? {
                Flow::Return(status) => {
                    self.set_status(status);
                    Ok(Flow::Normal)
                }
                flow => Ok(flow),
            };
        }
        match self.builtin(&argv) {
            Ok(Some(flow)) => return Ok(flow),
            Ok(None) => {}
            Err(error) if error.status == 130 => return Err(error),
            Err(error) => {
                self.emit(2, format!("rash: {}\n", error.message).as_bytes())?;
                self.set_status(error.status);
                return Ok(Flow::Normal);
            }
        }
        let result = self.host.run_pipeline(
            &[host::Command {
                display: self.command_display.clone(),
                argv,
                env: self.session.exports(),
                redirects: self.redirects.clone(),
                builtin: false,
            }],
            false,
            self.capture,
        )?;
        self.stdout.extend(result.stdout);
        self.session.status = result.status;
        self.session.pipeline = result.pipeline;
        Ok(Flow::Normal)
    }

    fn builtin(&mut self, argv: &[Vec<u8>]) -> Result<Option<Flow>> {
        let args = &argv[1..];
        let mut status = 0;
        match argv[0].as_slice() {
            b":" | b"true" => {}
            b"false" => status = 1,
            b"echo" => {
                let mut i = 0;
                let mut newline = true;
                let mut escapes = false;
                while let Some(arg) = args.get(i).filter(|a| {
                    a.starts_with(b"-") && a.len() > 1 && a[1..].iter().all(|c| b"neE".contains(c))
                }) {
                    for c in &arg[1..] {
                        match c {
                            b'n' => newline = false,
                            b'e' => escapes = true,
                            b'E' => escapes = false,
                            _ => {}
                        }
                    }
                    i += 1;
                }
                let mut text = join(args[i..].iter().cloned(), b" ");
                if escapes {
                    if let Some(decoded) =
                        crate::ansi_c::decode(&cstring(&text)?, crate::ansi_c::ECHO, true, |c| {
                            c.to_ascii_uppercase()
                        })
                    {
                        text = decoded.bytes;
                        newline &= !decoded.saw_c;
                    }
                }
                if newline {
                    text.push(b'\n');
                }
                self.emit(1, &text)?;
            }
            b"printf" => status = self.printf(args)?,
            b"pwd" => self.emit(1, &[self.session.cwd.as_slice(), b"\n"].concat())?,
            b"cd" => {
                let args = args.strip_prefix(&[b"--".to_vec()]).unwrap_or(args);
                if args.len() > 1 {
                    return Err(Error::new("cd: too many arguments"));
                }
                let path = args
                    .first()
                    .cloned()
                    .or(self.session.value(b"HOME")?)
                    .ok_or_else(|| Error::new("cd: HOME not set"))?;
                let path = if path == b"-" {
                    self.session
                        .value(b"OLDPWD")?
                        .ok_or_else(|| Error::new("cd: OLDPWD not set"))?
                } else {
                    path
                };
                let old = self.session.cwd.clone();
                let path = normalize(&path_join(&old, &path)?)?;
                self.host.change_dir(&path)?;
                self.session.set_value(b"OLDPWD", old)?;
                self.session.set_cwd(path);
            }
            b"export" | b"readonly" | b"declare" | b"typeset" | b"local" => {
                status = self.declare(&argv[0], args)?;
            }
            b"unset" => {
                let functions = args.first().is_some_and(|a| a == b"-f");
                for name in args.iter().filter(|a| !a.starts_with(b"-")) {
                    if functions {
                        self.session.functions.remove(name);
                    } else if self.session.variables.get(name).is_some_and(|v| v.readonly) {
                        return Err(Error::new("unset: readonly variable"));
                    } else {
                        self.session.variables.remove(name);
                    }
                }
            }
            b"alias" => {
                if args.is_empty() {
                    for (name, value) in self.session.aliases.clone() {
                        self.emit(
                            1,
                            &[
                                b"alias ".as_slice(),
                                &name,
                                b"=",
                                crate::quote::single_quote(&cstring(&value)?).to_bytes(),
                                b"\n",
                            ]
                            .concat(),
                        )?;
                    }
                }
                for arg in args {
                    if let Some(eq) = arg.iter().position(|&c| c == b'=') {
                        self.session
                            .aliases
                            .insert(arg[..eq].to_vec(), arg[eq + 1..].to_vec());
                    } else if let Some(value) = self.session.aliases.get(arg).cloned() {
                        self.emit(
                            1,
                            &[
                                b"alias ".as_slice(),
                                arg,
                                b"=",
                                crate::quote::single_quote(&cstring(&value)?).to_bytes(),
                                b"\n",
                            ]
                            .concat(),
                        )?;
                    } else {
                        status = 1;
                    }
                }
            }
            b"unalias" => {
                if args.first().is_some_and(|a| a == b"-a") {
                    self.session.aliases.clear();
                } else {
                    for name in args {
                        self.session.aliases.remove(name);
                    }
                }
            }
            b"set" => self.set_options(args)?,
            b"shopt" => {
                status = self.shopt(args)?;
            }
            b"let" => {
                let mut n = 0;
                for arg in args {
                    let expression = expand::expand_scalar(arg, self)?;
                    n = arithmetic::eval(&expression, self)?;
                }
                status = i32::from(n == 0);
            }
            b"shift" => {
                let count = args
                    .first()
                    .map(|a| parse_number(a))
                    .transpose()?
                    .unwrap_or(1);
                if count < 0 || count as usize > self.session.positional.len() {
                    status = 1;
                } else {
                    self.session.positional.drain(..count as usize);
                }
            }
            b"return" => {
                if self.session.function_depth == 0 && self.session.source_depth == 0 {
                    return Err(Error::new("return: not in a function or sourced script"));
                }
                status = args
                    .first()
                    .map(|a| parse_number(a))
                    .transpose()?
                    .map_or(self.session.status, |n| (n & 255) as i32);
                return Ok(Some(Flow::Return(status)));
            }
            b"exit" => {
                status = args
                    .first()
                    .map(|a| parse_number(a))
                    .transpose()?
                    .map_or(self.session.status, |n| (n & 255) as i32);
                self.set_status(status);
                return Ok(Some(Flow::Exit(status)));
            }
            b"exec" => {
                if self.session.subshell_depth > 0 {
                    return Err(Error::new("rash: exec in a subshell is not implemented"));
                }
                if args.is_empty() {
                    return Err(Error::new(
                        "rash: persistent exec redirections are not implemented",
                    ));
                }
                let result = self.host.run_pipeline(
                    &[host::Command {
                        display: self.command_display.clone(),
                        argv: argv.to_vec(),
                        env: self.session.exports(),
                        redirects: self.redirects.clone(),
                        builtin: true,
                    }],
                    false,
                    self.capture,
                )?;
                self.stdout.extend(result.stdout);
                self.set_status(result.status);
                return Ok(Some(Flow::Normal));
            }
            b"break" | b"continue" => {
                let n = args
                    .first()
                    .map(|a| parse_number(a))
                    .transpose()?
                    .unwrap_or(1)
                    .max(1) as usize;
                self.set_status(0);
                return Ok(Some(if argv[0] == b"break" {
                    Flow::Break(n)
                } else {
                    Flow::Continue(n)
                }));
            }
            b"source" | b"." => {
                let file = args
                    .first()
                    .ok_or_else(|| Error::new("source: filename required"))?;
                let path = self.source_path(file)?;
                let source = std::fs::read(os::from_bytes(&path)?)?;
                let old = if args.len() > 1 {
                    Some(std::mem::replace(
                        &mut self.session.positional,
                        args[1..].to_vec(),
                    ))
                } else {
                    None
                };
                self.session.source_depth += 1;
                let result = self.source(&source);
                self.session.source_depth -= 1;
                if let Some(old) = old {
                    self.session.positional = old;
                }
                self.trap(b"RETURN")?;
                return match result? {
                    Flow::Return(code) => {
                        self.set_status(code);
                        Ok(Some(Flow::Normal))
                    }
                    flow => Ok(Some(flow)),
                };
            }
            b"eval" => return self.source(&join(args.iter().cloned(), b" ")).map(Some),
            b"test" | b"[" => {
                let args = if argv[0] == b"[" {
                    if args.last().is_none_or(|a| a != b"]") {
                        return Err(Error::new("[: missing ]"));
                    }
                    &args[..args.len() - 1]
                } else {
                    args
                };
                status = i32::from(!self.test(args, false)?);
            }
            b"command" | b"builtin" => {
                let mut index = 0;
                if args.first().is_some_and(|a| a == b"-v" || a == b"-V") {
                    for name in &args[1..] {
                        let value = if BUILTINS.contains(&name.as_slice())
                            || self.session.functions.contains_key(name)
                        {
                            Some(name.clone())
                        } else {
                            self.find_command(name)
                        };
                        if let Some(value) = value {
                            self.emit(1, &[value.as_slice(), b"\n"].concat())?;
                        } else {
                            status = 1;
                        }
                    }
                    self.set_status(status);
                    return Ok(Some(Flow::Normal));
                }
                if args.first().is_some_and(|a| a == b"--" || a == b"-p") {
                    index += 1;
                }
                if index == args.len() {
                    self.set_status(0);
                    return Ok(Some(Flow::Normal));
                }
                if let Some(flow) = self.builtin(&args[index..])? {
                    return Ok(Some(flow));
                }
                if argv[0] == b"builtin" {
                    return Err(Error::new("builtin: not a shell builtin"));
                }
                let result = self.host.run_pipeline(
                    &[host::Command {
                        display: self.command_display.clone(),
                        argv: args[index..].to_vec(),
                        env: self.session.exports(),
                        redirects: self.redirects.clone(),
                        builtin: false,
                    }],
                    false,
                    self.capture,
                )?;
                self.stdout.extend(result.stdout);
                self.session.pipeline = result.pipeline;
                self.session.status = result.status;
                return Ok(Some(Flow::Normal));
            }
            b"type" => {
                for name in args.iter().filter(|arg| !arg.starts_with(b"-")) {
                    let text = if self.session.functions.contains_key(name) {
                        [name.as_slice(), b" is a function\n"].concat()
                    } else if BUILTINS.contains(&name.as_slice()) {
                        [name.as_slice(), b" is a shell builtin\n"].concat()
                    } else if let Some(path) = self.find_command(name) {
                        [name.as_slice(), b" is ", &path, b"\n"].concat()
                    } else {
                        status = 1;
                        continue;
                    };
                    self.emit(1, &text)?;
                }
            }
            b"read" => {
                let mut raw = false;
                let mut names = vec![];
                for arg in args {
                    if arg == b"-r" {
                        raw = true;
                    } else if arg.starts_with(b"-") {
                        return Err(Error::new("read: unsupported option"));
                    } else {
                        names.push(arg.clone());
                    }
                }
                if names.is_empty() {
                    names.push(b"REPLY".to_vec());
                }
                let mut line = vec![];
                loop {
                    match self.host.read_line(&self.redirects, !self.trap_active)? {
                        host::ReadResult::Line(bytes) => {
                            line.extend(bytes);
                            break;
                        }
                        host::ReadResult::Interrupted(bytes) => {
                            line.extend(bytes);
                            if let Some(status) = self.interrupt()? {
                                return Ok(Some(Flow::Exit(status)));
                            }
                        }
                    }
                }
                status = i32::from(!line.ends_with(b"\n"));
                if line.ends_with(b"\n") {
                    line.pop();
                }
                if !raw {
                    let mut out = vec![];
                    let mut escaped = false;
                    for c in line {
                        if c == b'\\' && !escaped {
                            escaped = true;
                            continue;
                        }
                        out.push(c);
                        escaped = false;
                    }
                    line = out;
                }
                let fields = if names.len() == 1 {
                    vec![line]
                } else {
                    line.splitn(names.len(), |c| b" \t".contains(c))
                        .map(<[u8]>::to_vec)
                        .collect()
                };
                for (i, name) in names.iter().enumerate() {
                    self.session
                        .set_value(name, fields.get(i).cloned().unwrap_or_default())?;
                }
            }
            b"trap" => {
                let args = if args.first().is_some_and(|arg| arg == b"--") {
                    &args[1..]
                } else {
                    args
                };
                if args.is_empty() || args[0] == b"-p" {
                    for (name, source) in self.session.traps.clone() {
                        self.emit(
                            1,
                            &[
                                b"trap -- ",
                                crate::quote::single_quote(&cstring(&source)?).to_bytes(),
                                b" ",
                                &name,
                                b"\n",
                            ]
                            .concat(),
                        )?;
                    }
                } else {
                    if args.len() < 2 {
                        return Err(Error::new("trap: expected a handler and signal"));
                    }
                    for name in &args[1..] {
                        let name = name.strip_prefix(b"SIG").unwrap_or(name);
                        let name = if name == b"0" { b"EXIT" } else { name };
                        let name =
                            if [b"EXIT".as_slice(), b"DEBUG", b"ERR", b"RETURN"].contains(&name) {
                                name.to_vec()
                            } else {
                                self.host.set_signal(
                                    name,
                                    if args[0] == b"-" {
                                        host::SignalDisposition::Default
                                    } else if args[0].is_empty() {
                                        host::SignalDisposition::Ignore
                                    } else {
                                        host::SignalDisposition::Trap
                                    },
                                )?
                            };
                        if args[0] == b"-" {
                            self.session.traps.remove(&name);
                        } else {
                            self.session.traps.insert(name, args[0].clone());
                        }
                    }
                }
            }
            b"jobs" | b"fg" | b"bg" | b"wait" | b"disown" | b"kill" => {
                let result = self.host.run_pipeline(
                    &[host::Command {
                        display: self.command_display.clone(),
                        argv: argv.to_vec(),
                        env: self.session.exports(),
                        redirects: self.redirects.clone(),
                        builtin: true,
                    }],
                    false,
                    self.capture,
                )?;
                self.stdout.extend(result.stdout);
                self.session.status = result.status;
                self.session.pipeline = result.pipeline;
                return Ok(Some(Flow::Normal));
            }
            _ => return Ok(None),
        }
        self.set_status(status);
        Ok(Some(Flow::Normal))
    }

    fn source_path(&self, file: &[u8]) -> Result<Vec<u8>> {
        if !has_path_separator(file) {
            if let Some(path) = self.find_command(file) {
                return Ok(path);
            }
        }
        path_join(&self.session.cwd, file)
    }

    fn find_command(&self, name: &[u8]) -> Option<Vec<u8>> {
        let path = self
            .session
            .value(b"PATH")
            .ok()
            .flatten()
            .unwrap_or_default();
        let paths = if has_path_separator(name) {
            vec![path_join(&self.session.cwd, name).ok()?]
        } else {
            std::env::split_paths(&os::from_bytes(&path).ok()?)
                .filter_map(|p| {
                    let base =
                        path_join(&self.session.cwd, &os::to_bytes(p.as_os_str()).ok()?).ok()?;
                    path_join(&base, name).ok()
                })
                .collect()
        };
        paths.into_iter().find_map(|p| {
            let path = PathBuf::from(os::from_bytes(&p).ok()?);
            if path.is_file() {
                return Some(p);
            }
            // std::process::Command also searches for an .exe suffix on Windows.
            #[cfg(windows)]
            if path.extension().is_none() {
                let executable = path.with_extension("exe");
                if executable.is_file() {
                    return os::to_bytes(executable.as_os_str()).ok();
                }
            }
            None
        })
    }

    fn conditional(&mut self, words: &[Word]) -> Result<bool> {
        if words.is_empty() {
            return Err(Error::new("empty conditional expression"));
        }
        // Split at the least binding operator, outside parentheses. Expansion happens
        // only after short-circuit selection, so skipped commands have no side effects.
        for op in [b"||".as_slice(), b"&&"] {
            let mut depth = 0;
            for (i, word) in words.iter().enumerate() {
                match word.raw.as_slice() {
                    b"(" => depth += 1,
                    b")" => depth -= 1,
                    _ => {}
                }
                if depth == 0 && word.raw == op {
                    let left = self.conditional(&words[..i])?;
                    return if op == b"||" {
                        Ok(left || self.conditional(&words[i + 1..])?)
                    } else {
                        Ok(left && self.conditional(&words[i + 1..])?)
                    };
                }
            }
        }
        if words[0].raw == b"!" {
            return self.conditional(&words[1..]).map(|v| !v);
        }
        if words[0].raw == b"(" && words.last().unwrap().raw == b")" {
            return self.conditional(&words[1..words.len() - 1]);
        }
        let mut args = vec![];
        for (i, word) in words.iter().enumerate() {
            let pattern = i == 2 && matches!(words[1].raw.as_slice(), b"=" | b"==" | b"!=");
            let arithmetic = words.len() == 3
                && matches!(
                    words[1].raw.as_slice(),
                    b"-eq" | b"-ne" | b"-lt" | b"-le" | b"-gt" | b"-ge"
                );
            let value = if pattern {
                expand::expand_pattern(&word.raw, self)?
            } else {
                self.one(word)?
            };
            args.push(if arithmetic && i != 1 {
                arithmetic::eval(&value, self)?.to_string().into_bytes()
            } else {
                value
            });
        }
        self.test(&args, true)
    }

    fn test(&mut self, args: &[Vec<u8>], conditional: bool) -> Result<bool> {
        if args.is_empty() {
            return Ok(false);
        }
        if args[0] == b"!" {
            return self.test(&args[1..], conditional).map(|v| !v);
        }
        if let Some(i) = args.iter().position(|a| a == b"||" || a == b"-o") {
            if i > 0 {
                return Ok(self.test(&args[..i], conditional)?
                    || self.test(&args[i + 1..], conditional)?);
            }
        }
        if let Some(i) = args.iter().position(|a| a == b"&&" || a == b"-a") {
            if i > 0 {
                return Ok(self.test(&args[..i], conditional)?
                    && self.test(&args[i + 1..], conditional)?);
            }
        }
        match args {
            [value] => Ok(!value.is_empty()),
            [op, value] => {
                // String predicates do not need an OS representation of the value.
                if op == b"-n" {
                    return Ok(!value.is_empty());
                }
                if op == b"-z" {
                    return Ok(value.is_empty());
                }
                let path = PathBuf::from(os::from_bytes(&path_join(&self.session.cwd, value)?)?);
                Ok(match op.as_slice() {
                    b"-e" => path.exists(),
                    b"-f" => path.is_file(),
                    b"-d" => path.is_dir(),
                    b"-L" | b"-h" => path.is_symlink(),
                    b"-s" => path.metadata().is_ok_and(|m| m.len() > 0),
                    _ => return Err(Error::new("unsupported test operator")),
                })
            }
            [left, op, right] => Ok(match op.as_slice() {
                b"=" | b"==" => {
                    if conditional {
                        expand::pattern_matches_context(right, left, self)?
                    } else {
                        left == right
                    }
                }
                b"!=" => {
                    if conditional {
                        !expand::pattern_matches_context(right, left, self)?
                    } else {
                        left != right
                    }
                }
                b"<" => left < right,
                b">" => left > right,
                b"-eq" => parse_number(left)? == parse_number(right)?,
                b"-ne" => parse_number(left)? != parse_number(right)?,
                b"-lt" => parse_number(left)? < parse_number(right)?,
                b"-le" => parse_number(left)? <= parse_number(right)?,
                b"-gt" => parse_number(left)? > parse_number(right)?,
                b"-ge" => parse_number(left)? >= parse_number(right)?,
                _ => return Err(Error::new("unsupported test operator")),
            }),
            _ => Err(Error::new("test: invalid expression")),
        }
    }

    fn set_options(&mut self, args: &[Vec<u8>]) -> Result<()> {
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == b"--" {
                self.session.positional = args[i + 1..].to_vec();
                return Ok(());
            }
            if arg == b"-o" || arg == b"+o" {
                i += 1;
                let name = args
                    .get(i)
                    .ok_or_else(|| Error::new("set: option name required"))?;
                self.option(name, arg[0] == b'-')?;
            } else if arg.len() > 1 && matches!(arg[0], b'-' | b'+') {
                for c in &arg[1..] {
                    self.option(
                        match c {
                            b'e' => b"errexit",
                            b'u' => b"nounset",
                            b'f' => b"noglob",
                            b'x' => b"xtrace",
                            b'a' => b"allexport",
                            b'H' => b"histexpand",
                            b'm' => b"monitor",
                            b'v' => b"verbose",
                            _ => return Err(Error::new("set: unsupported option")),
                        },
                        arg[0] == b'-',
                    )?;
                }
            } else {
                self.session.positional = args[i..].to_vec();
                return Ok(());
            }
            i += 1;
        }
        Ok(())
    }

    fn option(&mut self, name: &[u8], enabled: bool) -> Result<()> {
        match name {
            b"pipefail" => self.session.options.pipefail = enabled,
            b"errexit" => self.session.options.errexit = enabled,
            b"nounset" => self.session.options.nounset = enabled,
            b"noglob" => self.session.options.noglob = enabled,
            b"xtrace" => self.session.options.xtrace = enabled,
            b"verbose" => self.session.options.verbose = enabled,
            b"allexport" => self.session.options.allexport = enabled,
            b"histexpand" if !enabled => {}
            b"monitor" if enabled == self.session.interactive => {}
            _ => {
                return Err(Error::new(format!(
                    "set: unsupported option {}",
                    display(name)
                )));
            }
        }
        Ok(())
    }

    fn shopt(&mut self, args: &[Vec<u8>]) -> Result<i32> {
        let mode = args.first().filter(|a| a.starts_with(b"-"));
        let mut status = 0;
        for name in args.iter().skip(usize::from(mode.is_some())) {
            let value = match name.as_slice() {
                b"extglob" => &mut self.session.options.extglob,
                b"nullglob" => &mut self.session.options.nullglob,
                b"failglob" => &mut self.session.options.failglob,
                b"nocaseglob" => &mut self.session.options.nocaseglob,
                b"expand_aliases" => &mut self.session.options.expand_aliases,
                _ => {
                    return Err(Error::new(format!(
                        "shopt: unsupported option {}",
                        display(name)
                    )));
                }
            };
            if mode.is_some_and(|m| m == b"-s") {
                *value = true;
            } else if mode.is_some_and(|m| m == b"-u") {
                *value = false;
            } else {
                status |= i32::from(!*value);
            }
        }
        Ok(status)
    }

    fn declare(&mut self, command: &[u8], args: &[Vec<u8>]) -> Result<i32> {
        let mut export = command == b"export";
        let mut readonly = command == b"readonly";
        let local = command == b"local";
        let mut print = false;
        let mut unexport = false;
        let mut indexed = false;
        let mut associative = false;
        let mut nameref = false;
        let mut integer = false;
        let mut functions = false;
        let mut names_only = false;
        let mut names = vec![];
        for arg in args {
            if arg.starts_with(b"-") {
                for c in &arg[1..] {
                    match c {
                        b'f' => functions = true,
                        b'F' => {
                            functions = true;
                            names_only = true;
                        }
                        b'x' => export = true,
                        b'r' => readonly = true,
                        b'p' => print = true,
                        b'n' if command == b"export" => unexport = true,
                        b'n' => nameref = true,
                        b'a' => indexed = true,
                        b'A' => associative = true,
                        b'i' => integer = true,
                        b'g' | b'-' => {}
                        _ => return Err(Error::new("declare: unsupported option")),
                    }
                }
            } else {
                names.push(arg.clone());
            }
        }
        if functions {
            if local
                || export
                || readonly
                || unexport
                || indexed
                || associative
                || nameref
                || integer
            {
                return Err(Error::new("declare: function attributes are not supported"));
            }
            let all = names.is_empty();
            if all {
                names.extend(self.session.functions.keys().cloned());
            }
            let mut status = 0;
            for name in names {
                let Some(body) = self.session.functions.get(&name).cloned() else {
                    status = 1;
                    continue;
                };
                if names_only {
                    if all {
                        self.emit(1, b"declare -f ")?;
                    }
                    self.emit(1, &[name, b"\n".to_vec()].concat())?;
                } else {
                    self.emit(1, &syntax::format_function(&name, &body))?;
                }
            }
            return Ok(status);
        }
        if print || names.is_empty() {
            let vars = if names.is_empty() {
                self.session.variables.clone()
            } else {
                names
                    .into_iter()
                    .filter_map(|name| {
                        self.session
                            .variables
                            .get(&name)
                            .cloned()
                            .map(|v| (name, v))
                    })
                    .collect()
            };
            for (name, var) in vars {
                if command == b"export" && !var.exported {
                    continue;
                }
                let value = var.value.scalar().unwrap_or_default();
                self.emit(
                    1,
                    &[
                        b"declare ".as_slice(),
                        if var.exported {
                            b"-x ".as_slice()
                        } else {
                            b"-- "
                        },
                        &name,
                        b"=",
                        crate::quote::double_quote(&cstring(value)?).to_bytes(),
                        b"\n",
                    ]
                    .concat(),
                )?;
            }
            return Ok(0);
        }
        if local && self.session.function_depth == 0 {
            return Err(Error::new("local: can only be used in a function"));
        }
        for arg in names {
            let (name, value) = arg
                .iter()
                .position(|&c| c == b'=')
                .map_or((arg.as_slice(), None), |i| {
                    (&arg[..i], Some(arg[i + 1..].to_vec()))
                });
            if !valid_name(name) {
                return Err(Error::new("declare: invalid identifier"));
            }
            if local {
                let old = self.session.variables.get(name).cloned();
                self.session
                    .locals
                    .last_mut()
                    .unwrap()
                    .entry(name.to_vec())
                    .or_insert(old);
            }
            if indexed || associative {
                let v = self
                    .session
                    .variables
                    .entry(name.to_vec())
                    .or_insert_with(|| Variable::scalar(vec![]));
                if v.readonly {
                    return Err(Error::new("readonly variable"));
                }
                v.value = if associative {
                    Value::Associative(BTreeMap::new())
                } else {
                    Value::Indexed(BTreeMap::new())
                };
            }
            if let Some(value) = value {
                if value.starts_with(b"(") && value.ends_with(b")") {
                    self.assign_array(name, &value, false)?;
                } else {
                    let value = if integer {
                        arithmetic::eval(&value, self)?.to_string().into_bytes()
                    } else {
                        value
                    };
                    self.session.set_value(name, value)?;
                }
            }
            let v = self
                .session
                .variables
                .entry(name.to_vec())
                .or_insert_with(|| Variable::scalar(vec![]));
            v.exported = (v.exported || export) && !unexport;
            v.readonly |= readonly;
            v.nameref |= nameref;
        }
        Ok(0)
    }

    fn printf(&mut self, args: &[Vec<u8>]) -> Result<i32> {
        let (target, args) = if args.first().is_some_and(|a| a == b"-v") {
            (
                Some(
                    args.get(1)
                        .ok_or_else(|| Error::new("printf: -v requires a name"))?
                        .clone(),
                ),
                &args[2..],
            )
        } else {
            (None, args)
        };
        let args = args.strip_prefix(&[b"--".to_vec()]).unwrap_or(args);
        let format = args
            .first()
            .ok_or_else(|| Error::new("printf: format required"))?;
        if let Some(target) = &target {
            if !valid_name(target) {
                return Err(Error::new("printf: invalid variable name"));
            }
        }
        let output = crate::builtins_printf::format(format, &args[1..], self)?;
        if let Some(target) = target {
            let mut value = output.value;
            if let Some(nul) = value.iter().position(|&c| c == 0) {
                value.truncate(nul);
            }
            self.session.set_value(&target, value)?;
        } else {
            self.emit(1, &output.bytes)?;
        }
        for diagnostic in output.diagnostics {
            self.emit(2, format!("printf: {diagnostic}\n").as_bytes())?;
        }
        Ok(output.status)
    }
}

impl Runner<'_> {
    fn variable_name(&mut self, name: &[u8]) -> Result<Vec<u8>> {
        let name = self.session.resolve_name(name)?;
        let Some((base, index)) = array_reference(&name) else {
            return Ok(name);
        };
        if index == b"@" || index == b"*" {
            return Ok(name);
        }
        let key = if self
            .session
            .variables
            .get(base)
            .is_some_and(|v| matches!(v.value, Value::Associative(_)))
        {
            expand::expand_scalar(index, self)?
        } else {
            arithmetic::eval(index, self)?.to_string().into_bytes()
        };
        Ok([base, b"[", &key, b"]"].concat())
    }
}

impl Variables for Runner<'_> {
    fn get(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>> {
        let name = self.variable_name(name)?;
        self.session.value(&name)
    }
    fn set(&mut self, name: &[u8], value: Vec<u8>) -> Result<()> {
        let name = self.variable_name(name)?;
        self.session.set_value(&name, value)
    }
}

impl ExpansionContext for Runner<'_> {
    fn interactive(&self) -> bool {
        self.session.interactive
    }

    fn positional(&self) -> Vec<Vec<u8>> {
        self.session.positional.clone()
    }
    fn arg0(&self) -> Vec<u8> {
        self.session.arg0.clone()
    }
    fn status(&self) -> i32 {
        self.session.status
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn last_pid(&self) -> Option<u32> {
        self.session.last_pid
    }
    fn cwd(&self) -> Vec<u8> {
        self.session.cwd.clone()
    }
    fn command_substitute(&mut self, source: &[u8]) -> Result<Vec<u8>> {
        let mut child = self.session.subshell()?;
        child.traps.retain(|_, source| source.is_empty());
        let cwd = self.session.cwd.clone();
        self.host.push_signal_scope()?;
        let mut runner = Runner::new(&mut child, self.host, true);
        let result = runner.source(source);
        let output = runner.stdout;
        self.host.pop_signal_scope()?;
        self.host.change_dir(&cwd)?;
        result?;
        self.last_substitution_status = Some(child.status);
        self.session.status = child.status;
        Ok(output)
    }
    fn option_flags(&self) -> Vec<u8> {
        let mut flags = vec![];
        for (flag, set) in [
            (b'a', self.session.options.allexport),
            (b'e', self.session.options.errexit),
            (b'f', self.session.options.noglob),
            (b'i', self.session.interactive),
            (b'm', self.session.interactive),
            (b'u', self.session.options.nounset),
            (b'v', self.session.options.verbose),
            (b'x', self.session.options.xtrace),
        ] {
            if set {
                flags.push(flag);
            }
        }
        flags
    }
    fn nounset(&self) -> bool {
        self.session.options.nounset
    }
    fn noglob(&self) -> bool {
        self.session.options.noglob
    }
    fn nullglob(&self) -> bool {
        self.session.options.nullglob
    }
    fn failglob(&self) -> bool {
        self.session.options.failglob
    }
    fn extglob(&self) -> bool {
        self.session.options.extglob
    }
    fn array(&mut self, name: &[u8]) -> Result<Option<Vec<Vec<u8>>>> {
        if name == b"PIPESTATUS" {
            return Ok(Some(
                self.session
                    .pipeline
                    .iter()
                    .map(|s| s.to_string().into_bytes())
                    .collect(),
            ));
        }
        Ok(self
            .session
            .variables
            .get(name)
            .map(|variable| match &variable.value {
                Value::Scalar(value) => vec![value.clone()],
                Value::Indexed(values) => values.values().cloned().collect(),
                Value::Associative(values) => values.values().cloned().collect(),
            }))
    }
}

fn cstring(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| Error::new("NUL in shell value"))
}
fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn join(values: impl IntoIterator<Item = Vec<u8>>, sep: &[u8]) -> Vec<u8> {
    let mut result = vec![];
    for (i, value) in values.into_iter().enumerate() {
        if i > 0 {
            result.extend(sep);
        }
        result.extend(value);
    }
    result
}
fn valid_name(name: &[u8]) -> bool {
    name.first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        && name.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}
fn assignment(word: &[u8]) -> Option<(&[u8], &[u8], bool)> {
    let eq = word.iter().position(|&c| c == b'=')?;
    let append = eq > 0 && word[eq - 1] == b'+';
    let name = &word[..eq - usize::from(append)];
    let base = array_reference(name).map_or(name, |(base, _)| base);
    valid_name(base).then_some((name, &word[eq + 1..], append))
}
fn array_reference(name: &[u8]) -> Option<(&[u8], &[u8])> {
    let open = name.iter().position(|&c| c == b'[')?;
    name.ends_with(b"]")
        .then_some((&name[..open], &name[open + 1..name.len() - 1]))
}
fn parse_number(bytes: &[u8]) -> Result<i64> {
    if bytes.is_empty() {
        return Ok(0);
    }
    display(bytes)
        .parse()
        .map_err(|_| Error::new(format!("{}: integer expression expected", display(bytes))))
}
fn has_path_separator(path: &[u8]) -> bool {
    path.contains(&b'/') || cfg!(windows) && path.contains(&b'\\')
}
fn path_join(base: &[u8], path: &[u8]) -> Result<Vec<u8>> {
    os::to_bytes(
        PathBuf::from(os::from_bytes(base)?)
            .join(os::from_bytes(path)?)
            .as_os_str(),
    )
}
fn normalize(path: &[u8]) -> Result<Vec<u8>> {
    let mut result = PathBuf::new();
    for component in PathBuf::from(os::from_bytes(path)?).components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            c => result.push(c),
        }
    }
    os::to_bytes(result.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryHost {
        output: Vec<u8>,
        interrupted: bool,
        pending: Vec<Vec<u8>>,
        signals: BTreeMap<Vec<u8>, host::SignalDisposition>,
        signal_scopes: Vec<BTreeMap<Vec<u8>, host::SignalDisposition>>,
    }
    impl Host for MemoryHost {
        fn run_pipeline(&mut self, _: &[host::Command], _: bool, _: bool) -> Result<RunResult> {
            Err(Error::new("unexpected external execution"))
        }
        fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>> {
            Ok(path.to_vec())
        }
        fn write(
            &mut self,
            _: i32,
            bytes: &[u8],
            _: &[Redirection],
            capture: bool,
        ) -> Result<Vec<u8>> {
            if capture {
                Ok(bytes.to_vec())
            } else {
                self.output.extend(bytes);
                Ok(vec![])
            }
        }
        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<host::ReadResult> {
            Ok(host::ReadResult::Line(b"one two three\n".to_vec()))
        }
        fn cancelled(&self) -> bool {
            self.interrupted
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            vec![]
        }
        fn set_signal(
            &mut self,
            name: &[u8],
            disposition: host::SignalDisposition,
        ) -> Result<Vec<u8>> {
            let name = if name == b"2" {
                b"INT".to_vec()
            } else {
                name.to_vec()
            };
            self.signals.insert(name.clone(), disposition);
            Ok(name)
        }
        fn pending_signals(&mut self) -> Vec<Vec<u8>> {
            std::mem::take(&mut self.pending)
        }
        fn push_signal_scope(&mut self) -> Result<()> {
            self.signal_scopes.push(self.signals.clone());
            self.signals
                .retain(|_, disposition| *disposition == host::SignalDisposition::Ignore);
            Ok(())
        }
        fn pop_signal_scope(&mut self) -> Result<()> {
            self.signals = self.signal_scopes.pop().unwrap();
            Ok(())
        }
    }
    fn session() -> Session {
        Session::new(BTreeMap::new(), b"/".to_vec(), true, false)
    }

    #[test]
    fn signal_traps_are_delivered_at_safe_points_and_restore_status() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(
                b"count=0; trap -- 'seen=$?; ((count+=1))' 2; false",
                &mut host,
            )
            .unwrap();
        host.pending.push(b"INT".to_vec());
        session
            .eval(
                b"printf '%s:%s:%s\\n' \"$seen\" \"$count\" \"$?\"",
                &mut host,
            )
            .unwrap();
        assert_eq!(host.output, b"1:1:1\n");
        session.eval(b"trap '' SIGINT", &mut host).unwrap();
        assert_eq!(
            host.signals[b"INT".as_slice()],
            host::SignalDisposition::Ignore
        );
        session.eval(b"trap - INT", &mut host).unwrap();
        assert_eq!(
            host.signals[b"INT".as_slice()],
            host::SignalDisposition::Default
        );
    }

    #[test]
    fn child_signal_scopes_restore_handlers_even_after_errors() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(
                b"trap 'echo parent' USR1; (trap '' USR1); value=$(trap '' USR1; printf child)",
                &mut host,
            )
            .unwrap();
        assert_eq!(
            host.signals[b"USR1".as_slice()],
            host::SignalDisposition::Trap
        );
        assert!(session.eval(b"(trap '' USR1; ((1/0)))", &mut host).is_err());
        assert_eq!(
            host.signals[b"USR1".as_slice()],
            host::SignalDisposition::Trap
        );
        host.pending.push(b"USR1".to_vec());
        session.eval(b":", &mut host).unwrap();
        assert_eq!(host.output, b"parent\n");
        assert!(host.signal_scopes.is_empty());
    }

    #[test]
    fn exit_in_signal_handler_stops_the_submission_without_an_error() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(b"trap 'echo trapped; exit 17' USR1", &mut host)
            .unwrap();
        host.pending.push(b"USR1".to_vec());
        let result = session.eval(b"echo must-not-run", &mut host).unwrap();
        assert_eq!(result.status, 17);
        assert_eq!(session.exited, Some(17));
        assert_eq!(host.output, b"trapped\n");
    }

    #[test]
    fn persistent_functions_scope_and_substitution_use_no_evaluator_child() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(
                b"value=outer; f() { local value=inner; printf '%s:%s' \"$value\" \"$1\"; }; false",
                &mut host,
            )
            .unwrap();
        assert_eq!(session.status, 1);
        session.eval(b"previous=$?; result=$(f argument); printf '%s|%s|%s\\n' \"$previous\" \"$value\" \"$result\"", &mut host).unwrap();
        assert_eq!(host.output, b"1|outer|inner:argument\n");
        assert_eq!(
            session.value(b"result").unwrap(),
            Some(b"inner:argument".to_vec())
        );
    }

    #[test]
    fn control_flow_and_arithmetic_remain_in_process() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session.eval(b"n=0; for i in 1 2 3; do ((n+=i)); done; if [[ $n -eq 6 ]]; then echo sum:$n; fi; while ((n>3)); do ((n--)); done; case $n in 3) echo three;; *) echo wrong;; esac", &mut host).unwrap();
        assert_eq!(host.output, b"sum:6\nthree\n");
    }

    #[test]
    fn parsing_does_not_execute() {
        let session = session();
        assert_eq!(session.check_syntax(b"echo \"$(touch unexpected)\"").0, 0);
        assert_eq!(session.check_syntax(b"echo \"$(touch unexpected)").0, 2);
    }

    #[test]
    fn aliases_are_expanded_when_each_input_unit_is_parsed() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(
                b"alias begin='if true; then'\nbegin echo alias-grammar; fi\n",
                &mut host,
            )
            .unwrap();
        assert_eq!(host.output, b"alias-grammar\n");
    }

    #[test]
    fn interruption_preserves_state_and_returns_130() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session.eval(b"retained=present", &mut host).unwrap();
        host.interrupted = true;
        assert_eq!(
            session
                .eval(b"while true; do :; done", &mut host)
                .unwrap_err()
                .status,
            130
        );
        assert_eq!(
            session.value(b"retained").unwrap(),
            Some(b"present".to_vec())
        );
        assert_eq!(session.exited, None);
    }

    #[test]
    fn arrays_and_namerefs_retain_values_across_submissions() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session.eval(b"a=(zero 'one two'); declare -A m=([key]='map value'); n=3; declare -n ref=n; ((ref+=4))", &mut host).unwrap();
        session
            .eval(
                b"printf '%s|%s|%s\\n' \"${a[1]}\" \"${m[key]}\" \"$n\"; printf '<%s>' \"${a[@]}\"",
                &mut host,
            )
            .unwrap();
        assert_eq!(host.output, b"one two|map value|7\n<zero><one two>");
    }
    #[test]
    fn expansion_status_and_lazy_conditionals() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session.eval(b"false; printf '%s:%s\\n' \"$(true)\" \"$?\"; x=$(false); printf 'status:%s\\n' \"$?\"", &mut host).unwrap();
        assert_eq!(host.output, b":0\nstatus:1\n");
        session.eval(b"a=0; [[ yes || $(bad) ]] && [[ '' && $(bad) || yes ]]; [[ a++ -eq 0 ]]; a[1+2]=value; echo ${a[3]}", &mut host).unwrap();
        assert!(host.output.ends_with(b"value\n"));
    }

    #[test]
    fn prefix_assignment_order_and_scope() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session.eval(b"a=outer; f() { printf '%s:%s:%s\\n' \"$a\" \"$b\" \"$1\"; }; a=inner b=$a f \"$a\"; echo $a:${b-unset}", &mut host).unwrap();
        assert_eq!(host.output, b"inner:inner:outer\nouter:unset\n");
    }
    #[test]
    fn array_assignment_boundaries_do_not_panic_or_repeat_subscripts() {
        let mut session = session();
        let mut host = MemoryHost::default();
        session
            .eval(
                b"i=0; a[i++]=x; echo $i; a=([9223372036854775807]=last)",
                &mut host,
            )
            .unwrap();
        assert_eq!(host.output, b"1\n");
        assert!(session.eval(b"a+=(overflow)", &mut host).is_err());
        session.set_value(b"scalar", b"retained".to_vec()).unwrap();
        assert!(session.set_value(b"scalar[invalid]", vec![]).is_err());
        assert_eq!(
            session.value(b"scalar").unwrap(),
            Some(b"retained".to_vec())
        );
    }

    #[test]
    fn recursive_substitutions_are_bounded_and_nameref_prefixes_restore() {
        // Match the native runner's stack reservation. Windows' default test
        // thread stack is too small for the evaluator's supported depth limit.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut session = session();
                let mut host = MemoryHost::default();
                session
                    .eval(
                        b"n=old; declare -n ref=n; f() { echo $n:$ref; }; ref=new f; echo $n:$ref",
                        &mut host,
                    )
                    .unwrap();
                assert_eq!(host.output, b"new:new\nold:old\n");
                session
                    .eval(br#"x='echo $(eval "$x")'; eval "$x""#, &mut host)
                    .unwrap();
                assert!(
                    String::from_utf8_lossy(&host.output)
                        .contains("maximum subshell depth exceeded")
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn native_paths_support_source_globs_and_command_lookup() {
        let directory = std::env::temp_dir().join(format!(
            "rash native-é-🐟-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        std::fs::write(directory.join("source.rash"), b"echo sourced").unwrap();
        let executable = directory.join(if cfg!(windows) {
            "portable-probe.exe"
        } else {
            "portable-probe"
        });
        std::fs::write(&executable, b"").unwrap();

        let mut session = session();
        let mut host = MemoryHost::default();
        let paths = std::env::join_paths([directory.join("missing"), directory.clone()]).unwrap();
        session
            .set_value(b"PATH", os::to_bytes(&paths).unwrap())
            .unwrap();
        let mut directory_bytes = os::to_bytes(directory.as_os_str()).unwrap();
        if cfg!(windows) {
            directory_bytes.iter_mut().for_each(|byte| {
                if *byte == b'\\' {
                    *byte = b'/';
                }
            });
        }
        session
            .set_value(b"DIRECTORY", directory_bytes.clone())
            .unwrap();
        session
            .eval(
                br#"source "$DIRECTORY/source.rash"; printf '%s\n' "$DIRECTORY"/*.rash; type portable-probe"#,
                &mut host,
            )
            .unwrap();
        let expected = [
            b"sourced\n".as_slice(),
            &directory_bytes,
            b"/source.rash\nportable-probe is ",
            &os::to_bytes(executable.as_os_str()).unwrap(),
            b"\n",
        ]
        .concat();
        assert_eq!(host.output, expected);
        assert_eq!(session.status, 0);
    }
    #[test]
    fn login_logout_runs_after_exit_without_losing_its_status() {
        let mut session = session();
        let mut host = MemoryHost::default();
        let home = std::env::temp_dir().join(format!(
            "rash-logout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join(".bash_logout"), b"echo logout").unwrap();
        session
            .set_value(b"HOME", os::to_bytes(home.as_os_str()).unwrap())
            .unwrap();
        session.login = true;
        session.startup_ran = true;
        session.eval(b"exit 17", &mut host).unwrap();
        assert_eq!(host.output, b"logout\n");
        assert_eq!((session.exited, session.status), (Some(17), 17));
        std::fs::remove_dir_all(home).unwrap();
    }
}
