//! Bounded Git reference discovery for native completion.
//!
//! Repository discovery and reference reads use gix directly. The caller
//! separately recognizes a revision slot and supplies literal repository-routing
//! options; edited shell source, aliases and completion scripts never run.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// This budget is checked between library calls; synchronous filesystem access
// cannot be forcibly interrupted. No worker thread or child process is left
// running after a query returns.
const QUERY_BUDGET: Duration = Duration::from_millis(500);
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

    pub(super) fn set_environment(&mut self, environment: Vec<(OsString, OsString)>) {
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
        let deadline = Instant::now() + QUERY_BUDGET;
        let Some(routing) = Routing::new(cwd, global_args, &self.environment) else {
            return Vec::new();
        };
        let Some(repository) = routing.open() else {
            return Vec::new();
        };
        // gix's file reference backend cannot read reftable repositories yet.
        // Do not accidentally return partial file refs from a mixed migration.
        if repository
            .config_snapshot()
            .string("extensions.refStorage")
            .is_some_and(|storage| {
                let storage: &[u8] = storage.as_ref();
                storage != b"files"
            })
        {
            return Vec::new();
        }
        let Ok(platform) = repository.references() else {
            return Vec::new();
        };
        let Ok(references) = platform.all() else {
            return Vec::new();
        };
        let mut output = Vec::new();
        let mut complete = true;
        for (index, reference) in references.enumerate() {
            if index >= MAX_REFS || Instant::now() >= deadline {
                complete = false;
                break;
            }
            let Ok(reference) = reference else {
                complete = false;
                continue;
            };
            let name: &[u8] = reference.name().as_bstr().as_ref();
            if name.len() > MAX_REF_BYTES || output.len() + name.len() + 1 > MAX_OUTPUT {
                complete = false;
                break;
            }
            output.extend_from_slice(name);
            output.push(b'\n');
        }
        let mut refs = parse_refs(&output, complete);
        // Resolve symbolic references without peeling tags or reading objects.
        if refs.len() < MAX_REFS && Instant::now() < deadline && resolves_head(&repository) {
            refs.push(GitRef {
                name: "HEAD".into(),
                kind: RefKind::Head,
            });
        }
        refs
    }
}

struct Routing {
    cwd: PathBuf,
    git_dir: Option<PathBuf>,
    ceiling_dirs: Vec<PathBuf>,
    cross_fs: bool,
}

impl Routing {
    fn new(cwd: &Path, arguments: &[String], environment: &[(OsString, OsString)]) -> Option<Self> {
        if !cwd.is_absolute() || !approved_routing(arguments) {
            return None;
        }
        let variable = |key: &str| {
            environment.iter().rev().find_map(|(name, value)| {
                name.to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(key))
                    .then_some(value.as_os_str())
            })
        };
        // These overrides cannot currently be applied to a gix repository
        // opened from a session snapshot. Decline rather than use another
        // repository's refs or consult the process-global environment.
        if variable("GIT_COMMON_DIR").is_some_and(|value| !value.is_empty())
            || variable("GIT_CONFIG_PARAMETERS").is_some_and(|value| !value.is_empty())
            || variable("GIT_CONFIG_COUNT").is_some_and(|value| !value.is_empty() && value != "0")
        {
            return None;
        }
        let mut cwd = gix::path::realpath(cwd).ok()?;
        let mut git_dir = variable("GIT_DIR").map(PathBuf::from);
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "-C" => {
                    let value = arguments.next()?;
                    if !value.is_empty() {
                        cwd = gix::path::realpath(cwd.join(value)).ok()?;
                        if !cwd.is_dir() {
                            return None;
                        }
                    }
                }
                "--git-dir" => git_dir = Some(arguments.next()?.into()),
                // --bare supplies GIT_DIR only if it is unset, capturing the
                // directory at this point. A relative --git-dir instead uses
                // the final -C directory.
                "--bare" => {
                    git_dir.get_or_insert_with(|| cwd.clone());
                }
                // Worktree paths do not select the reference store. Git's
                // local revision commands also ignore server namespaces.
                "--work-tree" | "--namespace" => {
                    arguments.next()?;
                }
                _ => return None,
            }
        }
        if git_dir
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return None;
        }
        let git_dir = git_dir.map(|path| cwd.join(path));
        let ceiling_dirs = variable("GIT_CEILING_DIRECTORIES")
            .map(ceiling_directories)
            .unwrap_or_default();
        let cross_fs =
            variable("GIT_DISCOVERY_ACROSS_FILESYSTEM").map_or(Some(false), |value| {
                let value = value.to_str()?;
                match value.to_ascii_lowercase().as_str() {
                    "" | "false" | "no" | "off" | "0" => Some(false),
                    "true" | "yes" | "on" | "1" => Some(true),
                    _ => value.parse::<i64>().ok().map(|value| value != 0),
                }
            })?;
        Some(Self {
            cwd,
            git_dir,
            ceiling_dirs,
            cross_fs,
        })
    }

    fn open(&self) -> Option<gix::Repository> {
        // In particular, never ask gix to locate Git's installation config:
        // that lookup can itself launch git.exe. Isolated options also prevent
        // inherited GIT_* variables and include files from changing routing.
        let options =
            gix::open::Options::isolated().config_overrides(["gitoxide.objects.noReplace=true"]);
        match &self.git_dir {
            Some(git_dir) => gix::open_opts(git_dir, options.open_path_as_is(true)).ok(),
            None => {
                let (path, trust) = gix::discover::upwards_opts(
                    &self.cwd,
                    gix::discover::upwards::Options {
                        current_dir: Some(&self.cwd),
                        ceiling_dirs: self.ceiling_dirs.clone(),
                        match_ceiling_dir_or_error: false,
                        cross_fs: self.cross_fs,
                        ..Default::default()
                    },
                )
                .ok()?;
                let (git_dir, worktree) = path.into_repository_and_work_tree_directories();
                let root = worktree.as_deref().unwrap_or(&git_dir);
                // This gix version includes the ceiling directory itself in
                // discovery. Git stops before it when starting beneath it.
                // Check the discovered root before opening the repository.
                if self.ceiling_dirs.iter().any(|ceiling| {
                    self.cwd
                        .strip_prefix(ceiling)
                        .is_ok_and(|relative| !relative.as_os_str().is_empty())
                        && ceiling.starts_with(root)
                }) {
                    return None;
                }
                gix::open_opts(git_dir, options.open_path_as_is(true).with(trust)).ok()
            }
        }
    }
}

fn ceiling_directories(value: &OsStr) -> Vec<PathBuf> {
    let mut normalize = true;
    std::env::split_paths(value)
        .filter_map(|path| {
            if path.as_os_str().is_empty() {
                normalize = false;
                return None;
            }
            if !path.is_absolute() {
                return None;
            }
            Some(if normalize {
                gix::path::realpath(&path).unwrap_or(path)
            } else {
                path
            })
        })
        .collect()
}

fn resolves_head(repository: &gix::Repository) -> bool {
    let Ok(mut reference) = repository.find_reference("HEAD") else {
        return false;
    };
    for _ in 0..8 {
        if reference.target().try_id().is_some() {
            return true;
        }
        let Some(Ok(next)) = reference.follow() else {
            return false;
        };
        reference = next;
    }
    false
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

fn parse_refs(output: &[u8], complete: bool) -> Vec<GitRef> {
    let mut parsed = BTreeMap::new();
    let mut full_names = BTreeSet::new();
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
        full_names.insert(full.to_owned());
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
        if direct != *full && full_names.contains(&direct)
            || short != full && full_names.contains(short)
        {
            collisions.insert(short.clone());
        }
    }
    parsed
        .into_iter()
        .map(|(full, (short, kind))| GitRef {
            name: if !complete || collisions.contains(&short) || short.starts_with('-') {
                full
            } else {
                short
            },
            kind,
        })
        .collect()
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
                    "fish-gix-query-{}-{}",
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

        fn repository(&self, name: &str) -> gix::Repository {
            let mut repository = gix::ThreadSafeRepository::init_opts(
                self.path().join(name),
                gix::create::Kind::WithWorktree,
                Default::default(),
                gix::open::Options::isolated(),
            )
            .unwrap()
            .to_thread_local();
            repository.refs.write_reflog = gix::refs::store::WriteReflog::Disable;
            repository
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const OBJECT: &str = "0123456789012345678901234567890123456789";

    fn add_reference(repository: &gix::Repository, name: &str) {
        repository
            .reference(
                name,
                gix::ObjectId::from_hex(OBJECT.as_bytes()).unwrap(),
                gix::refs::transaction::PreviousValue::Any,
                "fixture",
            )
            .unwrap();
    }

    fn names(refs: &[GitRef]) -> BTreeSet<&str> {
        refs.iter()
            .map(|reference| reference.name.as_str())
            .collect()
    }

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parsing_labels_and_disambiguates_refs_without_object_reads() {
        let refs = parse_refs(b"refs/heads/main\nrefs/heads/topic\nrefs/tags/topic\nrefs/remotes/origin/main\nrefs/tags/v1\nrefs/heads/HEAD\nrefs/heads/-flag\n", true);
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
        let refs = parse_refs(b"refs/heads/good\nrefs/heads/good\nrefs/stash\nrefs/heads/\nrefs/tags/a b\nrefs/heads/../bad\nrefs/tags/ref.lock\nrefs/heads/\xff\nrefs/heads/a\x1b[31mb\nrefs/heads/valid\r\n", true);
        assert_eq!(names(&refs), BTreeSet::from(["good", "valid"]));
        assert!(
            parse_refs(
                format!("refs/heads/{}", "x".repeat(MAX_REF_BYTES)).as_bytes(),
                true
            )
            .is_empty()
        );
        assert_eq!(
            parse_refs("refs/heads/topic\n".repeat(MAX_REFS + 10).as_bytes(), true).len(),
            1
        );
    }

    #[test]
    fn namespace_shorthands_and_pseudo_refs_cannot_select_another_revision() {
        let refs = parse_refs(b"refs/heads/tags/v1\nrefs/tags/v1\nrefs/heads/main\nrefs/tags/heads/main\nrefs/heads/ORIG_HEAD\nrefs/heads/remotes/origin/topic\nrefs/remotes/origin/topic\nrefs/heads/refs/heads/main\nrefs/stash\nrefs/heads/stash\n", true);
        assert_eq!(
            names(&refs),
            BTreeSet::from([
                "refs/heads/tags/v1",
                "v1",
                "main",
                "refs/tags/heads/main",
                "refs/heads/ORIG_HEAD",
                "refs/heads/remotes/origin/topic",
                "origin/topic",
                "refs/heads/refs/heads/main",
                "refs/heads/stash",
            ])
        );
        // A truncated list may have hidden collisions, so keep full names.
        assert_eq!(
            names(&parse_refs(b"refs/heads/main\nrefs/tags/v1\n", false)),
            BTreeSet::from(["refs/heads/main", "refs/tags/v1"])
        );
    }

    #[test]
    fn routing_accepts_only_canonical_repository_options() {
        assert!(approved_routing(&arguments(&[
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
        ])));
        for args in [
            vec!["-c", "alias.for-each-ref=!bad"],
            vec!["--config-env=x=y"],
            vec!["reset", "--hard"],
            vec!["-C"],
            vec!["--"],
            vec!["--git-dir", "bad\0path"],
        ] {
            assert!(!approved_routing(&arguments(&args)));
        }
    }

    #[test]
    fn discovery_reads_refs_without_git_on_path_or_reading_objects() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        for name in [
            "refs/heads/main",
            "refs/heads/feature/é",
            "refs/remotes/origin/main",
            "refs/tags/v1",
        ] {
            add_reference(&repository, name);
        }
        let cwd = repository.workdir().unwrap().join("nested");
        std::fs::create_dir(&cwd).unwrap();
        let mut git = GitRefs::new();
        git.set_environment(vec![("PATH".into(), "".into())]);
        assert_eq!(
            names(&git.query(&cwd, &[])),
            BTreeSet::from(["main", "feature/é", "origin/main", "v1", "HEAD"])
        );
        assert!(!repository.git_dir().join("objects/01").exists());
    }

    #[test]
    fn literal_routing_observes_repeated_cwd_and_bare_option_order() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        add_reference(&repository, "refs/heads/topic");
        let cwd = temporary.path();
        let git = GitRefs::new();
        for args in [
            arguments(&["-C", "repo", "-C", "", "-C", ".git"]),
            arguments(&["--git-dir", ".", "-C", "repo/.git"]),
            arguments(&["--git-dir", "repo/.git", "--bare"]),
            arguments(&["-C", "repo/.git", "--bare"]),
            arguments(&[
                "--git-dir",
                "repo/.git",
                "--work-tree",
                "missing",
                "--namespace",
                "server",
            ]),
        ] {
            assert!(names(&git.query(cwd, &args)).contains("topic"), "{args:?}");
        }
        assert!(
            git.query(cwd, &arguments(&["--bare", "-C", "repo/.git"]))
                .is_empty()
        );
        assert!(git.query(cwd, &arguments(&["--git-dir", ""])).is_empty());
        assert!(git.query(cwd, &arguments(&["-C", "missing"])).is_empty());
    }

    #[test]
    fn exported_repository_overrides_are_explicit_and_do_not_need_process_environment() {
        let temporary = TemporaryDirectory::new();
        let first = temporary.repository("first");
        let second = temporary.repository("second");
        add_reference(&first, "refs/heads/first");
        add_reference(&second, "refs/heads/second");
        let mut git = GitRefs::new();
        git.set_environment(vec![
            ("git_dir".into(), "second/.git".into()),
            ("GIT_WORK_TREE".into(), "missing".into()),
            ("GIT_NAMESPACE".into(), "server".into()),
        ]);
        assert_eq!(
            names(&git.query(temporary.path(), &[])),
            BTreeSet::from(["second"])
        );
        assert_eq!(
            names(&git.query(temporary.path(), &arguments(&["--bare"]))),
            BTreeSet::from(["second"])
        );
        assert_eq!(
            names(&git.query(temporary.path(), &arguments(&["--git-dir", "first/.git"]))),
            BTreeSet::from(["first"])
        );
        for (key, value) in [
            ("GIT_COMMON_DIR", "second/.git"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_PARAMETERS", "core.bare=true"),
        ] {
            git.set_environment(vec![(key.into(), value.into())]);
            assert!(git.query(first.workdir().unwrap(), &[]).is_empty());
        }
    }

    #[test]
    fn exported_ceiling_directories_stop_upward_discovery() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        add_reference(&repository, "refs/heads/main");
        let cwd = repository.workdir().unwrap().join("nested");
        std::fs::create_dir(&cwd).unwrap();
        let mut git = GitRefs::new();
        assert!(names(&git.query(&cwd, &[])).contains("main"));
        git.set_environment(vec![(
            "GIT_CEILING_DIRECTORIES".into(),
            repository.workdir().unwrap().into(),
        )]);
        assert!(git.query(&cwd, &[]).is_empty());
        assert!(
            git.query(temporary.path(), &arguments(&["-C", "repo/nested"]))
                .is_empty()
        );
        // Explicit --git-dir bypasses discovery ceilings, as Git does.
        assert!(names(&git.query(&cwd, &arguments(&["--git-dir", "../.git"]))).contains("main"));
    }

    #[test]
    fn packed_refs_and_linked_worktrees_share_branches_but_keep_their_own_head() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        std::fs::write(repository.git_dir().join("packed-refs"), format!("# pack-refs with: peeled fully-peeled sorted\n{OBJECT} refs/heads/main\n{OBJECT} refs/heads/other\n{OBJECT} refs/tags/v1\n")).unwrap();
        let metadata = repository.git_dir().join("worktrees/linked");
        let worktree = temporary.path().join("linked");
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(metadata.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        std::fs::write(metadata.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            metadata.join("gitdir"),
            format!("{}\n", worktree.join(".git").display()),
        )
        .unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", metadata.display()),
        )
        .unwrap();
        let git = GitRefs::new();
        assert_eq!(
            names(&git.query(&worktree, &[])),
            BTreeSet::from(["main", "other", "v1", "HEAD"])
        );
        std::fs::write(metadata.join("HEAD"), "ref: refs/heads/unborn\n").unwrap();
        assert!(!names(&GitRefs::new().query(&worktree, &[])).contains("HEAD"));
        assert!(names(&git.query(repository.workdir().unwrap(), &[])).contains("HEAD"));
    }

    #[test]
    fn head_resolution_handles_detached_chained_and_cyclic_symbolic_refs() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        assert!(!resolves_head(&repository));
        std::fs::write(repository.git_dir().join("HEAD"), format!("{OBJECT}\n")).unwrap();
        assert!(resolves_head(&repository));
        add_reference(&repository, "refs/heads/main");
        std::fs::write(repository.git_dir().join("HEAD"), "ref: refs/heads/alias\n").unwrap();
        std::fs::write(
            repository.git_dir().join("refs/heads/alias"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        assert!(resolves_head(&repository));
        std::fs::write(
            repository.git_dir().join("refs/heads/alias"),
            "ref: refs/heads/alias\n",
        )
        .unwrap();
        assert!(!resolves_head(&repository));
    }

    #[test]
    fn unsupported_ref_storage_declines_without_falling_back_to_file_refs() {
        use std::io::Write as _;
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        add_reference(&repository, "refs/heads/main");
        std::fs::OpenOptions::new()
            .append(true)
            .open(repository.git_dir().join("config"))
            .unwrap()
            .write_all(b"\n[extensions]\n\trefStorage = reftable\n")
            .unwrap();
        assert!(
            GitRefs::new()
                .query(repository.workdir().unwrap(), &[])
                .is_empty()
        );
    }

    #[test]
    fn large_ref_lists_are_bounded_and_use_unambiguous_full_names() {
        let temporary = TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        let mut packed = String::from("# pack-refs with: peeled fully-peeled sorted\n");
        for index in 0..MAX_REFS + 10 {
            use std::fmt::Write as _;
            writeln!(packed, "{OBJECT} refs/heads/branch-{index:05}").unwrap();
        }
        std::fs::write(repository.git_dir().join("packed-refs"), packed).unwrap();
        let refs = GitRefs::new().query(repository.workdir().unwrap(), &[]);
        assert_eq!(refs.len(), MAX_REFS);
        assert!(
            refs.iter()
                .all(|reference| reference.name.starts_with("refs/heads/"))
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
}
