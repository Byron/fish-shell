//! Bounded Git reference discovery for native completion.
//!
//! Only fixed plumbing commands reach Git. The caller separately recognizes a
//! revision slot and supplies literal repository-routing options; edited shell
//! source, aliases, completion scripts and user-selected executables never run.

use super::host::CompletionProcessGroup;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::Read as _;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
};

const QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const CACHE_AGE: Duration = Duration::from_millis(500);
const MAX_CACHE_ENTRIES: usize = 8;
const MAX_REFS: usize = 4096;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_REF_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    Head,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitRef {
    pub(super) name: String,
    pub(super) kind: RefKind,
}

struct CachedRefs {
    cwd: PathBuf,
    global_args: Vec<String>,
    recorded: Instant,
    refs: Vec<GitRef>,
}

#[derive(Default)]
pub(super) struct GitRefs {
    environment: Vec<(OsString, OsString)>,
    cache: Mutex<VecDeque<CachedRefs>>,
}

impl GitRefs {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn set_environment(&mut self, mut environment: Vec<(OsString, OsString)>) {
        // Match NativeHost command execution: canonical PATH wins over Windows aliases.
        if environment.iter().any(|(name, _)| name == "PATH") {
            environment.retain(|(name, _)| {
                name == "PATH"
                    || !name
                        .to_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case("PATH"))
            });
        }
        self.environment = environment;
        self.cache = Mutex::default();
    }

    pub(super) fn query(&self, cwd: &Path, global_args: &[String]) -> Vec<GitRef> {
        if !approved_routing(global_args) {
            return Vec::new();
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|entry| entry.recorded.elapsed() < CACHE_AGE);
            if let Some(entry) = cache
                .iter()
                .find(|entry| entry.cwd == cwd && entry.global_args == global_args)
            {
                return entry.refs.clone();
            }
        }
        let refs = self.discover(cwd, global_args);
        if let Ok(mut cache) = self.cache.lock() {
            while cache.len() >= MAX_CACHE_ENTRIES {
                cache.pop_front();
            }
            cache.push_back(CachedRefs {
                cwd: cwd.into(),
                global_args: global_args.into(),
                recorded: Instant::now(),
                refs: refs.clone(),
            });
        }
        refs
    }

    fn discover(&self, cwd: &Path, global_args: &[String]) -> Vec<GitRef> {
        let deadline = Instant::now() + QUERY_TIMEOUT;
        let Some(executable) = find_git(&self.environment, cwd) else {
            return Vec::new();
        };
        let command = |arguments: &[&str]| {
            let mut command = Command::new(&executable);
            command
                .current_dir(cwd)
                .env_clear()
                .envs(
                    self.environment
                        .iter()
                        .filter(|(name, _)| {
                            // Trace settings can write files even for a read-only
                            // command. Completion needs neither tracing nor a pager.
                            !name.to_str().is_some_and(|name| {
                                name.to_ascii_uppercase().starts_with("GIT_TRACE")
                            })
                        })
                        .map(|(name, value)| (name, value)),
                )
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_NO_LAZY_FETCH", "1")
                .args(["--no-pager", "--no-optional-locks"])
                .args(global_args)
                .args(arguments);
            command
        };
        let count = format!("--count={MAX_REFS}");
        let Some(output) = capture(
            command(&[
                "for-each-ref",
                "--format=%(refname)",
                &count,
                "refs/heads/",
                "refs/remotes/",
                "refs/tags/",
            ]),
            deadline,
            MAX_OUTPUT,
        ) else {
            return Vec::new();
        };
        let mut refs = parse_refs(&output);
        // for-each-ref succeeds in an unborn repository too. Verify HEAD by
        // resolving its name, without peeling or asking Git to read an object.
        if capture(
            command(&["rev-parse", "--verify", "--quiet", "HEAD"]),
            deadline,
            128,
        )
        .is_some_and(|output| valid_object_name(&output))
        {
            refs.push(GitRef {
                name: "HEAD".into(),
                kind: RefKind::Head,
            });
        }
        refs
    }
}

/// A second boundary prevents an accidental caller from appending a command,
/// configuration injection, or a flag that changes the fixed query's behavior.
fn approved_routing(arguments: &[String]) -> bool {
    if arguments.len() > 64
        || arguments.iter().map(String::len).sum::<usize>() > 32 * 1024
        || arguments.iter().any(|arg| arg.contains('\0'))
    {
        return false;
    }
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bare" => {}
            "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                if arguments.next().is_none() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn find_git(environment: &[(OsString, OsString)], cwd: &Path) -> Option<PathBuf> {
    let path = environment.iter().rev().find_map(|(name, value)| {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("PATH"))
            .then_some(value)
    })?;
    // PATH explicitly containing a relative directory (including ".") is
    // honored, but Windows' implicit current-directory executable search is
    // not used. Batch wrappers must never introduce another shell evaluator.
    for directory in std::env::split_paths(path).filter(|path| !path.as_os_str().is_empty()) {
        let candidate = cwd.join(directory).join("git.exe");
        if candidate.is_absolute() && candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn parse_refs(output: &[u8]) -> Vec<GitRef> {
    let mut parsed = BTreeMap::new();
    for line in output.split(|byte| *byte == b'\n').take(MAX_REFS) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Ok(full) = std::str::from_utf8(line) else {
            continue;
        };
        if full.len() > MAX_REF_BYTES
            || full.chars().any(|ch| ch.is_control())
            || full.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
            || full.contains("..")
            || full.contains("@{")
            || full.ends_with(['/', '.'])
            || full
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
        {
            continue;
        }
        let Some((short, kind)) = [
            ("refs/heads/", RefKind::LocalBranch),
            ("refs/remotes/", RefKind::RemoteBranch),
            ("refs/tags/", RefKind::Tag),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| full.strip_prefix(prefix).map(|short| (short, kind))) else {
            continue;
        };
        if !short.is_empty() {
            parsed.insert(full.to_owned(), (short.to_owned(), kind));
        }
    }
    let mut seen = BTreeSet::new();
    let mut collisions: BTreeSet<String> = [
        "HEAD",
        "FETCH_HEAD",
        "ORIG_HEAD",
        "MERGE_HEAD",
        "REBASE_HEAD",
        "REVERT_HEAD",
        "CHERRY_PICK_HEAD",
        "BISECT_HEAD",
        "AUTO_MERGE",
        "MERGE_AUTOSTASH",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for (full, (short, _)) in &parsed {
        if !seen.insert(short.clone()) {
            collisions.insert(short.clone());
        }
        // Git resolves refs/<name> before trying refs/heads/<name> or
        // refs/tags/<name>. A branch named tags/v1 must not select the tag v1.
        let direct = format!("refs/{short}");
        if direct != *full && parsed.contains_key(&direct)
            || short != full && parsed.contains_key(short)
        {
            collisions.insert(short.clone());
        }
    }
    parsed
        .into_iter()
        .map(|(full, (short, kind))| GitRef {
            name: if collisions.contains(&short) || short.starts_with('-') {
                full
            } else {
                short
            },
            kind,
        })
        .collect()
}

fn valid_object_name(output: &[u8]) -> bool {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    let output = output.strip_suffix(b"\r").unwrap_or(output);
    matches!(output.len(), 40 | 64) && output.iter().all(u8::is_ascii_hexdigit)
}

struct QueryProcess {
    child: Child,
    group: CompletionProcessGroup,
}

impl Drop for QueryProcess {
    fn drop(&mut self) {
        // Kill descendants too: otherwise an inherited stdout pipe could keep
        // a timed-out completion alive after Git's original process exits.
        let _ = self.group.terminate(1);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn capture(mut command: Command, deadline: Instant, limit: usize) -> Option<Vec<u8>> {
    if Instant::now() >= deadline {
        return None;
    }
    let group = CompletionProcessGroup::new(true).ok()?;
    group.configure(&mut command);
    command
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = command.spawn().ok()?;
    let mut process = QueryProcess { child, group };
    process.group.add(&process.child).ok()?;
    process.group.start(&process.child).ok()?;
    let mut reader = process.child.stdout.take()?;
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let mut available = 0;
        let success = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if success == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(ERROR_BROKEN_PIPE as i32)
        {
            return None;
        }
        if available > 0 {
            let size = (available as usize).min(buffer.len());
            let read = reader.read(&mut buffer[..size]).ok()?;
            if read == 0 || output.len().saturating_add(read) > limit {
                return None;
            }
            output.extend_from_slice(&buffer[..read]);
            continue;
        }
        if let Some(status) = process.child.try_wait().ok()? {
            return status.success().then_some(output);
        }
        std::thread::sleep(
            Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "fish-git-query-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create private fixture directory: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parsing_labels_and_disambiguates_refs_without_object_reads() {
        let refs = parse_refs(b"refs/heads/main\nrefs/heads/topic\nrefs/tags/topic\nrefs/remotes/origin/main\nrefs/tags/v1\nrefs/heads/HEAD\nrefs/heads/-flag\n");
        for (name, kind) in [
            ("main", RefKind::LocalBranch),
            ("refs/heads/topic", RefKind::LocalBranch),
            ("refs/tags/topic", RefKind::Tag),
            ("origin/main", RefKind::RemoteBranch),
            ("v1", RefKind::Tag),
            ("refs/heads/HEAD", RefKind::LocalBranch),
            ("refs/heads/-flag", RefKind::LocalBranch),
        ] {
            assert!(
                refs.contains(&GitRef {
                    name: name.into(),
                    kind
                }),
                "{refs:?}"
            );
        }
        assert_eq!(refs.len(), 7);
    }

    #[test]
    fn malformed_ref_output_and_unrelated_namespaces_are_ignored() {
        let refs = parse_refs(b"refs/heads/good\nrefs/heads/good\nrefs/stash\nrefs/heads/\nrefs/tags/a b\nrefs/heads/../bad\nrefs/tags/ref.lock\nrefs/heads/\xff\nrefs/heads/a\x1b[31mb\nrefs/heads/valid\r\n");
        assert_eq!(
            refs.iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            ["good", "valid"]
        );
        assert!(
            parse_refs(format!("refs/heads/{}", "x".repeat(MAX_REF_BYTES)).as_bytes()).is_empty()
        );
        assert_eq!(
            parse_refs("refs/heads/topic\n".repeat(MAX_REFS + 10).as_bytes()).len(),
            1
        );
        assert!(valid_object_name(
            b"0123456789012345678901234567890123456789\n"
        ));
        assert!(valid_object_name("a".repeat(64).as_bytes()));
        assert!(!valid_object_name(b"HEAD\n"));
    }

    #[test]
    fn namespace_shorthands_and_pseudo_refs_cannot_select_another_revision() {
        let refs = parse_refs(b"refs/heads/tags/v1\nrefs/tags/v1\nrefs/heads/main\nrefs/tags/heads/main\nrefs/heads/ORIG_HEAD\nrefs/heads/remotes/origin/topic\nrefs/remotes/origin/topic\nrefs/heads/refs/heads/main\n");
        let names: BTreeSet<_> = refs
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "refs/heads/tags/v1",
                "v1",
                "main",
                "refs/tags/heads/main",
                "refs/heads/ORIG_HEAD",
                "refs/heads/remotes/origin/topic",
                "origin/topic",
                "refs/heads/refs/heads/main",
            ])
        );
    }

    #[test]
    fn routing_accepts_only_canonical_repository_options() {
        let arguments = [
            "-C",
            "repo with spaces",
            "-C",
            "",
            "--git-dir",
            "../metadata",
            "--work-tree",
            "tree",
            "--namespace",
            "topic",
            "--bare",
        ]
        .map(String::from);
        assert!(approved_routing(&arguments));
        for arguments in [
            vec!["-c", "alias.for-each-ref=!bad"],
            vec!["--config-env=x=y"],
            vec!["reset", "--hard"],
            vec!["-C"],
            vec!["--"],
            vec!["--git-dir", "bad\0path"],
        ] {
            assert!(!approved_routing(
                &arguments.into_iter().map(String::from).collect::<Vec<_>>()
            ));
        }
    }

    #[test]
    fn executable_lookup_uses_snapshot_path_without_implicit_cwd_or_wrappers() {
        let temporary = TemporaryDirectory::new();
        let cwd = temporary.path().join("cwd");
        let tools = temporary.path().join("tools");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(cwd.join("git.exe"), []).unwrap();
        std::fs::write(tools.join("git.cmd"), []).unwrap();
        let environment = vec![(OsString::from("pAtH"), tools.as_os_str().to_owned())];
        assert_eq!(find_git(&environment, &cwd), None);
        std::fs::write(tools.join("git.exe"), []).unwrap();
        assert_eq!(find_git(&environment, &cwd), Some(tools.join("git.exe")));
        assert_eq!(find_git(&[], &cwd), None);
        assert_eq!(find_git(&[("PATH".into(), "".into())], &cwd), None);
        assert_eq!(
            find_git(&[("PATH".into(), ".".into())], &cwd),
            Some(cwd.join(".").join("git.exe"))
        );
    }

    #[test]
    fn canonical_path_takes_precedence_over_other_windows_spellings() {
        let temporary = TemporaryDirectory::new();
        let preferred = temporary.path().join("preferred");
        let shadow = temporary.path().join("shadow");
        std::fs::create_dir(&preferred).unwrap();
        std::fs::create_dir(&shadow).unwrap();
        std::fs::write(preferred.join("git.exe"), []).unwrap();
        std::fs::write(shadow.join("git.exe"), []).unwrap();
        let mut git = GitRefs::new();
        git.set_environment(vec![
            ("Path".into(), shadow.as_os_str().to_owned()),
            ("PATH".into(), preferred.as_os_str().to_owned()),
            ("pAtH".into(), shadow.as_os_str().to_owned()),
            ("KEEP".into(), "value".into()),
        ]);
        assert_eq!(
            find_git(&git.environment, temporary.path()),
            Some(preferred.join("git.exe"))
        );
        assert_eq!(
            git.environment,
            vec![
                ("PATH".into(), preferred.as_os_str().to_owned()),
                ("KEEP".into(), "value".into()),
            ]
        );

        let environment = vec![("pAtH".into(), shadow.as_os_str().to_owned())];
        git.set_environment(environment.clone());
        assert_eq!(git.environment, environment);
        assert_eq!(
            find_git(&git.environment, temporary.path()),
            Some(shadow.join("git.exe"))
        );
    }

    #[test]
    fn environment_updates_invalidate_cached_results_and_failures() {
        let mut git = GitRefs::new();
        git.cache.lock().unwrap().push_back(CachedRefs {
            cwd: PathBuf::from("C:/fixture"),
            global_args: Vec::new(),
            recorded: Instant::now(),
            refs: vec![GitRef {
                name: "cached".into(),
                kind: RefKind::LocalBranch,
            }],
        });
        assert_eq!(git.query(Path::new("C:/fixture"), &[])[0].name, "cached");
        git.set_environment(Vec::new());
        assert!(git.cache.lock().unwrap().is_empty());
        assert!(git.query(Path::new("C:/fixture"), &[]).is_empty());
    }

    #[test]
    #[ignore = "subprocess fixture for the bounded query tests"]
    fn query_process_fixture() {
        let Some(mode) = std::env::var_os("FISH_GIT_QUERY_FIXTURE") else {
            return;
        };
        if mode == "flood" {
            use std::io::Write as _;
            let mut output = std::io::stdout().lock();
            loop {
                output.write_all(&[b'x'; 4096]).unwrap();
                output.flush().unwrap();
            }
        }
        if mode == "child" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        let marker = PathBuf::from(std::env::var_os("FISH_GIT_QUERY_MARKER").unwrap());
        let mut child = fixture("child").spawn().unwrap();
        std::fs::write(marker, format!("{} {}", std::process::id(), child.id())).unwrap();
        std::thread::sleep(Duration::from_secs(30));
        let _ = child.wait();
    }

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "native::interactive_git::tests::query_process_fixture",
                "--nocapture",
            ])
            .env("FISH_GIT_QUERY_FIXTURE", mode)
            .creation_flags(CREATE_NO_WINDOW);
        if mode == "child" {
            // The child has no fixture output file of its own; its PID is
            // recorded by its parent before both wait for cancellation.
            command.env_remove("FISH_GIT_QUERY_MARKER");
        }
        command
    }

    #[test]
    fn query_output_and_time_are_bounded_and_processes_are_reaped() {
        let started = Instant::now();
        assert!(capture(fixture("flood"), started + Duration::from_secs(3), 1024).is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
        let temporary = TemporaryDirectory::new();
        let marker = temporary.path().join("children");
        let mut command = fixture("wait");
        command.env("FISH_GIT_QUERY_MARKER", &marker);
        let started = Instant::now();
        assert!(capture(command, started + Duration::from_secs(1), 1024 * 1024).is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
        let pids = std::fs::read_to_string(marker).expect("fixture process started");
        for pid in pids
            .split_whitespace()
            .map(|pid| pid.parse::<u32>().unwrap())
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if !handle.is_null() {
                let mut status = 259;
                let result = unsafe { GetExitCodeProcess(handle, &mut status) };
                unsafe {
                    CloseHandle(handle);
                }
                assert_ne!(result, 0);
                assert_ne!(status, 259, "completion left process {pid} running");
            }
        }
    }
}
