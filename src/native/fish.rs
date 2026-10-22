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

#[path = "fish_argparse.rs"]
mod argparse_builtin;
#[path = "fish_autoload.rs"]
mod autoload;
#[path = "fish_complete.rs"]
mod complete;
#[path = "fish_events.rs"]
mod events;
#[path = "fish_expand.rs"]
mod expand;
#[path = "fish_functions.rs"]
mod functions;
#[path = "fish_history.rs"]
mod history_builtin;
#[path = "fish_universal.rs"]
mod universal;
pub(crate) use expand::unescape_literal;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Variable {
    pub(super) values: Vec<String>,
    pub(super) exported: bool,
    #[serde(default)]
    pub(super) pathvar: bool,
}

impl Variable {
    fn exported_value(&self) -> String {
        self.values.join(if self.pathvar { ";" } else { " " })
    }
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
    pipeline_input: bool,
    stdin_is_directly_redirected: bool,
    redirects: Vec<Redirection>,
    capture: bool,
    bytes: Vec<u8>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    #[serde(default)]
    universals: super::universal_variables::Store,
    #[serde(skip)]
    refreshing_universals: bool,
    variables: BTreeMap<String, Variable>,
    scopes: Vec<BTreeMap<String, Variable>>,
    functions: BTreeMap<String, functions::Function>,
    #[serde(default)]
    next_event_id: u64,
    #[serde(with = "autoload::serialization")]
    function_autoload: crate::autoload::Autoload,
    #[serde(default)]
    autoloaded_functions: std::collections::BTreeSet<String>,
    #[serde(default)]
    autoload_tombstones: std::collections::BTreeSet<String>,
    #[serde(default)]
    autoload_paths: Vec<String>,
    #[serde(default)]
    pub(super) completions: Vec<super::interactive_completion::CompletionSpec>,
    /// Retained in worker snapshots for commandline queries in pipeline helpers.
    #[serde(default)]
    completion_line: Option<super::interactive_completion::CompletionContext>,
    #[serde(default)]
    completion_depth: usize,
    #[serde(default)]
    pub(super) abbreviations: super::interactive_abbreviations::Abbreviations,
    #[serde(default)]
    pub(super) bindings: super::interactive_bindings::Bindings,
    #[serde(default)]
    history: Option<super::history::History>,
    #[serde(default)]
    pub(super) private_history: bool,
    status: i32,
    pipeline: Vec<i32>,
    last_pid: Option<u32>,
    fish_pid: u32,
    interactive: bool,
    #[serde(default)]
    pub(super) input_language: super::InputLanguage,
    /// Prompts and command substitutions cannot change the outer reader mode.
    #[serde(default)]
    input_mode_blocked: bool,
    login: bool,
    flow: Flow,
    depth: usize,
    loops: usize,
    function_depth: usize,
    function_frames: Vec<functions::FunctionFrame>,
    source_depth: usize,
    #[serde(default)]
    worker_pipeline_input: bool,
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
                name.clone(),
                Variable {
                    values,
                    exported: true,
                    pathvar: name.ends_with("PATH"),
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
                pathvar: false,
            },
        );
        let mut session = Self {
            universals: Default::default(),
            refreshing_universals: false,
            variables,
            scopes: vec![BTreeMap::new()],
            functions: BTreeMap::new(),
            next_event_id: 0,
            function_autoload: autoload::resolver(),
            autoloaded_functions: Default::default(),
            autoload_tombstones: Default::default(),
            autoload_paths: Vec::new(),
            completions: Vec::new(),
            completion_line: None,
            completion_depth: 0,
            abbreviations: Default::default(),
            bindings: Default::default(),
            history: None,
            private_history: false,
            status: 0,
            pipeline: vec![0],
            last_pid: None,
            fish_pid: std::process::id(),
            interactive: false,
            input_language: super::InputLanguage::Fish,
            input_mode_blocked: false,
            login: false,
            flow: Flow::Normal,
            depth: 0,
            loops: 0,
            function_depth: 0,
            function_frames: vec![],
            source_depth: 0,
            worker_pipeline_input: false,
        };
        session.initialize_function_paths();
        Ok(session)
    }

    pub(crate) fn run_worker(
        (mut session, source, result_path): (Self, String, Option<std::path::PathBuf>),
        host: &mut impl Host,
    ) -> Result<i32> {
        session.flow = Flow::Normal;
        session.input_mode_blocked = true;
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
        let mut merged = self.universals.vars.clone();
        merged.extend(self.variables.clone());
        for scope in &self.scopes[self.scope_start()..] {
            merged.extend(scope.clone());
        }
        merged
            .into_iter()
            .filter(|(_, var)| var.exported)
            .map(|(name, var)| {
                let value = var.exported_value();
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
            self.function_names(),
            Some(self.values("PATH").join(";").into()),
            self.variable_names(),
        )
    }

    pub fn completion_environment(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        self.exports()
            .into_iter()
            .filter_map(|(name, value)| {
                Some((
                    rash::os::from_bytes(&name).ok()?,
                    rash::os::from_bytes(&value).ok()?,
                ))
            })
            .collect()
    }

    fn variable_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = self
            .variables
            .keys()
            .chain(self.universals.vars.keys())
            .cloned()
            .collect();
        for scope in &self.scopes[self.scope_start()..] {
            names.extend(scope.keys().cloned());
        }
        names.extend(["status", "pipestatus", "last_pid", "fish_pid", "argv"].map(str::to_owned));
        names.into_iter().collect()
    }

    pub fn load_config(&mut self, path: &std::path::Path, host: &mut impl Host) -> Result<()> {
        let source = std::fs::read_to_string(path)?;
        self.scopes.push(BTreeMap::from([(
            "argv".into(),
            Variable {
                values: Vec::new(),
                exported: false,
                pathvar: false,
            },
        )]));
        self.source_depth += 1;
        let result = self.eval_source(&source, host, &mut Output::default());
        self.source_depth -= 1;
        self.scopes.pop();
        if self.flow == Flow::Return {
            self.flow = Flow::Normal;
        }
        result
    }

    pub(super) fn editor_variables(&self) -> BTreeMap<String, Vec<String>> {
        self.variable_names()
            .into_iter()
            .filter(|name| name.starts_with("fish_"))
            .map(|name| {
                let values = self.values(&name);
                (name, values)
            })
            .collect()
    }

    pub fn prompt(&mut self, host: &mut impl Host) -> Result<Option<String>> {
        self.refresh_universals(host)?;
        if !self.ensure_function("fish_prompt", host, &mut Output::default())? {
            return Ok(None);
        }
        let status = self.status;
        let pipeline = self.pipeline.clone();
        let mut output = Output {
            capture: true,
            ..Default::default()
        };
        let input_mode_blocked = std::mem::replace(&mut self.input_mode_blocked, true);
        let result = self.eval_source("fish_prompt", host, &mut output);
        self.input_mode_blocked = input_mode_blocked;
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
                pathvar: false,
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
        let mut output = Output {
            pipeline_input: std::mem::take(&mut self.worker_pipeline_input),
            ..Default::default()
        };
        let result = self.eval_source(source, host, &mut output);
        if let Err(error) = &result {
            self.status = error.status;
        } else if host.cancelled() {
            self.status = 130;
        }
        result.map(|_| self.status)
    }

    /// Capture the dialect for the entire submission before any command runs.
    /// A fish_input_mode request takes effect only on the next prompt.
    pub(super) fn eval_input(&mut self, source: &str, host: &mut impl Host) -> Result<i32> {
        if self.input_language == super::InputLanguage::Fish {
            return self.eval(source, host);
        }
        match crate::bash_to_fish::translate(&WString::from(source)) {
            Ok(translated) => self.eval(&translated.to_string(), host),
            Err(error) => {
                self.status = 2;
                self.pipeline = vec![2];
                Err(Error::new(error.message))
            }
        }
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
        self.refresh_universals(host)?;
        if self.flow != Flow::Normal || host.cancelled() {
            return Ok(());
        }
        if node.bg.is_some() || !node.continuation.is_empty() {
            return self.eval_pipeline(node, source, host, output);
        }
        if node.variables.is_empty() {
            return self.eval_statement(&node.statement, source, host, output);
        }
        let scope = self.assignments(&node.variables, source, host)?;
        if self.flow != Flow::Normal || host.cancelled() {
            return Ok(());
        }
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
            let mut worker = self.clone();
            worker.worker_pipeline_input = index > 0;
            let state = super::worker::encode(&(worker, &stage_source, return_path))?;
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
                updated.input_mode_blocked = self.input_mode_blocked;
                updated.refreshing_universals = self.refreshing_universals;
                updated.completion_line = self.completion_line.clone();
                updated.completion_depth = self.completion_depth;
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
        self.refresh_universals(host)
    }

    /// Implicit bindings inherit the visible path metadata, including explicit
    /// --unpath, and default new PATH-suffixed names to native path lists.
    fn binding(&self, name: &str, values: Vec<String>, exported: bool) -> Variable {
        let pathvar = self
            .variable(name)
            .map(|variable| variable.pathvar)
            .unwrap_or_else(|| name.ends_with("PATH"));
        let values = if pathvar {
            values
                .into_iter()
                .flat_map(|value| value.split(';').map(str::to_owned).collect::<Vec<_>>())
                .collect()
        } else {
            values
        };
        Variable {
            values,
            exported,
            pathvar,
        }
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
                let variable = self.binding(name, values, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.to_owned(), variable);
                self.fire_variable_event(name, false, host)?;
                if self.flow != Flow::Normal || host.cancelled() {
                    break;
                }
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
        // Input inherited by a compound statement is not a direct redirection
        // of the individual builtins inside its body.
        if !matches!(
            statement,
            ast::Statement::Decorated(_) | ast::Statement::Not(_)
        ) {
            output.pipeline_input = false;
        }
        match statement {
            ast::Statement::Decorated(node) => self.eval_command(node, source, host, output),
            ast::Statement::Not(node) => {
                let scope = self.assignments(&node.variables, source, host)?;
                if self.flow != Flow::Normal || host.cancelled() {
                    return Ok(());
                }
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
            return self.define_function(header, text(source, &node.jobs), source, host, output);
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
                        let variable = self.binding(&name, vec![value], false);
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(name.clone(), variable);
                        self.fire_variable_event(&name, false, host)?;
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
        let stdin_is_directly_redirected = std::mem::take(&mut output.pipeline_input)
            || redirects[output.redirects.len()..].iter().any(|redirect| {
                redirect.fd == 0 && !matches!(redirect.action, RedirectAction::Close)
            });
        let saved_stdin = std::mem::replace(
            &mut output.stdin_is_directly_redirected,
            stdin_is_directly_redirected,
        );
        let redirects = host.prepare_redirects(&redirects)?;
        let saved = std::mem::replace(&mut output.redirects, redirects);
        let result = self.dispatch(&name, &args, node.decoration(), host, output);
        output.redirects = saved;
        output.stdin_is_directly_redirected = saved_stdin;
        // The set builtin supplies a new status for queries, erasure and
        // listing, but assignment preserves the preceding pipeline verbatim.
        if name != "set" || node.decoration() == StatementDecoration::Command {
            self.pipeline = vec![self.status];
        }
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
            .or_else(|| self.universals.vars.get(name))
    }
    fn scope_start(&self) -> usize {
        self.function_frames
            .last()
            .map_or(0, |frame| frame.visibility_start)
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
            expand::Request::VariableSeparator(name) => Ok(vec![
                if self.variable(name).is_some_and(|variable| variable.pathvar) {
                    ";"
                } else {
                    " "
                }
                .into(),
            ]),
            expand::Request::Command { source, quoted } => {
                let saved_flow = self.flow;
                self.flow = Flow::Normal;
                let mut output = Output {
                    capture: true,
                    ..Default::default()
                };
                let input_mode_blocked = std::mem::replace(&mut self.input_mode_blocked, true);
                // Substitutions share ordinary variable updates with the caller,
                // but explicit local bindings belong only to this substitution.
                self.scopes.push(BTreeMap::new());
                let result = self.eval_source(source, host, &mut output);
                self.scopes.pop();
                self.input_mode_blocked = input_mode_blocked;
                if self.completion_depth == 0 || self.flow != Flow::Exit {
                    self.flow = saved_flow;
                }
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
                self.0.extend(bytes);
                Ok(vec![])
            }
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
    fn argparse_validator_errors_unwind_temporary_scope_and_reader_state() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session
            .eval(
                "set -l argv original; set -l _flag_value caller",
                &mut capture,
            )
            .unwrap();
        session.status = 7;
        session.pipeline = vec![9, 7];
        let scope_count = session.scopes.len();
        let frame_count = session.function_frames.len();
        let args = [
            "x=!set -l transient value; fish_input_mode bash",
            "--",
            "-x",
            "argument",
        ]
        .map(str::to_owned);
        let mut output = Output::default();
        assert!(session.argparse(&args, &mut capture, &mut output).is_err());
        assert_eq!(session.scopes.len(), scope_count);
        assert_eq!(session.function_frames.len(), frame_count);
        assert!(!session.input_mode_blocked);
        assert_eq!(session.status, 7);
        assert_eq!(session.pipeline, [9, 7]);
        assert!(session.flow == Flow::Normal);
        assert_eq!(session.values("argv"), ["original"]);
        assert_eq!(session.values("_flag_value"), ["caller"]);
        assert!(session.values("transient").is_empty());
    }

    #[test]
    fn completion_queries_restore_reader_state_and_preserve_live_mutations() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.set_mode(true, false);
        session.eval("complete -c completion_probe -f -n 'set -g completion_touched yes; true' -a '(echo alpha)'", &mut capture).unwrap();
        session.status = 7;
        session.pipeline = vec![9, 7];
        let mut engine = super::super::interactive_completion::CompletionEngine::new("fish");
        let (_, choices) = session
            .completion_query(
                "completion_probe ",
                17,
                &std::env::current_dir().unwrap(),
                &mut capture,
                &mut engine,
            )
            .unwrap();
        assert_eq!(choices[0].display, "alpha");
        assert_eq!(session.values("completion_touched"), ["yes"]);
        assert_eq!(session.status, 7);
        assert_eq!(session.pipeline, [9, 7]);
        assert!(!session.input_mode_blocked);
        assert!(session.completion_line.is_none());
        assert_eq!(session.completion_depth, 0);
        assert!(session.flow == Flow::Normal);
        assert!(capture.0.is_empty());

        session.completions.clear();
        session
            .eval(
                "complete -c completion_probe -f -a '(fish_input_mode bash)'",
                &mut capture,
            )
            .unwrap();
        session.status = 7;
        session.pipeline = vec![9, 7];
        assert!(
            session
                .completion_query(
                    "completion_probe ",
                    17,
                    &std::env::current_dir().unwrap(),
                    &mut capture,
                    &mut engine
                )
                .is_err()
        );
        assert_eq!(session.status, 7);
        assert_eq!(session.pipeline, [9, 7]);
        assert!(!session.input_mode_blocked);
        assert!(session.completion_line.is_none());
        assert_eq!(session.completion_depth, 0);
        assert!(session.flow == Flow::Normal);
        assert_eq!(session.input_language, super::super::InputLanguage::Fish);
    }

    #[test]
    fn reader_completion_exit_restores_preceding_status_and_keeps_exit_flow() {
        for registration in ["-n 'exit 23' -a alpha", "-a '(exit 24)'"] {
            let mut session = Session::new().unwrap();
            let mut capture = Capture::default();
            session.set_mode(true, false);
            session
                .eval(
                    &format!("complete -c completion_probe -f {registration}"),
                    &mut capture,
                )
                .unwrap();
            session.status = 7;
            session.pipeline = vec![9, 7];
            let mut engine = super::super::interactive_completion::CompletionEngine::new("fish");
            session
                .completion_query(
                    "completion_probe ",
                    17,
                    &std::env::current_dir().unwrap(),
                    &mut capture,
                    &mut engine,
                )
                .unwrap();
            assert!(session.flow == Flow::Exit);
            assert_eq!(session.status, 7);
            assert_eq!(session.pipeline, [9, 7]);
            assert!(!session.input_mode_blocked);
            assert!(session.completion_line.is_none());
            assert_eq!(session.completion_depth, 0);
        }
    }

    #[test]
    fn completion_commandline_cursor_uses_unicode_characters_and_preserves_suffix() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.eval("complete -c completion_probe -f -n 'set -g observed (commandline -opc); set -g token (commandline -ct); true' -a alpha", &mut capture).unwrap();
        let line = "echo ö; completion_probe alZZ";
        let cursor = line.find("ZZ").unwrap();
        let mut engine = super::super::interactive_completion::CompletionEngine::new("fish");
        session
            .completion_query(
                line,
                cursor,
                &std::env::current_dir().unwrap(),
                &mut capture,
                &mut engine,
            )
            .unwrap();
        assert_eq!(session.values("observed"), ["completion_probe"]);
        assert_eq!(session.values("token"), ["al"]);
    }

    #[test]
    fn bash_input_keeps_fish_state_expansion_and_function_semantics() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.set_mode(true, false);
        session.eval("set -g words 'one two' three; function private_fn; echo $words; end; fish_input_mode bash", &mut capture).unwrap();
        session.eval_input("value=hello; private_fn; echo ${value}; echo $words; say() { echo \"$1\"; }; say 'native fish'; echo $(x=nested; echo $x); echo `literal`", &mut capture).unwrap();
        assert_eq!(
            String::from_utf8(capture.0).unwrap(),
            "one two three\nhello\none two three\nnative fish\nnested\n`literal`\n"
        );
        assert_eq!(session.values("value"), ["hello"]);
        assert_eq!(session.values("words"), ["one two", "three"]);
        assert!(session.functions.contains_key("say"));
    }

    #[test]
    fn input_mode_switch_applies_after_the_entire_submission() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.set_mode(true, false);
        session
            .eval_input("fish_input_mode bash; set -g first fish", &mut capture)
            .unwrap();
        assert_eq!(session.input_language, super::super::InputLanguage::Bash);
        session
            .eval_input(
                "fish_input_mode fish; second=bash; echo $first $second",
                &mut capture,
            )
            .unwrap();
        assert_eq!(session.input_language, super::super::InputLanguage::Fish);
        session
            .eval_input("set -g third fish; echo $third", &mut capture)
            .unwrap();
        assert_eq!(String::from_utf8(capture.0).unwrap(), "fish bash\nfish\n");
    }

    #[test]
    fn bash_input_special_variables_and_control_flow_use_fish_values() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.input_language = super::super::InputLanguage::Bash;
        session.set_args(vec!["first".into(), "second word".into()]);
        session.last_pid = Some(42);
        session.eval_input(
            "false; echo $?suffix; echo $!; echo $1; echo $#; printf '<%s>\\n' \"$@\"; x=prefix; echo ${x}suffix; if false; then echo bad; elif true; then echo branch; else echo bad; fi; for item in one two; do echo $item; done; flag=yes; while test \"$flag\" = yes; do flag=no; echo loop; done; until true; do echo bad; done; f() { printf '[%s]\\n' \"$@\"; }; f a 'b c'",
            &mut capture,
        ).unwrap();
        assert_eq!(
            String::from_utf8(capture.0).unwrap(),
            "1suffix\n42\nfirst\n2\n<first>\n<second word>\nprefixsuffix\nbranch\none\ntwo\nloop\n[a]\n[b c]\n"
        );
    }

    #[test]
    fn bash_translation_rejects_whole_submission_before_side_effects() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        session.set_mode(true, false);
        session.input_language = super::super::InputLanguage::Bash;
        for source in [
            "changed=yes; echo ${missing:-fallback}",
            "changed=yes\necho $(echo ${missing:-fallback})",
            "changed=yes; [[ a == a ]]",
            "changed=yes; fish_input_mode fish; unsupported=(one two)",
        ] {
            assert!(
                session.eval_input(source, &mut capture).is_err(),
                "{source}"
            );
            assert!(session.values("changed").is_empty());
            assert_eq!(session.status(), 2);
            assert_eq!(session.input_language, super::super::InputLanguage::Bash);
        }
        assert!(capture.0.is_empty());
    }

    #[test]
    fn prompts_substitutions_and_noninteractive_code_cannot_switch_input() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        assert!(session.eval("fish_input_mode bash", &mut capture).is_err());
        session.set_mode(true, false);
        session
            .eval(
                "function fish_prompt; fish_input_mode bash; end",
                &mut capture,
            )
            .unwrap();
        assert!(session.prompt(&mut capture).is_err());
        assert!(
            session
                .eval("echo (fish_input_mode bash)", &mut capture)
                .is_err()
        );
        assert_eq!(session.input_language, super::super::InputLanguage::Fish);
        // The guard is restored on error so a subsequent outer request works.
        session.eval("fish_input_mode bash", &mut capture).unwrap();
        session.eval("set -g still_fish yes", &mut capture).unwrap();
        assert_eq!(session.values("still_fish"), ["yes"]);
    }

    #[test]
    fn native_windows_status_is_quiet_and_selects_startup_code() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        assert_eq!(
            session
                .eval("false; status is-native-windows", &mut capture)
                .unwrap(),
            0
        );
        assert!(capture.0.is_empty());
        session
            .eval(
                "if status is-native-windows; set -g backend native; else; set -g backend posix; end",
                &mut capture,
            )
            .unwrap();
        assert_eq!(session.values("backend"), ["native"]);
        session.set_mode(true, true);
        session.eval("fish_input_mode bash", &mut capture).unwrap();
        assert_eq!(
            session
                .eval_input("status is-native-windows", &mut capture)
                .unwrap(),
            0
        );
        assert!(capture.0.is_empty());
    }

    #[test]
    fn native_windows_status_rejects_operands() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        let error = session
            .eval("status is-native-windows extra", &mut capture)
            .unwrap_err();
        assert_eq!(error.status, 2);
        assert_eq!(
            error.message,
            "status is-native-windows: expected 0 arguments; got 1"
        );
        assert!(capture.0.is_empty());
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
    fn failed_command_substitutions_unwind_their_local_scope() {
        let mut session = Session::new().unwrap();
        let mut capture = Capture::default();
        assert!(
            session
                .eval(
                    "echo (set -l failed_substitution_local hidden; math 'invalid(')",
                    &mut capture
                )
                .is_err()
        );
        assert!(session.variable("failed_substitution_local").is_none());
        session.eval("echo recovered", &mut capture).unwrap();
        assert_eq!(String::from_utf8(capture.0).unwrap(), "recovered\n");
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
