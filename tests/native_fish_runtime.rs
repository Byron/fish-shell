//! Runtime variables and outer shell lifecycle through isolated native Fish.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-runtime-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(root.join("config/fish")).unwrap();
        Self(root)
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
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, source: &str) -> Output {
        self.command().args(["-c", source]).output().unwrap()
    }

    fn config(&self, source: &str) {
        std::fs::write(self.0.join("config/fish/config.fish"), source).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-runtime-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn output_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output_text(&output)
}

#[test]
fn runtime_variables_replace_inherited_versions_and_reset_duration() {
    let fixture = Fixture::new();
    let output = success(
        fixture
            .command()
            .env("version", "forged")
            .env("FISH_VERSION", "forged")
            .env("CMD_DURATION", "999")
            .env("COLUMNS", "99")
            .env("LINES", "43")
            .args([
                "-N",
                "-c",
                r#"
            printf '%s\n' $version $FISH_VERSION $CMD_DURATION $COLUMNS $LINES
            set -qgx version; echo version_export:$status
            set -qgx FISH_VERSION; echo fish_version_export:$status
            set -qgx CMD_DURATION; echo duration_export:$status
        "#,
            ])
            .output()
            .unwrap(),
    );
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines[0], lines[1]);
    assert_ne!(lines[0], "forged");
    let cli = success(fixture.command().arg("--version").output().unwrap());
    assert!(cli.contains(lines[0]), "{cli:?} versus {output:?}");
    assert_eq!(
        &lines[2..],
        [
            "0",
            "99",
            "43",
            "version_export:1",
            "fish_version_export:1",
            "duration_export:1"
        ]
    );
}

#[test]
fn versions_are_protected_through_every_user_binding_route() {
    let fixture = Fixture::new();
    fixture.config(
        r#"
        set -g original_version $version
        function verify --on-event fish_exit
            test "$version" = "$original_version"; echo version_intact:$status
            test "$FISH_VERSION" = "$original_version"; echo fish_version_intact:$status
        end
    "#,
    );
    for attempt in [
        "set -g version forged",
        "set -e FISH_VERSION",
        "set -l version forged",
        "set -U FISH_VERSION forged",
        "version=forged true",
        "for FISH_VERSION in forged; echo unreachable; end",
        "function bad --argument-names version; end",
        "function bad --inherit-variable version; end",
        "string match -r '(?<FISH_VERSION>.+)' forged",
        "printf 'forged\\n' | read version",
    ] {
        let output = fixture.run(attempt);
        assert!(
            !output.status.success(),
            "{attempt}: {}",
            output_text(&output)
        );
        assert_eq!(
            output_text(&output),
            "version_intact:0\nfish_version_intact:0\n",
            "{attempt}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("version") || error.contains("FISH_VERSION"),
            "{attempt}: {error}"
        );
        assert!(
            error.contains("read-only") || error.contains("read only"),
            "{attempt}: {error}"
        );
    }
}

#[test]
fn duration_and_size_variables_remain_writable_without_a_reader() {
    let fixture = Fixture::new();
    assert_eq!(success(fixture.run("set -g CMD_DURATION 777; set -g COLUMNS 88; set -g LINES 33; echo $CMD_DURATION $COLUMNS $LINES")), "777 88 33\n");
    assert_eq!(
        success(
            fixture
                .command()
                .env("COLUMNS", "invalid")
                .env("LINES", "0")
                .args(["-N", "-c", "echo $COLUMNS $LINES"])
                .output()
                .unwrap()
        ),
        "80 24\n"
    );
}

#[test]
fn forced_interactive_commands_and_scripts_do_not_enter_the_reader() {
    let fixture = Fixture::new();
    fixture.config(
        r#"
        function pre --on-event fish_preexec; echo unexpected-pre; end
        function post --on-event fish_postexec; echo unexpected-post; end
        function finish --on-event fish_exit; echo exit:$status:$argv; end
    "#,
    );
    let source = "status is-interactive; echo interactive:$status; echo arg:$argv; false";
    std::fs::write(fixture.0.join("script.fish"), source).unwrap();
    for args in [
        vec!["-i", "-c", source, "two words"],
        vec!["-i", "script.fish", "two words"],
    ] {
        let output = fixture.command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            output_text(&output),
            "interactive:0\narg:two words\nexit:1:1\n"
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        success(
            fixture
                .command()
                .args(["-i", "-n", "-c", "echo ignored"])
                .output()
                .unwrap()
        ),
        ""
    );
}

#[test]
fn shutdown_runs_once_for_command_source_and_evaluation_failures() {
    let fixture = Fixture::new();
    fixture.config("function finish --on-event fish_exit; echo exit:$status:$argv; end");
    for (args, expected_status, expected_error) in [
        (vec!["-c", "false"], 1, false),
        (vec!["-c", "exit 9"], 9, false),
        (vec!["-c", "'unterminated"], 2, true),
        (vec!["missing-script.fish"], 1, true),
    ] {
        let output = fixture.command().args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_status),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output_text(&output),
            format!("exit:{expected_status}:{expected_status}\n")
        );
        assert_eq!(!output.stderr.is_empty(), expected_error);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .matches("fish: ")
                .count()
                <= 1
        );
    }
    fixture.config("function finish --on-event fish_exit; echo exit:$status:$argv; end; exit 5");
    let output = fixture.run("echo unreachable");
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(output_text(&output), "exit:5:5\n");
}

#[test]
fn nested_evaluation_and_workers_do_not_emit_reader_lifecycle_events() {
    let fixture = Fixture::new();
    fixture.config(
        r#"
        function pre --on-event fish_preexec; echo unexpected-pre; end
        function post --on-event fish_postexec; echo unexpected-post; end
        function finish --on-event fish_exit; echo exit:$status:$argv; end
    "#,
    );
    std::fs::write(fixture.0.join("nested.fish"), "echo sourced").unwrap();
    assert_eq!(
        success(
            fixture.run("eval 'echo evaluated'; source nested.fish; echo worker | string upper")
        ),
        "evaluated\nsourced\nWORKER\nexit:0:0\n"
    );
}

#[test]
fn exit_handlers_preserve_the_shell_status_and_stop_on_their_own_exit() {
    let fixture = Fixture::new();
    let output = fixture.run("function finish --on-event fish_exit; echo before:$status; exit 97; end; function later --on-event fish_exit; echo unreachable; end; false");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output_text(&output), "before:1\n");
    assert!(output.stderr.is_empty());
}
