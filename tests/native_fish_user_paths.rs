//! Shared user PATH reconstruction and fish_add_path through native startup.
#![cfg(windows)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "fish-native-user-paths-{}-{} space 文",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create user paths fixture: {error}"),
            }
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .current_dir(&self.0)
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("FISH_TEST_EXE", env!("CARGO_BIN_EXE_fish"))
            .env("PATH", r"C:\base;D:\base")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, source: &str) -> String {
        checked(self.command().args(["-c", source]).output().unwrap())
    }

    fn config(&self, name: &str, source: &str) {
        let path = self.0.join("config/fish").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().replace('/', "\\")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("fish-native-user-paths-")
            })
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn checked(output: Output) -> String {
    assert!(
        output.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn saved_paths_are_applied_before_snippets_and_config() {
    let fixture = Fixture::new();
    fixture.run(r"set -U fish_user_paths 'C:\saved' 'D:\saved'");
    fixture.config("conf.d/paths.fish", "echo snippet:(string join , -- $PATH)");
    fixture.config("config.fish", "echo config:(string join , -- $PATH)");
    assert_eq!(
        fixture.run("echo command:(string join , -- $PATH)"),
        concat!(
            "snippet:C:\\saved,D:\\saved,C:\\base,D:\\base\n",
            "config:C:\\saved,D:\\saved,C:\\base,D:\\base\n",
            "command:C:\\saved,D:\\saved,C:\\base,D:\\base\n",
        ),
    );
}

#[test]
fn duplicate_changes_replace_only_owned_path_entries() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g fish_user_paths 'C:\new' 'D:\base' 'C:\new'
            echo users:(string join , -- $fish_user_paths)
            echo added:(string join , -- $PATH)
            set fish_user_paths 'E:\later'
            echo replaced:(string join , -- $PATH)
            set -eg fish_user_paths
            echo erased:(string join , -- $PATH)
        "#
        ),
        concat!(
            "users:C:\\new,D:\\base\n",
            "added:C:\\new,D:\\base,C:\\base\n",
            "replaced:E:\\later,D:\\base,C:\\base\n",
            "erased:D:\\base,C:\\base\n",
        ),
    );
}

#[test]
fn native_separators_preserve_drive_colons_for_path_and_ordinary_variables() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g --path fish_user_paths 'C:\one;D:\two'
            echo split:(string join , -- $PATH)
            set -g --unpath fish_user_paths 'E:\three' 'F:\four'
            echo ordinary:(string join , -- $PATH)
        "#
        ),
        concat!(
            "split:C:\\one,D:\\two,C:\\base,D:\\base\n",
            "ordinary:E:\\three,F:\\four,C:\\base,D:\\base\n",
        ),
    );
}

#[test]
fn global_shadows_and_remote_universal_changes_reconstruct_visible_paths() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -U fish_user_paths 'C:\universal'
            set -g fish_user_paths 'D:\global'
            $FISH_TEST_EXE -c "set -U fish_user_paths 'E:\remote'"
            echo shadowed:(string join , -- $PATH)
            set -eg fish_user_paths
            echo exposed:(string join , -- $PATH)
            $FISH_TEST_EXE -c 'set -eU fish_user_paths'
            echo removed:(string join , -- $PATH)
        "#
        ),
        concat!(
            "shadowed:D:\\global,C:\\base,D:\\base\n",
            "exposed:E:\\remote,C:\\base,D:\\base\n",
            "removed:C:\\base,D:\\base\n",
        ),
    );
}

#[test]
fn function_path_changes_do_not_unregister_the_startup_handler() {
    let fixture = Fixture::new();
    fixture.config("config.fish", "set -g fish_function_path $PWD/elsewhere");
    assert_eq!(
        fixture.run(r"set -g fish_user_paths 'C:\first'; set -g fish_function_path; set fish_user_paths 'D:\next'; echo (string join , -- $PATH)"),
        "D:\\next,C:\\base,D:\\base\n",
    );
}

#[test]
fn startup_works_without_a_config_home_and_no_config_skips_reconstruction() {
    let fixture = Fixture::new();
    let mut command = fixture.command();
    command
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("FISH_NATIVE_CONFIG_HOME");
    assert_eq!(
        checked(
            command
                .args([
                    "-c",
                    r"set -g fish_user_paths 'C:\user'; echo (string join , -- $PATH)"
                ])
                .output()
                .unwrap()
        ),
        "C:\\user,C:\\base,D:\\base\n",
    );
    assert_eq!(
        checked(
            fixture
                .command()
                .args([
                    "-N",
                    "-c",
                    r"set -g fish_user_paths 'C:\user'; echo (string join , -- $PATH)"
                ])
                .output()
                .unwrap()
        ),
        "C:\\base,D:\\base\n",
    );
    fixture.config("config.fish", "echo must-not-run");
    assert_eq!(
        checked(
            fixture
                .command()
                .args(["-n", "-c", "echo valid"])
                .output()
                .unwrap()
        ),
        "",
    );
}

#[test]
fn empty_initial_paths_do_not_consume_process_stdin() {
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .args(["-c", "read payload; echo payload:$payload"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"untouched\n")
        .unwrap();
    assert_eq!(
        checked(child.wait_with_output().unwrap()),
        "payload:untouched\n"
    );
}

#[test]
fn bundled_fish_add_path_updates_saved_order_and_honors_dry_run() {
    let fixture = Fixture::new();
    for directory in ["first dir", "second", "third"] {
        std::fs::create_dir(fixture.0.join(directory)).unwrap();
    }
    assert_eq!(
        fixture.run(
            r#"
            fish_add_path 'first dir' second
            echo add:$status
            fish_add_path --append third
            echo append:$status
            fish_add_path --move second
            echo move:$status
            printf 'saved:%s\n' $fish_user_paths
            set -l plan (fish_add_path --dry-run --global --move 'first dir')
            echo dry:$status
            set -qg fish_user_paths
            echo global:$status
            printf 'before:%s\n' $fish_user_paths
            eval $plan
            printf 'after:%s\n' $fish_user_paths
        "#
        ),
        format!(
            "add:0\nappend:0\nmove:0\nsaved:{second}\nsaved:{first}\nsaved:{third}\ndry:0\nglobal:1\nbefore:{second}\nbefore:{first}\nbefore:{third}\nafter:{first}\nafter:{second}\nafter:{third}\n",
            first = fixture.path("first dir"),
            second = fixture.path("second"),
            third = fixture.path("third"),
        ),
    );
    assert_eq!(
        fixture.run("printf 'persisted:%s\\n' $PATH"),
        format!(
            "persisted:{}\npersisted:{}\npersisted:{}\npersisted:C:\\base\npersisted:D:\\base\n",
            fixture.path("second"),
            fixture.path("first dir"),
            fixture.path("third")
        ),
    );
}

#[test]
fn bundled_fish_add_path_verbose_skips_and_no_op_use_untranslated_messages() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join("included")).unwrap();
    std::fs::write(fixture.0.join("file"), "file").unwrap();
    assert_eq!(
        fixture.run(
            r#"
            fish_add_path included
            fish_add_path --verbose included file missing
            echo skipped:$status
            fish_add_path --verbose
            echo empty:$status
        "#
        ),
        format!(
            "Skipping already included path: {}\nSkipping path because it is a file instead of a directory: {}\nSkipping non-existent path: {}\nNo paths to add, not setting anything.\nskipped:1\nNo paths to add, not setting anything.\nempty:1\n",
            fixture.path("included"),
            fixture.path("file"),
            fixture.path("missing"),
        ),
    );
}

#[test]
fn gettext_fallback_concatenates_without_separators_or_newlines() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run("builtin _ 'one ' two --help; echo :$status; builtin _; echo empty:$status"),
        "one two--help:0\nempty:0\n",
    );
}
