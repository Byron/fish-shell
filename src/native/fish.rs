//! Initial native Fish evaluator using the same AST as the POSIX shell.
//!
//! Unsupported execution features produce errors, never translation into another shell.
use crate::ast::{self, Keyword as _, Token as _};
use crate::parse_constants::{ParseErrorCode, ParseKeyword, ParseTokenType, StatementDecoration};
use crate::redirection::RedirectionMode;
use crate::tokenizer::PipeOrRedir;
use fish_widestring::{WString, wstr};
use rash::host::{Command, Host, OpenMode, RedirectAction, Redirection};
use rash::{Error, Result};
use std::collections::BTreeMap;

#[path = "fish_complete.rs"]
mod complete;
#[path = "fish_expand.rs"]
mod expand;
pub(crate) use expand::unescape_literal;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Variable {
    values: Vec<String>,
    exported: bool,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Flow {
    Normal,
    Break,
    Continue,
    Return,
    Exit,
}

#[derive(Default)]
struct Output {
    redirects: Vec<Redirection>,
    capture: bool,
    bytes: Vec<u8>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    variables: BTreeMap<String, Variable>,
    scopes: Vec<BTreeMap<String, Variable>>,
    functions: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) completions: Vec<super::interactive_completion::CompletionSpec>,
    status: i32,
    pipeline: Vec<i32>,
    last_pid: Option<u32>,
    fish_pid: u32,
    interactive: bool,
    login: bool,
    flow: Flow,
    depth: usize,
    loops: usize,
    function_depth: usize,
    function_frames: Vec<usize>,
    source_depth: usize,
}

fn unsupported(what: &str) -> Error {
    Error::new(format!("native Windows Fish does not yet support {what}"))
}

fn parse(source: &str) -> Result<(WString, ast::Ast)> {
    let source = WString::from(source);
    let mut errors = Vec::new();
    let tree = ast::parse(&source, Default::default(), Some(&mut errors));
    if tree.errored() {
        return Err(Error::new(
            errors
                .iter()
                .map(|error| {
                    if source.is_empty() {
                        error.text.to_string()
                    } else {
                        error.describe(&source, false).to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok((source, tree))
}

pub fn check_syntax(source: &str) -> Result<()> {
    check_syntax_depth(source, 0)
}

fn check_syntax_depth(source: &str, depth: usize) -> Result<()> {
    if depth >= 128 {
        return Err(Error::new("maximum Fish syntax nesting depth exceeded"));
    }
    let (source, tree) = parse(source)?;
    for node in tree.walk() {
        match node.kind() {
            ast::Kind::Argument(arg) => expand::syntax(&text(&source, arg), depth)?,
            ast::Kind::VariableAssignment(arg) => expand::syntax(&text(&source, arg), depth)?,
            ast::Kind::Token(token) if token.token_type() == ParseTokenType::String => {
                if let Some(range) = ast::Token::as_leaf(token).range() {
                    expand::syntax(&source[range.as_usize()].to_string(), depth)?;
                }
            }
            _ => (),
        }
    }
    Ok(())
}

pub fn is_incomplete(source: &str) -> bool {
    let source = WString::from(source);
    let mut errors = Vec::new();
    ast::parse(&source, Default::default(), Some(&mut errors));
    !errors.is_empty()
        && errors.iter().all(|error| {
            matches!(
                error.code,
                ParseErrorCode::TokenizerUnterminatedQuote
                    | ParseErrorCode::TokenizerUnterminatedSubshell
                    | ParseErrorCode::TokenizerUnterminatedSlice
                    | ParseErrorCode::TokenizerUnterminatedEscape
            ) || error.text.to_string().starts_with("Missing end to balance")
                || (error.source_start >= source.len()
                    && error.text.to_string().contains("Expected"))
        })
}

fn text(source: &wstr, node: &impl ast::Node) -> String {
    source[node.source_range().as_usize()].to_string()
}

impl Session {
    pub fn new() -> Result<Self> {
        let mut variables = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            let mut name = name
                .into_string()
                .map_err(|_| Error::new("environment variable name is not Unicode"))?;
            // Windows' hidden per-drive working directories are not shell variables.
            if name.starts_with('=') {
                continue;
            }
            if name.eq_ignore_ascii_case("PATH") {
                name = "PATH".into();
            }
            let value = value
                .into_string()
                .map_err(|_| Error::new(format!("environment variable {name} is not Unicode")))?;
            let values = if name.ends_with("PATH") {
                value.split(';').map(str::to_owned).collect()
            } else {
                vec![value]
            };
            variables.insert(
                name,
                Variable {
                    values,
                    exported: true,
                },
            );
        }
        if !variables.contains_key("HOME") {
            if let Some(profile) = variables.get("USERPROFILE").cloned() {
                variables.insert("HOME".into(), profile);
            }
        }
        variables.insert(
            "PWD".into(),
            Variable {
                values: vec![std::env::current_dir()?.to_string_lossy().into_owned()],
                exported: true,
            },
        );
        Ok(Self {
            variables,
            scopes: vec![BTreeMap::new()],
            functions: BTreeMap::new(),
            completions: Vec::new(),
            status: 0,
            pipeline: vec![0],
            last_pid: None,
            fish_pid: std::process::id(),
            interactive: false,
            login: false,
            flow: Flow::Normal,
            depth: 0,
            loops: 0,
            function_depth: 0,
            function_frames: vec![],
            source_depth: 0,
        })
    }

    pub(crate) fn run_worker(
        (mut session, source, result_path): (Self, String, Option<std::path::PathBuf>),
        host: &mut impl Host,
    ) -> Result<i32> {
        session.flow = Flow::Normal;
        let result = session.eval(&source, host);
        if let Some(path) = result_path {
            // The parent removes the reservation if the pipeline is stopped or
            // cancelled. Do not recreate a stale result after it resumes.
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
            {
                file.write_all(&super::worker::encode(&session)?)?;
            }
        }
        result
    }

    fn exports(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let mut merged = self.variables.clone();
        for scope in &self.scopes[self.scope_start()..] {
            merged.extend(scope.clone());
        }
        merged
            .into_iter()
            .filter(|(_, var)| var.exported)
            .map(|(name, var)| {
                let value = var
                    .values
                    .join(if name.ends_with("PATH") { ";" } else { " " });
                (name.into_bytes(), value.into_bytes())
            })
            .collect()
    }

    pub fn set_mode(&mut self, interactive: bool, login: bool) {
        self.interactive = interactive;
        self.login = login;
    }

    pub fn interrupt(&mut self) {
        self.status = 130;
        self.pipeline = vec![130];
        self.flow = Flow::Normal;
    }

    pub fn completion_context(&self) -> (Vec<String>, Option<std::ffi::OsString>, Vec<String>) {
        (
            self.functions.keys().cloned().collect(),
            Some(self.values("PATH").join(";").into()),
            self.variable_names(),
        )
    }

    fn variable_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> =
            self.variables.keys().cloned().collect();
        for scope in &self.scopes[self.scope_start()..] {
            names.extend(scope.keys().cloned());
        }
        names.extend(["status", "pipestatus", "last_pid", "fish_pid", "argv"].map(str::to_owned));
        names.into_iter().collect()
    }

    pub fn load_config(&mut self, path: &std::path::Path, host: &mut impl Host) -> Result<()> {
        let source = std::fs::read_to_string(path)?;
        self.source_depth += 1;
        let result = self.eval_source(&source, host, &mut Output::default());
        self.source_depth -= 1;
        if self.flow == Flow::Return {
            self.flow = Flow::Normal;
        }
        result
    }

    pub fn prompt(&mut self, host: &mut impl Host) -> Result<Option<String>> {
        if !self.functions.contains_key("fish_prompt") {
            return Ok(None);
        }
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let mut output = Output {
            capture: true,
            ..Default::default()
        };
        let result = self.eval_source("fish_prompt", host, &mut output);
        self.status = status;
        self.pipeline = pipeline;
        result.map(|()| Some(String::from_utf8_lossy(&output.bytes).into_owned()))
    }

    pub fn set_args(&mut self, args: Vec<String>) {
        self.variables.insert(
            "argv".into(),
            Variable {
                values: args,
                exported: false,
            },
        );
    }
    pub fn status(&self) -> i32 {
        self.status
    }
    pub fn exit_requested(&self) -> bool {
        self.flow == Flow::Exit
    }

    pub fn eval(&mut self, source: &str, host: &mut impl Host) -> Result<i32> {
        let result = self.eval_source(source, host, &mut Output::default());
        if let Err(error) = &result {
            self.status = error.status;
        } else if host.cancelled() {
            self.status = 130;
        }
        result.map(|_| self.status)
    }

    fn eval_source(
        &mut self,
        source: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if self.depth >= 128 {
            return Err(Error::new("maximum Fish evaluation depth exceeded"));
        }
        let (source, tree) = parse(source)?;
        // Reject unsupported syntax throughout this input before any command executes.
        for node in tree.walk() {
            match node.kind() {
                ast::Kind::JobPipeline(job) if job.time.is_some() => {
                    return Err(unsupported("the time keyword"));
                }
                ast::Kind::NotStatement(node) if node.time.is_some() => {
                    return Err(unsupported("the time keyword"));
                }
                ast::Kind::DecoratedStatement(node)
                    if node.decoration() == StatementDecoration::Exec =>
                {
                    return Err(unsupported("exec process replacement"));
                }
                ast::Kind::Argument(arg) => expand::validate(&text(&source, arg))?,
                ast::Kind::VariableAssignment(arg) => expand::validate(&text(&source, arg))?,
                ast::Kind::Redirection(redir) => {
                    let raw = WString::from(text(&source, &redir.oper));
                    let parsed = PipeOrRedir::try_from(raw.as_utfstr())
                        .map_err(|_| Error::new("invalid redirection"))?;
                    if !(0..=2).contains(&parsed.fd) {
                        return Err(unsupported("redirection of descriptors above 2"));
                    }
                    if matches!(
                        parsed.mode,
                        RedirectionMode::TryInput | RedirectionMode::NoClob
                    ) {
                        return Err(unsupported("try-input and no-clobber redirections"));
                    }
                }
                ast::Kind::Token(token) if token.token_type() == ParseTokenType::String => {
                    if let Some(range) = ast::Token::as_leaf(token).range() {
                        expand::validate(&source[range.as_usize()].to_string())?;
                    }
                }
                ast::Kind::FunctionHeader(node) if !node.args.is_empty() => {
                    return Err(unsupported("function options"));
                }
                _ => (),
            }
        }
        self.depth += 1;
        let result = self.eval_jobs(tree.top(), &source, host, output);
        self.depth -= 1;
        result
    }

    fn eval_jobs(
        &mut self,
        jobs: &ast::JobList,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        for job in jobs {
            if self.flow != Flow::Normal || host.cancelled() {
                break;
            }
            self.eval_conjunction(job, source, host, output)?;
        }
        Ok(())
    }

    fn eval_conjunction(
        &mut self,
        node: &ast::JobConjunction,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if self.flow != Flow::Normal {
            return Ok(());
        }
        if let Some(decorator) = &node.decorator {
            let keyword = decorator.keyword();
            if (keyword == ParseKeyword::And && self.status != 0)
                || (keyword == ParseKeyword::Or && self.status == 0)
            {
                return Ok(());
            }
        }
        self.eval_job(&node.job, source, host, output)?;
        for continuation in &node.continuations {
            if self.flow != Flow::Normal {
                break;
            }
            let op = continuation.conjunction.token_type();
            if (op == ParseTokenType::AndAnd && self.status == 0)
                || (op == ParseTokenType::OrOr && self.status != 0)
            {
                self.eval_job(&continuation.job, source, host, output)?;
            }
        }
        Ok(())
    }

    fn eval_job(
        &mut self,
        node: &ast::JobPipeline,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if node.bg.is_some() || !node.continuation.is_empty() {
            return self.eval_pipeline(node, source, host, output);
        }
        if node.variables.is_empty() {
            return self.eval_statement(&node.statement, source, host, output);
        }
        let scope = self.assignments(&node.variables, source, host)?;
        self.scopes.push(scope);
        let result = self.eval_statement(&node.statement, source, host, output);
        self.scopes.pop();
        result
    }

    fn eval_pipeline(
        &mut self,
        node: &ast::JobPipeline,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if !host.supports_workers() {
            return Err(unsupported("isolated pipeline processes on this host"));
        }
        let background = node.bg.is_some();
        let result_file = if background {
            None
        } else {
            Some(super::worker::PayloadFile::create()?.0)
        };
        let mut stages = Vec::new();
        let statements = std::iter::once((&node.variables, &node.statement)).chain(
            node.continuation
                .iter()
                .map(|c| (&c.variables, &c.statement)),
        );
        let mut negate = false;
        for (index, (variables, statement)) in statements.enumerate() {
            let mut statement = statement;
            let mut prefix = variables
                .iter()
                .map(|v| text(source, v))
                .collect::<Vec<_>>();
            if index == 0 {
                while let ast::Statement::Not(not) = statement {
                    negate = !negate;
                    prefix.extend(not.variables.iter().map(|v| text(source, v)));
                    statement = &not.contents;
                }
            }
            prefix.push(text(source, statement));
            let stage_source = prefix.join(" ");
            let return_path = (index == node.continuation.len())
                .then(|| result_file.as_ref().map(|file| file.path().to_owned()))
                .flatten();
            let state = super::worker::encode(&(self.clone(), &stage_source, return_path))?;
            let mut stage =
                host.prepare_worker("fish", state, self.exports(), stage_source.into_bytes())?;
            stage.redirects.clone_from(&output.redirects);
            if let Some(continuation) = node.continuation.get(index) {
                let raw = WString::from(text(source, &continuation.pipe));
                let pipe = PipeOrRedir::try_from(raw.as_utfstr())
                    .map_err(|_| Error::new("invalid pipe"))?;
                if pipe.fd != 1 {
                    return Err(unsupported("piping descriptors other than stdout"));
                }
                if pipe.stderr_merge {
                    stage.redirects.push(Redirection {
                        fd: 2,
                        action: RedirectAction::Dup(1),
                    });
                }
            }
            stages.push(stage);
        }
        let result = host.run_pipeline(&stages, background, output.capture)?;
        if let Some(file) = result_file.filter(|_| !host.cancelled()) {
            let bytes = std::fs::read(file.path())?;
            if !bytes.is_empty() {
                let mut updated: Self = super::worker::decode(&bytes)?;
                if updated.values("PWD") != self.values("PWD") {
                    if let Some(cwd) = updated.values("PWD").first() {
                        host.change_dir(cwd.as_bytes())?;
                    }
                }
                // Evaluation depth belongs to the parent invocation, not its worker.
                updated.depth = self.depth;
                *self = updated;
            }
        }
        self.status = if negate {
            i32::from(result.status == 0)
        } else {
            result.status
        };
        self.pipeline = result.pipeline;
        if background {
            self.last_pid = result.last_pid;
        }
        output.bytes.extend(result.stdout);
        Ok(())
    }

    fn assignments(
        &mut self,
        assignments: &ast::VariableAssignmentList,
        source: &wstr,
        host: &mut impl Host,
    ) -> Result<BTreeMap<String, Variable>> {
        self.scopes.push(BTreeMap::new());
        let result = (|| {
            for assignment in assignments {
                let raw = text(source, assignment);
                let (name, value) = raw
                    .split_once('=')
                    .ok_or_else(|| Error::new("invalid variable assignment"))?;
                valid_name(name)?;
                if matches!(
                    name,
                    "status" | "pipestatus" | "last_pid" | "fish_pid" | "PWD"
                ) {
                    return Err(Error::new(format!("{name} is read-only")));
                }
                let values = self.expand(value, host)?;
                self.scopes.last_mut().unwrap().insert(
                    name.to_owned(),
                    Variable {
                        values,
                        exported: true,
                    },
                );
            }
            Ok(())
        })();
        let scope = self.scopes.pop().unwrap();
        result.map(|()| scope)
    }

    fn eval_statement_body(
        &mut self,
        statement: &ast::Statement,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        match statement {
            ast::Statement::Decorated(node) => self.eval_command(node, source, host, output),
            ast::Statement::Not(node) => {
                let scope = self.assignments(&node.variables, source, host)?;
                self.scopes.push(scope);
                let result = self.eval_statement(&node.contents, source, host, output);
                self.scopes.pop();
                result?;
                self.status = i32::from(self.status == 0);
                Ok(())
            }
            ast::Statement::If(node) => {
                self.scopes.push(BTreeMap::new());
                let result = (|| {
                    for clause in std::iter::once(&node.if_clause)
                        .chain(node.elseif_clauses.iter().map(|c| &c.if_clause))
                    {
                        self.eval_conjunction(&clause.condition, source, host, output)?;
                        for tail in &clause.andor_tail {
                            self.eval_conjunction(&tail.job, source, host, output)?;
                        }
                        if self.flow != Flow::Normal {
                            return Ok(());
                        }
                        if self.status == 0 {
                            return self.eval_jobs(&clause.body, source, host, output);
                        }
                    }
                    if let Some(clause) = &node.else_clause {
                        self.eval_jobs(&clause.body, source, host, output)?;
                    } else {
                        self.status = 0;
                    }
                    Ok(())
                })();
                self.scopes.pop();
                result
            }
            ast::Statement::Block(node) => self.eval_block(node, source, host, output),
            ast::Statement::Brace(node) => {
                self.scopes.push(BTreeMap::new());
                let result = self.eval_jobs(&node.jobs, source, host, output);
                self.scopes.pop();
                result
            }
            ast::Statement::Switch(node) => self.eval_switch(node, source, host, output),
        }
    }

    fn eval_block(
        &mut self,
        node: &ast::BlockStatement,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if let ast::BlockStatementHeader::Function(header) = &node.header {
            let name = self.expand_one(&text(source, &header.first_arg), host)?;
            if name.is_empty() || name.starts_with('-') {
                return Err(Error::new("function: invalid name"));
            }
            self.functions.insert(name, text(source, &node.jobs));
            self.status = 0;
            return Ok(());
        }
        self.scopes.push(BTreeMap::new());
        let result = (|| match &node.header {
            ast::BlockStatementHeader::Begin(_) => self.eval_jobs(&node.jobs, source, host, output),
            ast::BlockStatementHeader::For(header) => {
                let name = self.expand_one(&text(source, &header.var_name), host)?;
                valid_name(&name)?;
                let mut values = Vec::new();
                for arg in &header.args {
                    match self.expand(&text(source, arg), host) {
                        Ok(expanded) => values.extend(expanded),
                        Err(error) if error.status == 124 => (),
                        Err(error) => return Err(error),
                    }
                }
                self.loops += 1;
                let result = (|| {
                    for value in values {
                        self.scopes.last_mut().unwrap().insert(
                            name.clone(),
                            Variable {
                                values: vec![value],
                                exported: false,
                            },
                        );
                        self.eval_jobs(&node.jobs, source, host, output)?;
                        if self.flow == Flow::Continue {
                            self.flow = Flow::Normal;
                        } else if self.flow != Flow::Normal || host.cancelled() {
                            break;
                        }
                    }
                    Ok(())
                })();
                self.loops -= 1;
                if self.flow == Flow::Break {
                    self.flow = Flow::Normal;
                }
                result
            }
            ast::BlockStatementHeader::While(header) => {
                self.loops += 1;
                let result = (|| {
                    let mut last_status = 0;
                    loop {
                        self.eval_conjunction(&header.condition, source, host, output)?;
                        for tail in &header.andor_tail {
                            self.eval_conjunction(&tail.job, source, host, output)?;
                        }
                        if self.status != 0 || self.flow != Flow::Normal || host.cancelled() {
                            if self.flow == Flow::Normal {
                                self.status = last_status;
                            }
                            break;
                        }
                        self.eval_jobs(&node.jobs, source, host, output)?;
                        last_status = self.status;
                        if self.flow == Flow::Continue {
                            self.flow = Flow::Normal;
                        } else if self.flow != Flow::Normal {
                            break;
                        }
                    }
                    Ok(())
                })();
                self.loops -= 1;
                if self.flow == Flow::Break {
                    self.flow = Flow::Normal;
                }
                result
            }
            ast::BlockStatementHeader::Function(_) => unreachable!(),
        })();
        self.scopes.pop();
        result
    }

    fn eval_command(
        &mut self,
        node: &ast::DecoratedStatement,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut expanded_command = self.expand(&text(source, &node.command), host)?.into_iter();
        let name = expanded_command
            .next()
            .ok_or_else(|| Error::new("command expanded to an empty list"))?;
        let mut args: Vec<String> = expanded_command.collect();
        let mut redirects = output.redirects.clone();
        for arg in &node.args_or_redirs {
            match arg {
                ast::ArgumentOrRedirection::Argument(arg) => {
                    match self.expand(&text(source, arg), host) {
                        Ok(expanded) => args.extend(expanded),
                        Err(error)
                            if error.status == 124 && matches!(name.as_str(), "set" | "count") => {}
                        Err(error) => return Err(error),
                    }
                }
                ast::ArgumentOrRedirection::Redirection(redir) => {
                    self.append_redirection(redir, source, host, &mut redirects)?;
                }
            }
        }
        let redirects = host.prepare_redirects(&redirects)?;
        let saved = std::mem::replace(&mut output.redirects, redirects);
        let result = self.dispatch(&name, &args, node.decoration(), host, output);
        output.redirects = saved;
        self.pipeline = vec![self.status];
        result
    }

    fn write(&self, bytes: &[u8], host: &mut impl Host, output: &mut Output) -> Result<()> {
        output
            .bytes
            .extend(host.write(1, bytes, &output.redirects, output.capture)?);
        Ok(())
    }

    fn variable(&self, name: &str) -> Option<&Variable> {
        self.scopes[self.scope_start()..]
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .or_else(|| self.variables.get(name))
    }
    fn scope_start(&self) -> usize {
        self.function_frames.last().copied().unwrap_or(0)
    }
    fn values(&self, name: &str) -> Vec<String> {
        match name {
            "status" => vec![self.status.to_string()],
            "pipestatus" => self.pipeline.iter().map(ToString::to_string).collect(),
            "last_pid" => self.last_pid.iter().map(ToString::to_string).collect(),
            "fish_pid" => vec![self.fish_pid.to_string()],
            _ => self
                .variable(name)
                .map(|v| v.values.clone())
                .unwrap_or_default(),
        }
    }
    fn expand_one(&mut self, word: &str, host: &mut impl Host) -> Result<String> {
        let mut values = self.expand(word, host)?;
        if values.len() != 1 {
            return Err(Error::new("expected exactly one expansion result"));
        }
        Ok(values.pop().unwrap())
    }
    fn expand(&mut self, word: &str, host: &mut impl Host) -> Result<Vec<String>> {
        expand::word_with(word, |request| match request {
            expand::Request::Variable(name) => Ok(self.values(name)),
            expand::Request::Command { source, quoted } => {
                let saved_flow = self.flow;
                self.flow = Flow::Normal;
                let mut output = Output {
                    capture: true,
                    ..Default::default()
                };
                let result = self.eval_source(source, host, &mut output);
                self.flow = saved_flow;
                result?;
                expand::split_substitution(&output.bytes, quoted)
            }
        })
    }
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(crate::common::valid_var_name_char) {
        Err(Error::new(format!("invalid variable name: {name}")))
    } else {
        Ok(())
    }
}

#[path = "fish_blocks.rs"]
mod blocks;
#[path = "fish_builtins.rs"]
mod builtins;

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::{ReadResult, RunResult};

    #[derive(Default)]
    struct Capture(Vec<u8>);
    impl Host for Capture {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            Err(Error::new(
                "unexpected external command in Fish semantic test",
            ))
        }
        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            unreachable!()
        }
        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            unreachable!()
        }
        fn write(&mut self, _: i32, bytes: &[u8], _: &[Redirection], _: bool) -> Result<Vec<u8>> {
            self.0.extend(bytes);
            Ok(vec![])
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            vec![]
        }
    }

    fn run(source: &str) -> String {
        let mut capture = Capture::default();
        Session::new().unwrap().eval(source, &mut capture).unwrap();
        String::from_utf8(capture.0).unwrap()
    }

    #[test]
    fn fish_status_and_prefix_assignment_semantics() {
        // Expected results checked against the POSIX Fish evaluator.
        assert_eq!(
            run(
                "false; set native_probe value; echo $status; false; for x in; end; echo $status; false; while false; end; echo $status; set n yes; while test $n = yes; set n no; false; end; echo $status; X=hello Y=$X echo $X $Y; set old before; set -ap old new; echo $old"
            ),
            "1\n1\n0\n1\nhello hello\nnew before new\n"
        );
    }

    #[test]
    fn fish_function_boundaries_and_local_scope_semantics() {
        assert_eq!(
            run(
                "set -g label global; function inner; echo $label; echo $exported_label; set new_binding private; end; function outer; set -l label outer; set -lx exported_label exported; inner; set -q new_binding; echo $status; end; outer; set -l local_only value; begin; set -lq local_only; echo $status; set -gq local_only; echo $status; set -el local_only; end; set -q local_only; echo $status"
            ),
            "global\nexported\n1\n0\n1\n1\n"
        );
    }

    #[test]
    fn syntax_checks_do_not_limit_the_accepted_fish_grammar() {
        for source in [
            "echo (echo hi) | other",
            "echo \"$(echo \"a)\")\"",
            "echo $items[1..-1]",
            "switch value; case '*'; echo yes; end",
        ] {
            check_syntax(source).unwrap();
        }
        for source in [
            "echo $",
            "echo ${bash}",
            "echo (echo $)",
            "if true; echo yes",
        ] {
            assert!(check_syntax(source).is_err(), "{source}");
        }
        for source in ["if true", "echo 'unfinished", "echo |", "{ echo hi"] {
            assert!(is_incomplete(source), "{source}");
        }
        assert!(!is_incomplete("end"));
    }
}
