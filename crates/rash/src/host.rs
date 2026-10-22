//! Services supplied by the embedding shell. Execution requests contain expanded
//! commands or opaque interpreter snapshots for isolated pipeline processes.

use std::collections::BTreeMap;
use std::fs::File;
use std::sync::Arc;

use crate::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenMode {
    Read,
    Write,
    Append,
    Clobber,
    ReadWrite,
}

#[derive(Clone, Debug)]
pub enum RedirectAction {
    File {
        path: Vec<u8>,
        mode: OpenMode,
    },
    Dup(i32),
    Close,
    Here(Vec<u8>),
    /// An opened file description shared by every command in the redirect scope.
    Opened(Arc<File>),
}

#[derive(Clone, Debug)]
pub struct Redirection {
    pub fd: i32,
    pub action: RedirectAction,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub argv: Vec<Vec<u8>>,
    /// Unexpanded command words for job notifications; never reconstruct from values.
    pub display: Option<Vec<u8>>,
    pub env: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redirects: Vec<Redirection>,
    /// Only explicitly selected job-control services use a host builtin.
    pub builtin: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub status: i32,
    pub pipeline: Vec<i32>,
    pub stdout: Vec<u8>,
    pub last_pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDisposition {
    Default,
    Ignore,
    Trap,
}

pub enum ReadResult {
    Line(Vec<u8>),
    /// Return bytes consumed so far; the evaluator runs traps before resuming the read.
    Interrupted(Vec<u8>),
}

pub trait Host {
    /// Hosts with native process isolation can run builtins and compound commands
    /// in pipeline/background processes using this interpreter's own executable.
    fn supports_workers(&self) -> bool {
        false
    }

    fn prepare_worker(
        &mut self,
        _language: &str,
        _state: Vec<u8>,
        _env: BTreeMap<Vec<u8>, Vec<u8>>,
        _display: Vec<u8>,
    ) -> Result<Command> {
        Err(crate::Error::new(
            "isolated shell processes are unavailable on this host",
        ))
    }

    /// Install a disposition and return the platform's canonical name without `SIG`.
    fn set_signal(&mut self, _name: &[u8], _disposition: SignalDisposition) -> Result<Vec<u8>> {
        Err(crate::Error::new(
            "signal traps are unavailable on this host",
        ))
    }

    /// Signals are coalesced and delivered only at evaluator safe points.
    fn pending_signals(&mut self) -> Vec<Vec<u8>> {
        vec![]
    }

    fn push_signal_scope(&mut self) -> Result<()> {
        Ok(())
    }
    fn pop_signal_scope(&mut self) -> Result<()> {
        Ok(())
    }

    fn prepare_redirects(&mut self, redirects: &[Redirection]) -> Result<Vec<Redirection>> {
        Ok(redirects.to_vec())
    }

    fn run_pipeline(
        &mut self,
        stages: &[Command],
        background: bool,
        capture: bool,
    ) -> Result<RunResult>;

    fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>>;

    fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Vec<u8>>;

    fn read_line(
        &mut self,
        redirects: &[Redirection],
        interrupt_for_traps: bool,
    ) -> Result<ReadResult>;

    /// Inspect a resolved descriptor, including redirections, without I/O.
    /// Hosts without terminal streams may retain the default.
    fn fd_is_terminal(
        &mut self,
        _fd: i32,
        _redirects: &[Redirection],
        _capture: bool,
    ) -> Result<bool> {
        Ok(false)
    }

    fn cancelled(&self) -> bool;

    fn jobs(&self) -> Vec<Vec<u8>>;
}
