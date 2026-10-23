//! Explicit output boundaries across native interpreter processes.
//!
//! Only interpreter workers see these private framed files. External programs
//! use ordinary byte streams, which the host appends as inferred output.
use crate::separated_buffer::{SeparatedBuffer, SeparationType};
use rash::host::{
    Command, Host, ReadResult, RedirectAction, Redirection, RunResult, SignalDisposition,
};
use rash::{Error, Result};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const LIMIT: usize = crate::native::host::CAPTURE_BYTE_LIMIT;
pub(super) const ELEMENT_LIMIT: usize = 100_000;
const HEADER_SIZE: usize = 16;
const FRAME_LIMIT: u64 = LIMIT as u64 + ELEMENT_LIMIT as u64 * 5 + HEADER_SIZE as u64;

pub(super) struct CaptureFile(crate::native::worker::PayloadFile);

impl CaptureFile {
    pub fn new() -> Result<Self> {
        let (path, mut file) = crate::native::worker::PayloadFile::create()?;
        file.write_all(&[0; HEADER_SIZE])?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    pub fn read(&self) -> Result<SeparatedBuffer> {
        let mut bytes = Vec::new();
        File::open(self.path())?
            .take(FRAME_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > FRAME_LIMIT {
            return Err(Error::new("command substitution output limit exceeded"));
        }
        let header = bytes
            .get(..HEADER_SIZE)
            .ok_or_else(|| Error::new("truncated captured output"))?;
        let expected_count = u64::from_le_bytes(header[..8].try_into().unwrap());
        let expected_size = u64::from_le_bytes(header[8..].try_into().unwrap());
        if expected_count > ELEMENT_LIMIT as u64 || expected_size > LIMIT as u64 {
            return Err(Error::new("command substitution output limit exceeded"));
        }
        let mut buffer = SeparatedBuffer::new(LIMIT);
        let mut cursor = &bytes[HEADER_SIZE..];
        let mut count = 0;
        while !cursor.is_empty() {
            count += 1;
            if count > expected_count {
                return Err(Error::new("invalid captured output count"));
            }
            let header = cursor
                .get(..5)
                .ok_or_else(|| Error::new("truncated captured output"))?;
            let separation = match header[0] {
                0 => SeparationType::Inferred,
                1 => SeparationType::Explicitly,
                _ => return Err(Error::new("invalid captured output boundary")),
            };
            let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
            let data = cursor
                .get(5..5 + length)
                .ok_or_else(|| Error::new("truncated captured output"))?;
            if !buffer.append(data, separation) {
                return Err(Error::new("command substitution output limit exceeded"));
            }
            cursor = &cursor[5 + length..];
        }
        if count != expected_count || buffer.len() as u64 != expected_size {
            return Err(Error::new("invalid captured output size"));
        }
        Ok(buffer)
    }
}

/// Lock the first byte for one complete frame. Separate interpreter workers can
/// share a destination; a partial write must never splice their frames together.
struct FrameLock<'a>(&'a File);

impl<'a> FrameLock<'a> {
    fn acquire(file: &'a File) -> Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
        use windows_sys::Win32::Storage::FileSystem::LockFile;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            // SAFETY: the borrowed file keeps this handle alive until unlocking.
            if unsafe { LockFile(file.as_raw_handle(), 0, 0, 1, 0) } != 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_LOCK_VIOLATION as i32)
                || std::time::Instant::now() >= deadline
            {
                return Err(error.into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl Drop for FrameLock<'_> {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::UnlockFile;
        // SAFETY: this guard owns the corresponding lock on the live file.
        unsafe { UnlockFile(self.0.as_raw_handle(), 0, 0, 1, 0) };
    }
}

fn append(path: &Path, bytes: &[u8], separation: SeparationType) -> Result<()> {
    if bytes.is_empty() && separation == SeparationType::Inferred {
        return Ok(());
    }
    // Never create a missing path: cancellation removes the parent's reservation.
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let _lock = FrameLock::acquire(&file)?;
    let mut stream = &file;
    let mut header = [0; HEADER_SIZE];
    stream.read_exact(&mut header)?;
    let count = u64::from_le_bytes(header[..8].try_into().unwrap());
    let size = u64::from_le_bytes(header[8..].try_into().unwrap());
    if count >= ELEMENT_LIMIT as u64 || size.saturating_add(bytes.len() as u64) > LIMIT as u64 {
        stream.rewind()?;
        stream.write_all(&u64::MAX.to_le_bytes())?;
        return Err(Error::new("command substitution output limit exceeded"));
    }
    let offset = stream.seek(std::io::SeekFrom::End(0))?;
    if offset + 5 + bytes.len() as u64 > FRAME_LIMIT {
        return Err(Error::new("command substitution output limit exceeded"));
    }
    let mut frame = Vec::with_capacity(5 + bytes.len());
    frame.push(u8::from(separation == SeparationType::Explicitly));
    frame.extend((bytes.len() as u32).to_le_bytes());
    frame.extend(bytes);
    stream.write_all(&frame)?;
    stream.rewind()?;
    stream.write_all(&(count + 1).to_le_bytes())?;
    stream.write_all(&(size + bytes.len() as u64).to_le_bytes())?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Destination {
    Ordinary,
    Local,
    Inherited(PathBuf),
}

fn destinations(
    inherited: &[Option<PathBuf>; 3],
    redirects: &[Redirection],
    capture: bool,
    pipe: Option<i32>,
) -> Result<[Destination; 3]> {
    let mut result = inherited
        .clone()
        .map(|path| path.map_or(Destination::Ordinary, Destination::Inherited));
    if capture {
        result[1] = Destination::Local;
    }
    if let Some(fd) = pipe {
        *result
            .get_mut(fd as usize)
            .ok_or_else(|| Error::new("invalid pipe descriptor"))? = Destination::Ordinary;
    }
    let pipeline = result.clone();
    for redirect in redirects {
        let destination = usize::try_from(redirect.fd)
            .ok()
            .filter(|fd| *fd < result.len())
            .ok_or_else(|| Error::new("invalid captured output descriptor"))?;
        result[destination] = match &redirect.action {
            RedirectAction::Pipeline => pipeline[destination].clone(),
            RedirectAction::Dup(source) => result
                .get(*source as usize)
                .ok_or_else(|| Error::new("invalid captured output descriptor"))?
                .clone(),
            _ => Destination::Ordinary,
        };
    }
    if result[0] != Destination::Ordinary {
        return Err(Error::new(
            "the capture output stream is not available for reading",
        ));
    }
    Ok(result)
}

/// Resolve the descriptors inherited by a pipeline worker at the parent scope.
pub(super) fn worker_captures(
    host: &impl Host,
    redirects: &[Redirection],
    capture: Option<&Path>,
    pipe: Option<i32>,
) -> Result<[Option<PathBuf>; 3]> {
    let inherited = std::array::from_fn(|fd| host.inherited_capture(fd as i32));
    Ok(
        destinations(&inherited, redirects, capture.is_some(), pipe)?.map(|destination| {
            match destination {
                Destination::Ordinary => None,
                Destination::Local => capture.map(Path::to_owned),
                Destination::Inherited(path) => Some(path),
            }
        }),
    )
}

pub(super) struct CapturingHost<'a, H> {
    inner: &'a mut H,
    inherited: [Option<PathBuf>; 3],
}

impl<'a, H: Host> CapturingHost<'a, H> {
    pub fn new(inner: &'a mut H, inherited: [Option<PathBuf>; 3]) -> Self {
        Self { inner, inherited }
    }

    fn destination(
        &self,
        fd: i32,
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Destination> {
        destinations(&self.inherited, redirects, capture, None)?
            .get(fd as usize)
            .cloned()
            .ok_or_else(|| Error::new("invalid captured output descriptor"))
    }
}

impl<H: Host> Host for CapturingHost<'_, H> {
    fn supports_workers(&self) -> bool {
        self.inner.supports_workers()
    }

    fn prepare_worker(
        &mut self,
        language: &str,
        state: Vec<u8>,
        env: BTreeMap<Vec<u8>, Vec<u8>>,
        display: Vec<u8>,
    ) -> Result<Command> {
        self.inner.prepare_worker(language, state, env, display)
    }

    fn inherited_capture(&self, fd: i32) -> Option<PathBuf> {
        self.inherited.get(fd as usize).cloned().flatten()
    }

    fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Vec<u8>> {
        match self.destination(fd, redirects, capture)? {
            Destination::Inherited(path) => {
                append(&path, bytes, SeparationType::Inferred)?;
                Ok(Vec::new())
            }
            _ => self.inner.write(fd, bytes, redirects, capture),
        }
    }

    fn write_separated(
        &mut self,
        fd: i32,
        bytes: &[u8],
        want_newline: bool,
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Option<Vec<u8>>> {
        match self.destination(fd, redirects, capture)? {
            Destination::Inherited(path) => {
                append(&path, bytes, SeparationType::Explicitly)?;
                Ok(None)
            }
            _ => self
                .inner
                .write_separated(fd, bytes, want_newline, redirects, capture),
        }
    }

    fn run_pipeline(
        &mut self,
        stages: &[Command],
        background: bool,
        capture: bool,
    ) -> Result<RunResult> {
        let mut streams = BTreeMap::<PathBuf, Arc<File>>::new();
        let mut redirected = Vec::with_capacity(stages.len());
        for (index, stage) in stages.iter().enumerate() {
            let pipe = (index + 1 < stages.len()).then_some(stage.pipe_fd);
            let resolved = destinations(&self.inherited, &stage.redirects, capture, pipe)?;
            if background
                && resolved
                    .iter()
                    .any(|stream| matches!(stream, Destination::Inherited(_)))
            {
                return Err(Error::new(
                    "background jobs inside command substitutions are not supported",
                ));
            }
            let mut stage = stage.clone();
            let mut prefix = Vec::new();
            for fd in [1, 2] {
                if pipe == Some(fd as i32) || (fd == 1 && capture) {
                    continue;
                }
                if let Some(path) = &self.inherited[fd] {
                    let file = match streams.get(path) {
                        Some(file) => file.clone(),
                        None => {
                            let file = crate::native::host::temporary_file(&[], false)?;
                            streams.insert(path.clone(), file.clone());
                            file
                        }
                    };
                    prefix.push(Redirection {
                        fd: fd as i32,
                        action: RedirectAction::Opened(file),
                    });
                }
            }
            prefix.append(&mut stage.redirects);
            stage.redirects = prefix;
            redirected.push(stage);
        }
        let result = self.inner.run_pipeline(&redirected, background, capture);
        if self.inner.cancelled() || result.as_ref().is_ok_and(|result| result.stopped) {
            return result;
        }
        // Synchronous calls finish writing before the inferred frame is appended.
        // A shared file for duplicated descriptors preserves their byte order.
        for (path, file) in streams {
            let mut stream = &*file;
            stream.rewind()?;
            let mut bytes = Vec::new();
            stream.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
            append(&path, &bytes, SeparationType::Inferred)?;
        }
        result
    }

    fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>> {
        self.inner.change_dir(path)
    }
    fn prepare_redirects(&mut self, redirects: &[Redirection]) -> Result<Vec<Redirection>> {
        self.inner.prepare_redirects(redirects)
    }
    fn read_line(&mut self, redirects: &[Redirection], interrupt: bool) -> Result<ReadResult> {
        self.inner.read_line(redirects, interrupt)
    }
    fn read_record(
        &mut self,
        redirects: &[Redirection],
        delimiter: Option<u8>,
        max_chars: Option<usize>,
        interrupt: bool,
    ) -> Result<ReadResult> {
        self.inner
            .read_record(redirects, delimiter, max_chars, interrupt)
    }
    fn copy_input_to(
        &mut self,
        redirects: &[Redirection],
        output: &mut dyn std::io::Write,
    ) -> Result<bool> {
        self.inner.copy_input_to(redirects, output)
    }
    fn fd_is_terminal(
        &mut self,
        fd: i32,
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<bool> {
        match self.destination(fd, redirects, capture)? {
            Destination::Ordinary => self.inner.fd_is_terminal(fd, redirects, capture),
            _ => Ok(false),
        }
    }
    fn set_signal(&mut self, name: &[u8], disposition: SignalDisposition) -> Result<Vec<u8>> {
        self.inner.set_signal(name, disposition)
    }
    fn pending_signals(&mut self) -> Vec<Vec<u8>> {
        self.inner.pending_signals()
    }
    fn push_signal_scope(&mut self) -> Result<()> {
        self.inner.push_signal_scope()
    }
    fn pop_signal_scope(&mut self) -> Result<()> {
        self.inner.pop_signal_scope()
    }
    fn cancelled(&self) -> bool {
        self.inner.cancelled()
    }
    fn jobs(&self) -> Vec<Vec<u8>> {
        self.inner.jobs()
    }
    fn retain_job_resources(
        &mut self,
        last_pid: u32,
        resources: &[rash::host::JobResource],
    ) -> Result<()> {
        self.inner.retain_job_resources(last_pid, resources)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_frames_preserve_empty_fields_and_mixed_output() {
        let capture = CaptureFile::new().unwrap();
        append(capture.path(), b"before", SeparationType::Inferred).unwrap();
        append(capture.path(), b"", SeparationType::Explicitly).unwrap();
        append(capture.path(), b"a\nb", SeparationType::Explicitly).unwrap();
        append(capture.path(), b"after", SeparationType::Inferred).unwrap();
        let output = capture.read().unwrap();
        assert_eq!(output.elements().len(), 4);
        assert!(output.elements()[1].is_explicitly_separated());
        assert_eq!(output.newline_serialized(), b"before\na\nb\nafter");
    }

    #[test]
    fn capture_frames_reject_partial_or_excessive_output() {
        let capture = CaptureFile::new().unwrap();
        std::fs::write(capture.path(), [1, 3, 0, 0, 0, b'x']).unwrap();
        assert!(capture.read().is_err());
        assert!(
            append(
                capture.path(),
                &vec![0; LIMIT + 1],
                SeparationType::Inferred
            )
            .is_err()
        );
    }

    #[test]
    fn capture_limits_bound_empty_field_metadata_and_discard_overflow() {
        let capture = CaptureFile::new().unwrap();
        let mut header = (ELEMENT_LIMIT as u64).to_le_bytes().to_vec();
        header.extend(0u64.to_le_bytes());
        std::fs::write(capture.path(), header).unwrap();
        assert!(append(capture.path(), b"", SeparationType::Explicitly).is_err());
        assert!(capture.read().is_err());

        let capture = CaptureFile::new().unwrap();
        append(capture.path(), b"before", SeparationType::Inferred).unwrap();
        assert!(append(capture.path(), &vec![0; LIMIT], SeparationType::Inferred).is_err());
        assert!(
            capture.read().is_err(),
            "overflow must discard the preceding output too"
        );
    }

    #[test]
    fn capture_destinations_resolve_duplication_before_later_redirects() {
        let inherited = [None, None, Some(PathBuf::from("outer"))];
        let redirects = [
            Redirection {
                fd: 2,
                action: RedirectAction::Dup(1),
            },
            Redirection {
                fd: 1,
                action: RedirectAction::Close,
            },
        ];
        assert_eq!(
            destinations(&inherited, &redirects, true, None).unwrap(),
            [
                Destination::Ordinary,
                Destination::Ordinary,
                Destination::Local,
            ]
        );
        assert_eq!(
            destinations(&inherited, &[], true, None).unwrap()[2],
            Destination::Inherited(PathBuf::from("outer"))
        );
    }
}
