//! Completion files load on the first query through the native public executable.
#![cfg(windows)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-completion-autoload-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn write(&self, relative: &str, source: &str) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    fn function(&self, name: &str, source: &str) {
        self.write(&format!("config/fish/functions/{name}.fish"), source);
    }

    fn completion(&self, name: &str, source: &str) {
        self.write(&format!("config/fish/completions/{name}.fish"), source);
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
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0);
        command
    }

    fn run(&self, source: &str) -> Output {
        self.command().args(["-c", source]).output().unwrap()
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
                    .starts_with("fish-native-completion-autoload-")
            })
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn output(result: Output) -> String {
    assert!(
        result.status.success(),
        "status {:?}, stdout {}, stderr {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn first_query_loads_user_function_and_completion_once() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.completion(
        "native_probe",
        "set -g loads (math $loads + 1); complete native_probe -f -a alpha -d 'User choice'",
    );
    fixture.completion("unused_probe", "echo MUST_NOT_RUN");
    assert_eq!(
        output(fixture.run("set -g loads 0; complete -C 'native_probe al'; complete -C 'native_probe al'; echo loads:$loads")),
        "alpha\tUser choice\nalpha\tUser choice\nloads:1\n"
    );
}

#[test]
fn first_query_follows_new_function_and_completion_wrappers() {
    let fixture = Fixture::new();
    fixture.function(
        "native_alias",
        "function native_alias --wraps 'native_target fixed'; end",
    );
    fixture.function("native_target", "function native_target; end");
    fixture.function("native_last", "function native_last; end");
    fixture.completion(
        "native_target",
        "complete native_target -f -a alpha; complete native_target -w 'native_last other'",
    );
    fixture.completion(
        "native_last",
        "complete native_last -f -n '__fish_seen_subcommand_from fixed' -a omega",
    );
    assert_eq!(
        output(fixture.run("complete -C 'native_alias '; complete -C 'command native_alias om'"))
            .lines()
            .collect::<Vec<_>>(),
        ["alpha", "omega"]
    );
}

#[test]
fn nonexistent_commands_keep_manual_completions_without_loading_files() {
    let fixture = Fixture::new();
    fixture.completion(
        "missing_probe",
        "set -g unexpected yes; complete missing_probe -a wrong",
    );
    assert_eq!(
        output(fixture.run("complete missing_probe -f -a explicit; complete -C 'missing_probe ex'; set -q unexpected; echo missing:$status")),
        "explicit\nmissing:1\n"
    );
}

#[test]
fn native_paths_order_user_vendor_embedded_and_generated_completions() {
    let fixture = Fixture::new();
    fixture.completion("echo", "complete echo -f -l native-user");
    fixture.write(
        "data/fish/vendor_completions.d/echo.fish",
        "complete echo -f -l native-vendor",
    );
    fixture.write(
        "vendor/fish/vendor_completions.d/echo.fish",
        "complete echo -f -l native-system",
    );
    fixture.write(
        "cache/fish/generated_completions/echo.fish",
        "complete echo -f -l native-generated",
    );
    fixture.function("native_generated", "function native_generated; end");
    fixture.write(
        "cache/fish/generated_completions/native_generated.fish",
        "complete native_generated -f -a cached",
    );
    assert_eq!(
        output(fixture.run(
            r#"
            complete -C 'echo --native'
            set fish_complete_path $__fish_vendor_completionsdirs
            complete -C 'echo --native'
            set fish_complete_path $fish_complete_path[2]
            complete -C 'echo --native'
            set fish_complete_path $__fish_cache_dir/generated_completions
            complete -C 'echo --native'
            string match -q -- '-n*' (complete -C 'echo -n'); echo embedded:$status
            complete -C 'native_generated ca'
        "#
        )),
        "--native-user\n--native-vendor\n--native-system\nembedded:0\ncached\n"
    );
}

#[test]
fn explicit_completion_paths_override_native_defaults() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.completion("native_probe", "complete native_probe -f -a wrong");
    fixture.write(
        "custom/native_probe.fish",
        "complete native_probe -f -a selected",
    );
    assert_eq!(
        output(
            fixture
                .command()
                .env("fish_complete_path", fixture.0.join("custom"))
                .args(["-c", "complete -C 'native_probe '"])
                .output()
                .unwrap()
        ),
        "selected\n"
    );
}

#[test]
fn whole_command_erasure_prevents_autoload_before_and_after_loading() {
    let fixture = Fixture::new();
    for name in ["native_before", "native_after"] {
        fixture.function(name, &format!("function {name}; end"));
        fixture.completion(name, &format!("complete {name} -f -l native-option"));
    }
    assert_eq!(
        output(fixture.run(
            r#"
            complete native_before -e
            complete -C 'native_before --native'
            complete -C 'native_after --native'
            complete native_after -e
            complete -C 'native_after --native'
            complete native_before -f -a manual
            complete -C 'native_before ma'
        "#
        )),
        "--native-option\nmanual\n"
    );
}

#[test]
fn changing_paths_discards_loaded_command_registrations_and_wrappers() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.function("native_target", "function native_target; end");
    fixture.write(
        "first/native_probe.fish",
        "complete native_probe -f -a first; complete native_probe -w native_target",
    );
    fixture.write(
        "first/native_target.fish",
        "complete native_target -f -a wrapped",
    );
    fixture.write(
        "second/native_probe.fish",
        "complete native_probe -f -a second",
    );
    assert_eq!(
        output(fixture.run(
            r#"
            complete manual_probe -f -a retained
            set fish_complete_path first
            complete -C 'native_probe '
            set fish_complete_path second
            complete -C 'native_probe '
            complete -C 'manual_probe '
        "#
        )),
        "first\nwrapped\nsecond\nretained\n"
    );
}

#[test]
fn completion_autoload_state_survives_final_foreground_workers() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.completion(
        "native_probe",
        "set -g loads (math $loads + 1); complete native_probe -f -a alpha",
    );
    assert_eq!(
        output(fixture.run("set -g loads 0; echo ignored | complete -C 'native_probe al'; complete -C 'native_probe al'; echo ignored | complete -C 'native_probe al'; echo loads:$loads")),
        "alpha\nalpha\nalpha\nloads:1\n"
    );
}

#[test]
fn recursive_queries_during_autoload_do_not_reload_the_source() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.completion("native_probe", "set -g loads (math $loads + 1); complete -C 'native_probe --recursive'; complete native_probe -f -a alpha");
    assert_eq!(
        output(fixture.run("set -g loads 0; complete -C 'native_probe al'; echo loads:$loads")),
        "alpha\nloads:1\n"
    );
}

#[test]
fn autoload_cannot_consume_pending_input_and_restores_query_status() {
    let fixture = Fixture::new();
    fixture.function("native_probe", "function native_probe; end");
    fixture.completion(
        "native_probe",
        "read stolen; complete native_probe -f -n 'test $status -eq 1' -a alpha; true",
    );
    let mut child = fixture
        .command()
        .args([
            "-c",
            "false; complete -C 'native_probe al'; read pending; echo input:$pending",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"available\n")
        .unwrap();
    assert_eq!(
        output(child.wait_with_output().unwrap()),
        "alpha\ninput:available\n"
    );
}
