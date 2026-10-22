//! Native process and stream services for the portable shell evaluators.
//!
//! Expanded arguments go directly to `Command`; this adapter never invokes a
//! command interpreter to execute shell source.

use rash::host::{Command, Host, OpenMode, ReadResult, RedirectAction, Redirection, RunResult};
use rash::{Error, Result};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "jobs.rs"]
mod jobs;
pub(crate) use jobs::JobView;
#[path = "process/mod.rs"]
mod process;

pub struct NativeHost {
    jobs: BTreeMap<u32, jobs::Job>,
    /// Parent jobs can be inspected in workers, but never controlled or reaped.
    inherited_jobs: Vec<JobView>,
    next_job: u32,
    notifications: VecDeque<String>,
    controls: process::Controls,
    interrupted: bool,
    worker: bool,
    pending_workers: Vec<super::worker::PayloadFile>,
}

impl NativeHost {
    pub fn new() -> Result<Self> {
        Self::with_worker(false, Vec::new())
    }

    pub(crate) fn new_worker(inherited_jobs: Vec<JobView>) -> Result<Self> {
        Self::with_worker(true, inherited_jobs)
    }

    fn with_worker(worker: bool, inherited_jobs: Vec<JobView>) -> Result<Self> {
        // Fail early if the working directory cannot be represented by this host.
        rash::os::to_bytes(std::env::current_dir()?.as_os_str())?;
        process::install_controls(!worker)?;
        let next_job = inherited_jobs
            .iter()
            .map(|job| job.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::new("job IDs exhausted"))?;
        Ok(Self {
            jobs: BTreeMap::new(),
            inherited_jobs,
            next_job,
            notifications: VecDeque::new(),
            controls: process::controls(),
            interrupted: false,
            worker,
            pending_workers: Vec::new(),
        })
    }
}

#[derive(Clone)]
enum Target {
    Inherited(i32),
    File(Arc<File>),
    Capture,
    Closed,
    Null,
}

fn descriptor(fd: i32) -> Result<usize> {
    if (0..=2).contains(&fd) {
        Ok(fd as usize)
    } else {
        Err(Error::new(
            "native redirections currently support only descriptors 0, 1 and 2",
        ))
    }
}

fn checked_os(bytes: &[u8]) -> Result<std::ffi::OsString> {
    if bytes.contains(&0) {
        return Err(Error::new(
            "NUL in a native argument, environment value or path",
        ));
    }
    rash::os::from_bytes(bytes)
}

fn open_file(path: &[u8], mode: OpenMode) -> Result<Arc<File>> {
    // Shared Fish startup commonly redirects optional capability checks here.
    // Windows has the same sink/source semantics through its NUL device.
    #[cfg(windows)]
    let path = if path == b"/dev/null" { b"NUL" } else { path };
    let mut options = OpenOptions::new();
    match mode {
        OpenMode::Read => {
            options.read(true);
        }
        OpenMode::Write | OpenMode::Clobber => {
            options.write(true).create(true).truncate(true);
        }
        OpenMode::Append => {
            options.append(true).create(true);
        }
        OpenMode::ReadWrite => {
            options.read(true).write(true).create(true);
        }
    }
    Ok(Arc::new(options.open(checked_os(path)?)?))
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_path() -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rash-native-{}-{stamp}-{sequence}.tmp",
        std::process::id()
    ))
}

/// Shared seekable input/capture, removed when the last handle closes. Captures
/// use a file so a child writing both stdout and stderr cannot fill an unread pipe.
fn temporary_file(bytes: &[u8], read_only: bool) -> Result<Arc<File>> {
    Ok(temporary_stream(bytes, read_only, false)?.0)
}

/// A reader with its own cursor can collect completion diagnostics while a
/// background child continues writing through the shared output description.
#[cfg(windows)]
pub(super) fn temporary_output() -> Result<(Arc<File>, File)> {
    let (output, reader) = temporary_stream(&[], false, true)?;
    Ok((output, reader.expect("requested an independent reader")))
}

fn temporary_stream(
    bytes: &[u8],
    read_only: bool,
    independent_reader: bool,
) -> Result<(Arc<File>, Option<File>)> {
    for _ in 0..64 {
        let path = temporary_path();
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // FILE_FLAG_DELETE_ON_CLOSE | FILE_ATTRIBUTE_TEMPORARY.
            options.custom_flags(0x0400_0100);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        // Here input must remain read-only even when redirected onto an output
        // descriptor. Open before unlinking; both handles refer to the same file.
        let input = read_only.then(|| File::open(&path));
        let reader = independent_reader.then(|| File::open(&path));
        #[cfg(unix)]
        std::fs::remove_file(&path)?;
        let input = input.transpose()?;
        let reader = reader.transpose()?;
        file.write_all(bytes)?;
        file.rewind()?;
        return Ok((Arc::new(input.unwrap_or(file)), reader));
    }
    Err(Error::new("could not create a native temporary stream"))
}

fn targets(redirects: &[Redirection], capture: bool) -> Result<[Target; 3]> {
    apply_redirects(
        redirects,
        [
            Target::Inherited(0),
            if capture {
                Target::Capture
            } else {
                Target::Inherited(1)
            },
            Target::Inherited(2),
        ],
    )
}

fn apply_redirects(redirects: &[Redirection], mut targets: [Target; 3]) -> Result<[Target; 3]> {
    // Resolve duplication at its position in the list, before later replacements.
    for redirect in redirects {
        let fd = descriptor(redirect.fd)?;
        targets[fd] = match &redirect.action {
            RedirectAction::File { path, mode } => Target::File(open_file(path, *mode)?),
            RedirectAction::Opened(file) => Target::File(file.clone()),
            RedirectAction::Here(bytes) => Target::File(temporary_file(bytes, true)?),
            RedirectAction::Dup(source) => {
                let target = &targets[descriptor(*source)?];
                if matches!(target, Target::Closed) {
                    return Err(Error::new("cannot duplicate a closed descriptor"));
                }
                target.clone()
            }
            RedirectAction::Close => Target::Closed,
        };
    }
    Ok(targets)
}

fn inherited_stdio(fd: i32) -> Result<Stdio> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle as _;
        let handle = match fd {
            0 => io::stdin().as_handle().try_clone_to_owned()?,
            1 => io::stdout().as_handle().try_clone_to_owned()?,
            2 => io::stderr().as_handle().try_clone_to_owned()?,
            _ => unreachable!(),
        };
        Ok(handle.into())
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        let descriptor = match fd {
            0 => io::stdin().as_fd().try_clone_to_owned()?,
            1 => io::stdout().as_fd().try_clone_to_owned()?,
            2 => io::stderr().as_fd().try_clone_to_owned()?,
            _ => unreachable!(),
        };
        Ok(descriptor.into())
    }
}

fn child_stdio(target: &Target, fd: i32, capture: Option<&File>) -> Result<Stdio> {
    match target {
        Target::Inherited(source) if *source == fd => Ok(Stdio::inherit()),
        Target::Inherited(source) => inherited_stdio(*source),
        Target::File(file) => Ok(file.try_clone()?.into()),
        Target::Capture if fd == 0 => Err(Error::new(
            "the capture output stream is not available for reading",
        )),
        Target::Capture => Ok(capture.expect("capture file exists").try_clone()?.into()),
        Target::Null => Ok(Stdio::null()),
        Target::Closed => Err(Error::new(
            "closing an external command's descriptor is not supported by the native host yet",
        )),
    }
}

fn write_target(target: &Target, bytes: &[u8]) -> Result<Vec<u8>> {
    match target {
        Target::Capture => return Ok(bytes.to_vec()),
        Target::File(file) => (&**file).write_all(bytes)?,
        Target::Inherited(1) => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
        }
        Target::Inherited(2) => {
            let mut out = io::stderr().lock();
            out.write_all(bytes)?;
            out.flush()?;
        }
        Target::Closed if bytes.is_empty() => {}
        Target::Null => {}
        _ => return Err(Error::new("descriptor is not available for writing")),
    }
    Ok(Vec::new())
}

#[cfg(all(test, unix))]
fn exit_status(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal().map_or(1, |signal| 128 + signal)
    }
    #[cfg(windows)]
    {
        1
    }
}

impl Host for NativeHost {
    fn supports_workers(&self) -> bool {
        true
    }

    fn prepare_worker(
        &mut self,
        language: &str,
        state: Vec<u8>,
        env: BTreeMap<Vec<u8>, Vec<u8>>,
        display: Vec<u8>,
    ) -> Result<Command> {
        // Capture before launch inserts this helper pipeline into the job table.
        let jobs = self.worker_job_views()?;
        let (command, file) = super::worker::prepare(language, state, jobs, env, display)?;
        self.keep_worker(file);
        Ok(command)
    }
    fn prepare_redirects(&mut self, redirects: &[Redirection]) -> Result<Vec<Redirection>> {
        redirects
            .iter()
            .map(|redirect| {
                descriptor(redirect.fd)?;
                let action = match &redirect.action {
                    RedirectAction::File { path, mode } => {
                        RedirectAction::Opened(open_file(path, *mode)?)
                    }
                    RedirectAction::Here(bytes) => {
                        RedirectAction::Opened(temporary_file(bytes, true)?)
                    }
                    RedirectAction::Dup(fd) => {
                        descriptor(*fd)?;
                        redirect.action.clone()
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
        stages: &[Command],
        background: bool,
        capture: bool,
    ) -> Result<RunResult> {
        self.launch(stages, background, capture)
    }

    fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>> {
        std::env::set_current_dir(checked_os(path)?)?;
        rash::os::to_bytes(std::env::current_dir()?.as_os_str())
    }

    fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<Vec<u8>> {
        let fd = descriptor(fd)?;
        write_target(&targets(redirects, capture)?[fd], bytes)
    }

    fn read_line(&mut self, redirects: &[Redirection], _: bool) -> Result<ReadResult> {
        self.read_record(redirects, Some(b'\n'), None, false)
    }

    fn read_record(
        &mut self,
        redirects: &[Redirection],
        delimiter: Option<u8>,
        max_chars: Option<usize>,
        _: bool,
    ) -> Result<ReadResult> {
        let targets = targets(redirects, false)?;
        let mut input: Box<dyn io::Read + '_> = match &targets[0] {
            Target::Inherited(0) => Box::new(io::stdin().lock()),
            Target::File(file) => Box::new(&**file),
            _ => return Err(Error::new("descriptor is not available for reading")),
        };
        let mut line = Vec::new();
        let mut byte = [0];
        let mut characters = 0;
        let mut continuation = 0;
        loop {
            if self.cancelled() {
                return Ok(ReadResult::Interrupted(line));
            }
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    line.push(byte[0]);
                    if delimiter == Some(byte[0]) {
                        break;
                    }
                    if line.len() > 16 * 1024 * 1024 {
                        return Err(Error { message: "input record limit exceeded".into(), status: 122 });
                    }
                    if continuation > 0 && byte[0] & 0xc0 == 0x80 {
                        continuation -= 1;
                    } else {
                        continuation = match byte[0] {
                            0xc2..=0xdf => 1,
                            0xe0..=0xef => 2,
                            0xf0..=0xf4 => 3,
                            _ => 0,
                        };
                    }
                    if continuation == 0 {
                        characters += 1;
                        if max_chars.is_some_and(|limit| characters >= limit) {
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(ReadResult::Line(line))
    }

    fn fd_is_terminal(
        &mut self,
        fd: i32,
        redirects: &[Redirection],
        capture: bool,
    ) -> Result<bool> {
        use std::io::IsTerminal;
        match &targets(redirects, capture)?[descriptor(fd)?] {
            Target::Inherited(0) => Ok(io::stdin().is_terminal()),
            Target::Inherited(1) => Ok(io::stdout().is_terminal()),
            Target::Inherited(2) => Ok(io::stderr().is_terminal()),
            Target::File(file) => Ok(file.is_terminal()),
            Target::Capture | Target::Closed => Ok(false),
            _ => Err(Error::new(format!("descriptor {fd} is unavailable"))),
        }
    }

    fn cancelled(&self) -> bool {
        self.interrupted || process::controls().interrupt != self.controls.interrupt
    }

    fn jobs(&self) -> Vec<Vec<u8>> {
        self.job_views()
            .values()
            .map(|job| job.description(false).into_bytes())
            .collect()
    }
}

#[cfg(test)]
#[path = "host_jobs_tests.rs"]
mod job_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn posix_null_redirects_use_the_windows_null_device() {
        for mode in [
            OpenMode::Write,
            OpenMode::Append,
            OpenMode::Clobber,
            OpenMode::ReadWrite,
        ] {
            let file = open_file(b"/dev/null", mode).unwrap();
            (&*file).write_all(b"discarded").unwrap();
        }
        let file = open_file(b"/dev/null", OpenMode::Read).unwrap();
        assert_eq!((&*file).read(&mut [0; 1]).unwrap(), 0);
        let mut host = NativeHost::new().unwrap();
        let redirects = host
            .prepare_redirects(&[Redirection {
                fd: 2,
                action: RedirectAction::File {
                    path: b"/dev/null".to_vec(),
                    mode: OpenMode::Write,
                },
            }])
            .unwrap();
        assert!(
            host.write(2, b"quiet probe", &redirects, true)
                .unwrap()
                .is_empty()
        );
    }

    pub(super) struct TestFile(pub(super) PathBuf);

    impl TestFile {
        pub(super) fn new() -> Self {
            let path = temporary_path();
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            Self(path)
        }

        fn redirect(&self, fd: i32) -> Redirection {
            Redirection {
                fd,
                action: RedirectAction::File {
                    path: rash::os::to_bytes(self.0.as_os_str()).unwrap(),
                    mode: OpenMode::Write,
                },
            }
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn scoped_redirections_preserve_offsets_and_duplication_order() {
        let file = TestFile::new();
        let mut host = NativeHost::new().unwrap();
        let redirects = host
            .prepare_redirects(&[
                Redirection {
                    fd: 2,
                    action: RedirectAction::Dup(1),
                },
                file.redirect(1),
            ])
            .unwrap();
        assert_eq!(
            host.write(2, b"captured", &redirects, true).unwrap(),
            b"captured"
        );
        host.write(1, b"first", &redirects, true).unwrap();
        host.write(1, b" second", &redirects, true).unwrap();
        assert_eq!(std::fs::read(&file.0).unwrap(), b"first second");
        drop(redirects);

        let redirects = host
            .prepare_redirects(&[
                file.redirect(1),
                Redirection {
                    fd: 2,
                    action: RedirectAction::Dup(1),
                },
            ])
            .unwrap();
        assert!(host.write(2, b"file", &redirects, true).unwrap().is_empty());
        assert_eq!(std::fs::read(&file.0).unwrap(), b"file");
    }

    #[test]
    fn terminal_queries_resolve_capture_and_ordered_descriptor_duplication() {
        use std::io::IsTerminal;
        let mut host = NativeHost::new().unwrap();
        let stderr_is_terminal = std::io::stderr().is_terminal();
        assert!(!host.fd_is_terminal(1, &[], true).unwrap());
        let stderr_to_capture = Redirection {
            fd: 2,
            action: RedirectAction::Dup(1),
        };
        assert!(!host.fd_is_terminal(2, &[stderr_to_capture], true).unwrap());
        let stdout_to_stderr = Redirection {
            fd: 1,
            action: RedirectAction::Dup(2),
        };
        assert_eq!(
            host.fd_is_terminal(1, &[stdout_to_stderr.clone()], true)
                .unwrap(),
            stderr_is_terminal
        );
        // A later close must not retroactively change the previously duplicated handle.
        let closed_stderr = Redirection {
            fd: 2,
            action: RedirectAction::Close,
        };
        assert_eq!(
            host.fd_is_terminal(1, &[stdout_to_stderr, closed_stderr.clone()], true)
                .unwrap(),
            stderr_is_terminal
        );
        assert!(!host.fd_is_terminal(2, &[closed_stderr], true).unwrap());
        let file = TestFile::new();
        let file_first = [
            file.redirect(1),
            Redirection {
                fd: 2,
                action: RedirectAction::Dup(1),
            },
        ];
        let redirects = host.prepare_redirects(&file_first).unwrap();
        assert!(!host.fd_is_terminal(1, &redirects, false).unwrap());
        assert!(!host.fd_is_terminal(2, &redirects, false).unwrap());
    }

    #[test]
    fn here_input_is_shared_between_reads() {
        let mut host = NativeHost::new().unwrap();
        let redirects = host
            .prepare_redirects(&[Redirection {
                fd: 0,
                action: RedirectAction::Here(b"first\nsecond\n".to_vec()),
            }])
            .unwrap();
        for expected in [b"first\n".as_slice(), b"second\n", b""] {
            let ReadResult::Line(line) = host.read_line(&redirects, false).unwrap() else {
                panic!()
            };
            assert_eq!(line, expected);
        }
        let mut output = redirects.clone();
        output[0].fd = 1;
        assert!(
            host.write(1, b"cannot write here input", &output, false)
                .is_err()
        );
    }

    pub(super) fn probe_command() -> Command {
        let mut command = Command {
            argv: vec![
                rash::os::to_bytes(std::env::current_exe().unwrap().as_os_str()).unwrap(),
                b"--exact".to_vec(),
                b"native::host::tests::process_probe".to_vec(),
                b"--nocapture".to_vec(),
                b"--skip".to_vec(),
                "argument spaces & $() \"unicode Ω\"".as_bytes().to_vec(),
            ],
            display: None,
            env: [(
                b"RASH_NATIVE_HOST_PROBE".to_vec(),
                b"spaces & $literal".to_vec(),
            )]
            .into(),
            redirects: vec![Redirection {
                fd: 2,
                action: RedirectAction::Dup(1),
            }],
            builtin: false,
        };
        // MSYS2 Rust test executables load runtime DLLs through PATH. Preserve
        // that loader path while still checking that other parent exports clear.
        if std::env::consts::OS == "cygwin" {
            if let Some(path) = std::env::var_os("PATH") {
                command
                    .env
                    .insert(b"PATH".to_vec(), rash::os::to_bytes(&path).unwrap());
            }
        }
        if let Some((name, _)) = std::env::vars_os().find(|(name, _)| {
            let name = name.to_string_lossy();
            !name.eq_ignore_ascii_case("PATH")
                && !name.starts_with('=')
                && !name.starts_with("RASH_NATIVE_HOST_")
        }) {
            command.env.insert(
                b"RASH_NATIVE_HOST_EXCLUDED_NAME".to_vec(),
                rash::os::to_bytes(&name).unwrap(),
            );
        }
        command
    }

    #[test]
    fn external_capture_environment_and_exit_status() {
        let mut host = NativeHost::new().unwrap();
        let result = host.run_pipeline(&[probe_command()], false, true).unwrap();
        assert_eq!(result.status, 7);
        assert_eq!(result.pipeline, [7]);
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains("probe:spaces & $literal"), "{output}");
        assert!(output.contains("probe:stderr"), "{output}");
        assert!(output.contains("probe:argument:true"), "{output}");
        let has_loader_path = std::env::consts::OS == "cygwin";
        assert!(
            output.contains(&format!("probe:path_present:{has_loader_path}")),
            "{output}"
        );
        assert!(output.contains("probe:inherited_absent:true"), "{output}");
    }

    #[test]
    fn external_redirections_preserve_capture_order() {
        let mut host = NativeHost::new().unwrap();
        let file = TestFile::new();
        let mut command = probe_command();
        command.redirects.push(file.redirect(1));
        let result = host.run_pipeline(&[command], false, true).unwrap();
        assert_eq!(result.status, 7);
        let captured = String::from_utf8(result.stdout).unwrap();
        assert!(captured.contains("probe:stderr"), "{captured}");
        assert!(!captured.contains("probe:spaces"), "{captured}");
        let written = std::fs::read_to_string(&file.0).unwrap();
        assert!(written.contains("probe:spaces"), "{written}");
        assert!(!written.contains("probe:stderr"), "{written}");

        let mut command = probe_command();
        command.redirects.insert(0, file.redirect(1));
        let result = host.run_pipeline(&[command], false, true).unwrap();
        assert_eq!(result.status, 7);
        assert!(result.stdout.is_empty());
        let written = std::fs::read_to_string(&file.0).unwrap();
        assert!(written.contains("probe:spaces"), "{written}");
        assert!(written.contains("probe:stderr"), "{written}");
    }

    #[test]
    #[cfg(windows)]
    fn canonical_path_wins_over_windows_case_variants() {
        let mut host = NativeHost::new().unwrap();
        let mut command = probe_command();
        command.env.insert(b"PATH".to_vec(), b"canonical".to_vec());
        command.env.insert(b"Path".to_vec(), b"stale".to_vec());
        let result = host.run_pipeline(&[command], false, true).unwrap();
        assert_eq!(result.status, 7);
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains("probe:path:canonical"), "{output}");
    }

    #[test]
    fn process_probe() {
        if let Ok(value) = std::env::var("RASH_NATIVE_HOST_PROBE") {
            super::job_tests::child_action();
            println!("probe:{value}");
            println!(
                "probe:argument:{}",
                std::env::args().any(|arg| arg == "argument spaces & $() \"unicode Ω\"")
            );
            println!("probe:path_present:{}", std::env::var_os("PATH").is_some());
            println!("probe:path:{}", std::env::var("PATH").unwrap_or_default());
            println!(
                "probe:inherited_absent:{}",
                std::env::var_os("RASH_NATIVE_HOST_EXCLUDED_NAME")
                    .is_none_or(|name| std::env::var_os(name).is_none())
            );
            eprintln!("probe:stderr");
            std::process::exit(7);
        }
    }

    #[test]
    fn unavailable_commands_return_shell_status() {
        let mut host = NativeHost::new().unwrap();
        let command = Command {
            argv: vec![b"rash-native-nonexistent-command-4a7192".to_vec()],
            display: None,
            env: Default::default(),
            redirects: vec![Redirection {
                fd: 2,
                action: RedirectAction::Dup(1),
            }],
            builtin: false,
        };
        let result = host.run_pipeline(&[command], false, true).unwrap();
        assert_eq!(result.status, 127);
        assert_eq!(result.pipeline, [127]);
        assert!(!result.stdout.is_empty());
    }

    #[test]
    fn unsupported_descriptors_and_jobs_fail_explicitly() {
        let mut host = NativeHost::new().unwrap();
        assert!(host.write(3, b"", &[], false).is_err());
        assert!(host.run_pipeline(&[], true, false).is_err());
        assert!(
            host.set_signal(b"INT", rash::host::SignalDisposition::Trap)
                .is_err()
        );
    }

    #[test]
    #[cfg(unix)]
    fn unix_signal_status_uses_shell_convention() {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(exit_status(std::process::ExitStatus::from_raw(15)), 143);
        assert_eq!(exit_status(std::process::ExitStatus::from_raw(7 << 8)), 7);
        assert_eq!(exit_status(std::process::ExitStatus::from_raw(0)), 0);
    }
}
