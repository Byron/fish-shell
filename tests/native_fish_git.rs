//! Native adapters for the same gix queries used by the POSIX script helpers.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "fish-native-gix-{}-{} space 文",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create native Git fixture: {error}"),
            }
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .arg("-N")
            .current_dir(&self.0)
            .env("PATH", "")
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("profile"))
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("GIT_CONFIG_GLOBAL", "")
            .env("GIT_CONFIG_SYSTEM", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        command
    }

    #[cfg(feature = "git")]
    fn repository(&self) -> gix::Repository {
        let mut repository = gix::ThreadSafeRepository::init_opts(
            &self.0,
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        let Ok(root) = self.0.canonicalize() else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-gix-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn assert_output(output: Output, expected: &[u8]) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, expected);
}

#[test]
fn private_git_builtin_reports_feature_availability() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args([
            "-c",
            "type -t __fish_git; builtin __fish_git --is-enabled; echo enabled:$status",
        ])
        .output()
        .unwrap();
    assert_output(
        output,
        if cfg!(feature = "git") {
            b"builtin\nenabled:0\n"
        } else {
            b"builtin\nenabled:1\n"
        },
    );
}

#[cfg(feature = "git")]
#[test]
fn repository_history_status_and_config_queries_work_without_git_on_path() {
    use std::io::Write as _;

    let fixture = Fixture::new();
    let repository = fixture.repository();
    let blob = repository.write_blob(b"original\n").unwrap().detach();
    let tree = repository
        .write_object(&gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: "tracked.txt".into(),
                oid: blob,
            }],
        })
        .unwrap()
        .detach();
    repository
        .index_from_tree(&tree)
        .unwrap()
        .write(Default::default())
        .unwrap();
    let signature = gix::actor::SignatureRef {
        name: "Native fixture".into(),
        email: "fish@example.invalid".into(),
        time: "1000000000 +0000",
    };
    let commit = repository
        .commit_as(
            signature,
            signature,
            "HEAD",
            "fixture 文\n",
            tree,
            None::<gix::ObjectId>,
        )
        .unwrap()
        .detach();
    std::fs::write(fixture.0.join("tracked.txt"), "modified\n").unwrap();
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(repository.git_dir().join("config"))
        .unwrap();
    writeln!(config, "[alias]\n short = status --short").unwrap();
    drop(config);

    let output = fixture
        .command()
        .args([
            "-c",
            "__fish_git rev-parse HEAD; __fish_git log -1 '--grep=^fixture 文$'; __fish_git status --porcelain -z; __fish_git config --get-regexp '^ALIAS\\.short$'",
        ])
        .output()
        .unwrap();
    assert_output(
        output,
        format!("{commit}\nfixture 文\n M tracked.txt\0alias.short status --short\n").as_bytes(),
    );
}

#[cfg(feature = "git")]
#[test]
fn snapshot_overrides_and_errors_remain_shell_command_statuses() {
    let fixture = Fixture::new();
    let _repository = fixture.repository();
    let output = fixture
        .command()
        .args([
            "-c",
            "set -gx git_config_count 1; set -gx git_config_key_0 alias.snapshot; set -gx git_config_value_0 session; __fish_git config alias.snapshot; __fish_git reset --hard; echo invalid:$status; __fish_git config missing.key; echo missing:$status; __fish_git_query help --config; echo unavailable:$status; echo continued",
        ])
        .output()
        .unwrap();
    assert_output(
        output,
        b"session\ninvalid:2\nmissing:1\nunavailable:1\ncontinued\n",
    );
}

#[cfg(feature = "git")]
#[test]
fn unsupported_overrides_request_git_but_storage_errors_do_not_retry() {
    use std::io::Write as _;

    let fixture = Fixture::new();
    let repository = fixture.repository();
    for variable in [
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        let output = fixture
            .command()
            .env(variable, "unsupported")
            .args([
                "-c",
                "__fish_git rev-parse --git-dir; echo builtin:$status; __fish_git_query rev-parse --git-dir; echo helper:$status",
            ])
            .output()
            .unwrap();
        assert_output(output, b"builtin:125\nhelper:1\n");
    }
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(repository.git_dir().join("config"))
        .unwrap();
    writeln!(config, "[extensions]\n refStorage = reftable").unwrap();
    drop(config);
    let output = fixture
        .command()
        .args([
            "-c",
            "__fish_git_query rev-parse --git-dir; echo unsupported:$status",
        ])
        .output()
        .unwrap();
    assert_output(output, b"unsupported:1\n");
}

#[cfg(not(feature = "git"))]
#[test]
fn disabled_git_queries_request_git_and_handle_its_absence() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args([
            "-c",
            "builtin __fish_git rev-parse HEAD; echo builtin:$status; __fish_git_query rev-parse HEAD; echo helper:$status; echo continued",
        ])
        .output()
        .unwrap();
    assert_output(output, b"builtin:125\nhelper:1\ncontinued\n");
}

#[test]
fn shared_helper_uses_git_when_a_query_requires_it() {
    let Some(git_directory) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find(|directory| directory.join("git.exe").is_file())
    }) else {
        // The missing-executable path is covered independently above.
        return;
    };
    let fixture = Fixture::new();
    #[cfg(feature = "git")]
    let _repository = fixture.repository();
    #[cfg(not(feature = "git"))]
    {
        std::fs::create_dir_all(fixture.0.join(".git/objects")).unwrap();
        std::fs::create_dir_all(fixture.0.join(".git/refs/heads")).unwrap();
        std::fs::write(fixture.0.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            fixture.0.join(".git/config"),
            "[core]\n repositoryformatversion = 0\n bare = false\n",
        )
        .unwrap();
    }
    let output = fixture
        .command()
        .env("PATH", git_directory)
        .env("GIT_OBJECT_DIRECTORY", fixture.0.join(".git/objects"))
        .args([
            "-c",
            "command -sq git; echo found:$status; __fish_git_query rev-parse --is-inside-work-tree; echo fallback:$status",
        ])
        .output()
        .unwrap();
    assert_output(output, b"found:0\ntrue\nfallback:0\n");
}
