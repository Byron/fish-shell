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
    /// The descriptor attached to the next stage's stdin, normally stdout.
    /// Ignored on the final stage. Native hosts also support stderr (fd 2).
    pub pipe_fd: i32,
    /// Only explicitly selected job-control services use a host builtin.
    pub builtin: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub status: i32,
    /// A suspended foreground pipeline has not finished producing output.
    pub stopped: bool,
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

    /// Write one explicitly separated field. Captures retain even an empty field;
    /// ordinary streams honor the requested physical trailing newline.
    fn write_separated(
        &mut self,
        fd: i32,
        bytes: &[u8],
        want_newline: bool,
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Option<Vec<u8>>> {
        let mut captured = [false, capture, false];
        for redirect in redirects {
            let destination = usize::try_from(redirect.fd)
                .ok()
                .filter(|index| *index < captured.len())
                .ok_or_else(|| crate::Error::new("invalid output descriptor"))?;
            captured[destination] = match redirect.action {
                RedirectAction::Dup(source) => *captured
                    .get(source as usize)
                    .ok_or_else(|| crate::Error::new("invalid output descriptor"))?,
                _ => false,
            };
        }
        let captured = *captured
            .get(fd as usize)
            .ok_or_else(|| crate::Error::new("invalid output descriptor"))?;
        if captured {
            self.write(fd, bytes, redirects, capture).map(Some)
        } else {
            let mut rendered = bytes.to_vec();
            if want_newline {
                rendered.push(b'\n');
            }
            self.write(fd, &rendered, redirects, capture)?;
            Ok(None)
        }
    }

    /// A native worker may inherit an explicitly separated capture stream.
    /// Its private framing file is propagated to descendant interpreter workers.
    fn inherited_capture(&self, _fd: i32) -> Option<std::path::PathBuf> {
        None
    }

    fn read_line(
        &mut self,
        redirects: &[Redirection],
        interrupt_for_traps: bool,
    ) -> Result<ReadResult>;

    /// Read through one delimiter (included), a Unicode character limit, or EOF.
    /// A missing delimiter reads until EOF. The host must not consume a later
    /// record: multiple builtins may share the same opened input description.
    fn read_record(
        &mut self,
        redirects: &[Redirection],
        delimiter: Option<u8>,
        max_chars: Option<usize>,
        interrupt_for_traps: bool,
    ) -> Result<ReadResult> {
        if delimiter == Some(b'\n') && max_chars.is_none() {
            self.read_line(redirects, interrupt_for_traps)
        } else {
            Err(crate::Error::new(
                "this host does not support the requested input record mode",
            ))
        }
    }

    /// Copy stdin without decoding text or imposing a captured-record limit.
    /// Return false on interruption; the caller owns incomplete output cleanup.
    fn copy_input_to(
        &mut self,
        redirects: &[Redirection],
        output: &mut dyn std::io::Write,
    ) -> Result<bool> {
        loop {
            match self.read_record(redirects, None, Some(8192), false)? {
                ReadResult::Line(bytes) if bytes.is_empty() => return Ok(true),
                ReadResult::Line(bytes) => output.write_all(&bytes)?,
                ReadResult::Interrupted(_) => return Ok(false),
            }
        }
    }

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

    /// Transfer reserved temporary directories to a stopped job. The host
    /// removes them after the job finishes, including after a later resume.
    fn retain_job_directories(
        &mut self,
        _last_pid: u32,
        directories: &[std::path::PathBuf],
    ) -> Result<()> {
        if directories.is_empty() {
            Ok(())
        } else {
            Err(crate::Error::new(
                "this host cannot retain stopped-job resources",
            ))
        }
    }
}
