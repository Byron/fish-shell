//! Owned evaluator access while the native editor is reading a command.
//!
//! The pager and completer share this service. Callbacks temporarily borrow its
//! authoritative session and host; nested `complete -C` stays in the evaluator
//! and never acquires this mutex again.

use super::fish::Session;
use super::host::NativeHost;
use super::interactive_completion::CompletionEngine;
use rash::host::{
    Command, Host, ReadResult, RedirectAction, Redirection, RunResult, SignalDisposition,
};
use rash::{Error, Result};
use rustyline::completion::Pair;
use rustyline::error::ReadlineError;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read as _, Seek as _};
use std::path::Path;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    ENABLE_PROCESSED_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

pub(super) struct ReadRuntime {
    pub session: Session,
    pub host: NativeHost,
}

pub(super) struct CompletionService {
    pub engine: CompletionEngine,
    runtime: Option<ReadRuntime>,
    io: Option<CompletionIo>,
    printer: Option<Box<dyn rustyline::ExternalPrinter + Send>>,
    pending: Vec<u8>,
    read_error: Option<Error>,
}

impl CompletionService {
    pub(super) fn new(language: &str) -> Self {
        Self {
            engine: CompletionEngine::new(language),
            runtime: None,
            io: None,
            printer: None,
            pending: Vec::new(),
            read_error: None,
        }
    }

    pub(super) fn set_printer(
        &mut self,
        printer: impl rustyline::ExternalPrinter + Send + 'static,
    ) {
        self.printer = Some(Box::new(printer));
    }

    pub(super) fn begin_read(&mut self, session: Session, host: NativeHost) {
        assert!(self.runtime.is_none(), "the editor already owns a session");
        self.runtime = Some(ReadRuntime { session, host });
    }

    /// Recover both owners before the caller propagates any reader error.
    pub(super) fn end_read(&mut self) -> (ReadRuntime, Option<Error>, Vec<u8>) {
        self.collect_output();
        (
            self.runtime.take().expect("the editor owns a session"),
            self.read_error.take(),
            std::mem::take(&mut self.pending),
        )
    }

    pub(super) fn complete(
        &mut self,
        line: &str,
        pos: usize,
        cwd: &Path,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if self.runtime.is_none() {
            return Ok(self.engine.complete(line, pos, cwd));
        }

        // Rustyline clears processed input in raw mode. Restore Ctrl-C delivery
        // only while running shell callbacks, without enabling echo/line input.
        let mut console = match CompletionInput::new() {
            Ok(console) => console,
            Err(error) => {
                self.diagnostic(error);
                self.print_output();
                return Ok((pos, Vec::new()));
            }
        };
        if self.io.is_none() {
            match CompletionIo::new() {
                Ok(io) => self.io = Some(io),
                Err(error) => {
                    self.diagnostic(error);
                    if let Err(error) = console.restore() {
                        self.read_error = Some(error);
                        return Err(ReadlineError::Interrupted);
                    }
                    self.print_output();
                    return Ok((pos, Vec::new()));
                }
            }
        }
        let runtime = self.runtime.as_mut().unwrap();
        let mut host = CompletionHost::new(&mut runtime.host, self.io.as_ref().unwrap());
        let result = runtime
            .session
            .completion_query(line, pos, cwd, &mut host, &mut self.engine);
        let cancelled = runtime.host.take_console_interrupt();
        let exited = runtime.session.exit_requested();
        let restored = console.restore();
        self.collect_output();
        let result = match result {
            Ok(result) if !cancelled => result,
            Ok(_) => (pos, Vec::new()),
            Err(error) => {
                if !cancelled {
                    self.diagnostic(error);
                }
                (pos, Vec::new())
            }
        };
        if let Err(error) = restored {
            // Leaving raw mode also retries restoration through Rustyline's
            // own guard. Return owners before reporting this terminal failure.
            self.read_error = Some(error);
            return Err(ReadlineError::Interrupted);
        }
        if exited {
            return Err(ReadlineError::Interrupted);
        }
        self.print_output();
        Ok(result)
    }

    fn diagnostic(&mut self, error: Error) {
        self.pending
            .extend_from_slice(format!("fish: {}\n", error.message).as_bytes());
    }

    fn collect_output(&mut self) {
        if let Some(io) = &mut self.io {
            if let Err(error) = io.collect(&mut self.pending) {
                self.diagnostic(error);
            }
        }
    }

    fn print_output(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if let Some(printer) = &mut self.printer {
            // The Windows printer queue is nonblocking: callbacks run on the
            // same thread that will consume and redraw this message.
            let message = String::from_utf8_lossy(&self.pending).into_owned();
            if printer.print(message).is_ok() {
                self.pending.clear();
            } else {
                // A failed printer falls back to output after readline returns.
                self.printer = None;
            }
        }
    }
}

struct CompletionInput {
    handle: HANDLE,
    saved: u32,
    restored: bool,
}

impl CompletionInput {
    fn new() -> Result<Self> {
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut saved = 0;
        if handle.is_null()
            || handle == INVALID_HANDLE_VALUE
            || unsafe { GetConsoleMode(handle, &mut saved) } == 0
            || unsafe { SetConsoleMode(handle, saved | ENABLE_PROCESSED_INPUT) } == 0
        {
            return Err(Error::new(format!(
                "cannot enable completion cancellation: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self {
            handle,
            saved,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if !self.restored && unsafe { SetConsoleMode(self.handle, self.saved) } == 0 {
            return Err(Error::new(format!(
                "cannot restore completion input mode: {}",
                std::io::Error::last_os_error()
            )));
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for CompletionInput {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(super) struct CompletionIo {
    input: Arc<File>,
    output: Arc<File>,
    reader: File,
}

impl CompletionIo {
    pub(super) fn new() -> Result<Self> {
        let (output, reader) = super::host::temporary_output()?;
        Ok(Self {
            input: Arc::new(File::open("NUL")?),
            output,
            reader,
        })
    }

    pub(super) fn collect(&mut self, bytes: &mut Vec<u8>) -> Result<()> {
        let available = self
            .reader
            .metadata()?
            .len()
            .saturating_sub(self.reader.stream_position()?);
        // Snapshot the length: a background callback may still be writing.
        (&mut self.reader).take(available).read_to_end(bytes)?;
        Ok(())
    }

    fn redirects(
        &self,
        redirects: &[Redirection],
        input: bool,
        output: bool,
        buffer_output: bool,
    ) -> Vec<Redirection> {
        let mut defaults = Vec::with_capacity(redirects.len() + 3);
        if input {
            defaults.push(Redirection {
                fd: 0,
                action: RedirectAction::Opened(Arc::clone(&self.input)),
            });
        }
        if output && buffer_output {
            defaults.push(Redirection {
                fd: 1,
                action: RedirectAction::Opened(Arc::clone(&self.output)),
            });
        }
        if buffer_output {
            defaults.push(Redirection {
                fd: 2,
                action: RedirectAction::Opened(Arc::clone(&self.output)),
            });
        }
        defaults.extend_from_slice(redirects);
        defaults
    }
}

/// Defaults belong at the host boundary, so event handlers that reset their
/// evaluator redirections still cannot read or overwrite the editor's console.
pub(super) struct CompletionHost<'a> {
    host: &'a mut dyn Host,
    io: &'a CompletionIo,
    buffer_output: bool,
}

impl<'a> CompletionHost<'a> {
    pub(super) fn new(host: &'a mut dyn Host, io: &'a CompletionIo) -> Self {
        Self {
            host,
            io,
            buffer_output: true,
        }
    }

    pub(super) fn input_only(host: &'a mut dyn Host, io: &'a CompletionIo) -> Self {
        Self {
            host,
            io,
            buffer_output: false,
        }
    }
}

impl Host for CompletionHost<'_> {
    fn supports_workers(&self) -> bool {
        self.host.supports_workers()
    }

    fn prepare_worker(
        &mut self,
        language: &str,
        state: Vec<u8>,
        env: BTreeMap<Vec<u8>, Vec<u8>>,
        display: Vec<u8>,
    ) -> Result<Command> {
        self.host.prepare_worker(language, state, env, display)
    }

    fn set_signal(&mut self, name: &[u8], disposition: SignalDisposition) -> Result<Vec<u8>> {
        self.host.set_signal(name, disposition)
    }

    fn pending_signals(&mut self) -> Vec<Vec<u8>> {
        self.host.pending_signals()
    }

    fn push_signal_scope(&mut self) -> Result<()> {
        self.host.push_signal_scope()
    }

    fn pop_signal_scope(&mut self) -> Result<()> {
        self.host.pop_signal_scope()
    }

    fn prepare_redirects(&mut self, redirects: &[Redirection]) -> Result<Vec<Redirection>> {
        self.host.prepare_redirects(redirects)
    }

    fn run_pipeline(
        &mut self,
        stages: &[Command],
        background: bool,
        capture: bool,
    ) -> Result<RunResult> {
        let stages: Vec<_> = stages
            .iter()
            .enumerate()
            .map(|(index, stage)| Command {
                redirects: self.io.redirects(
                    &stage.redirects,
                    index == 0,
                    index + 1 == stages.len() && !capture,
                    self.buffer_output,
                ),
                ..stage.clone()
            })
            .collect();
        self.host.run_pipeline(&stages, background, capture)
    }

    fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>> {
        self.host.change_dir(path)
    }

    fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Vec<u8>> {
        self.host.write(
            fd,
            bytes,
            &self
                .io
                .redirects(redirects, true, !capture, self.buffer_output),
            capture,
        )
    }

    fn read_line(&mut self, redirects: &[Redirection], interrupt: bool) -> Result<ReadResult> {
        self.host.read_line(
            &self.io.redirects(redirects, true, true, self.buffer_output),
            interrupt,
        )
    }

    fn cancelled(&self) -> bool {
        self.host.cancelled()
    }

    fn jobs(&self) -> Vec<Vec<u8>> {
        self.host.jobs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_runtime_can_be_shared_by_editor_handlers() {
        fn assert_send<T: Send>() {}
        assert_send::<ReadRuntime>();
        assert_send::<CompletionService>();
    }

    #[test]
    fn completion_streams_separate_capture_and_honor_explicit_redirects() {
        let mut native = NativeHost::new().unwrap();
        let mut io = CompletionIo::new().unwrap();
        {
            let mut host = CompletionHost::new(&mut native, &io);
            let ReadResult::Line(input) = host.read_line(&[], false).unwrap() else {
                panic!("null input cannot be interrupted");
            };
            assert!(input.is_empty());
            assert_eq!(
                host.write(1, b"candidate", &[], true).unwrap(),
                b"candidate"
            );
            assert!(host.write(1, b"event\n", &[], false).unwrap().is_empty());
            assert!(
                host.write(2, b"diagnostic\n", &[], true)
                    .unwrap()
                    .is_empty()
            );
            let redirected = host
                .prepare_redirects(&[Redirection {
                    fd: 0,
                    action: RedirectAction::Here(b"explicit\n".to_vec()),
                }])
                .unwrap();
            let ReadResult::Line(input) = host.read_line(&redirected, false).unwrap() else {
                panic!();
            };
            assert_eq!(input, b"explicit\n");
            assert_eq!(
                host.write(
                    2,
                    b"explicit capture",
                    &[Redirection {
                        fd: 2,
                        action: RedirectAction::Dup(1),
                    }],
                    true,
                )
                .unwrap(),
                b"explicit capture"
            );
        }
        let mut bytes = Vec::new();
        io.collect(&mut bytes).unwrap();
        assert_eq!(bytes, b"event\ndiagnostic\n");
        // Reading diagnostics must not move the writer's cursor backwards.
        native
            .write(2, b"later\n", &io.redirects(&[], true, true, true), false)
            .unwrap();
        bytes.clear();
        io.collect(&mut bytes).unwrap();
        assert_eq!(bytes, b"later\n");
    }
}
