//! Exercise the native executables through their public command-line interface.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fish-native-test-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only remove the private directory that this test created in temp_dir.
        let Ok(parent) = std::env::temp_dir().canonicalize() else {
            return;
        };
        let Ok(path) = self.0.canonicalize() else {
            return;
        };
        if path.parent() == Some(parent.as_path())
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fish-native-test-")
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn shell(name: &str, cwd: &Path) -> Command {
    let executable = match name {
        "fish" => env!("CARGO_BIN_EXE_fish"),
        "rash" => env!("CARGO_BIN_EXE_rash"),
        _ => unreachable!(),
    };
    let mut command = Command::new(executable);
    command.arg("-N").current_dir(cwd).stdin(Stdio::null());
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn assert_success(output: &Output) {
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
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fish_uses_fish_functions_lists_blocks_and_unicode_arguments() {
    let scratch = Scratch::new();
    let source = "set values one 'two words'; \
        function greet; echo 'hello Ω' $argv; end; \
        for value in $values; greet $value; end; \
        if false; echo wrong; else; echo right; end; echo $argv";
    let output = shell("fish", &scratch.0)
        .args(["-c", source, "space value", "漢字🌊"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "hello Ω one\nhello Ω two words\nright\nspace value 漢字🌊\n"
    );
}

#[test]
fn fish_script_paths_and_arguments_are_native_unicode() {
    let scratch = Scratch::new();
    let script = scratch.join("fish script Ω.fish");
    std::fs::write(&script, "for value in $argv; echo $value; end\n").unwrap();
    let output = shell("fish", &scratch.0)
        .arg(&script)
        .args(["two words", "漢字🌊"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "two words\n漢字🌊\n");
}

#[test]
fn fish_function_scope_and_source_return_preserve_caller_state() {
    let scratch = Scratch::new();
    std::fs::write(
        scratch.join("loaded Ω.fish"),
        "set -g from_source $argv; return 7; echo unreachable\n",
    )
    .unwrap();
    let source = "function local_scope; set -l label local; set fresh private; echo $label; end; \
        set -g label global; local_scope; echo $label; set -q fresh; echo $status; \
        source 'loaded Ω.fish' 'source argument'; echo $status; echo $from_source; echo $argv";
    let output = shell("fish", &scratch.0)
        .args(["-c", source, "outer value"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "local\nglobal\n1\n7\nsource argument\nouter value\n"
    );
}

#[test]
fn rash_preserves_bash_functions_substitutions_and_positional_arguments() {
    let scratch = Scratch::new();
    let source = "name='native Ω'; greet() { printf '<%s>\\n' \"$1\"; }; \
        result=$(greet \"$name\"); printf '%s\\n' \"$result\"; printf '<%s>\\n' \"$@\"";
    let output = shell("rash", &scratch.0)
        .args(["-c", source, "rash-test", "two words", "漢字🌊"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "<native Ω>\n<two words>\n<漢字🌊>\n");
}

#[test]
fn rash_scripts_change_directory_and_share_redirected_input() {
    let scratch = Scratch::new();
    let child = scratch.join("child dir Ω");
    std::fs::create_dir(&child).unwrap();
    let script = scratch.join("rash script Ω.sh");
    std::fs::write(&script, "cd 'child dir Ω'\npwd\nprintf '%s\\n' \"$1\"\nprintf 'first\\nsecond\\n' > input.txt\n{ read a; read b; } < input.txt\nprintf '%s/%s\\n' \"$a\" \"$b\"\n").unwrap();
    let output = shell("rash", &scratch.0)
        .arg(&script)
        .arg("script argument 漢字")
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    let mut lines = text.lines();
    assert_eq!(
        Path::new(lines.next().unwrap()).canonicalize().unwrap(),
        child.canonicalize().unwrap()
    );
    assert_eq!(
        lines.collect::<Vec<_>>(),
        ["script argument 漢字", "first/second"]
    );
    assert_eq!(
        std::fs::read(child.join("input.txt")).unwrap(),
        b"first\nsecond\n"
    );
}

#[test]
fn syntax_only_checks_do_not_execute_earlier_commands() {
    for (name, source) in [
        ("fish", "echo side_effect > marker; if true"),
        ("rash", "printf side_effect > marker; if true; then"),
    ] {
        let scratch = Scratch::new();
        let output = shell(name, &scratch.0)
            .args(["-n", "-c", source])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!scratch.join("marker").exists());
    }
}

#[test]
fn fish_rejects_unsupported_constructs_before_any_effects() {
    for unsupported in [
        "time echo unsupported",
        "exec echo unsupported",
        "echo unsupported 4>file",
    ] {
        let scratch = Scratch::new();
        let source = format!("echo side_effect > marker; {unsupported}");
        let output = shell("fish", &scratch.0)
            .args(["-c", &source])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{unsupported}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{unsupported}: {}",
            stdout(&output)
        );
        assert!(!scratch.join("marker").exists(), "{unsupported}");
    }
}

#[test]
fn malformed_initial_syntax_does_not_start_a_command() {
    for (name, source) in [
        ("fish", "end; echo side_effect > marker"),
        ("rash", "if ; then printf side_effect > marker; fi"),
    ] {
        let scratch = Scratch::new();
        let output = shell(name, &scratch.0)
            .args(["-c", source])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!scratch.join("marker").exists());
    }
}

#[test]
fn fish_rash_mode_is_explicit_and_both_binaries_remain_native_under_msys() {
    let scratch = Scratch::new();
    let output = shell("fish", &scratch.0)
        .args([
            "--rash-shell",
            "-c",
            "greeting=bash; printf '%s\\n' \"$greeting\"",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "bash\n");

    for name in ["fish", "rash"] {
        let output = shell(name, &scratch.0)
            .env("MSYSTEM", "MSYS")
            .arg("--version")
            .output()
            .unwrap();
        assert_success(&output);
        assert!(stdout(&output).contains("(native)"), "{}", stdout(&output));
        let output = shell(name, &scratch.0)
            .env("MSYSTEM", "MSYS")
            .args(["-c", "echo \"$MSYSTEM\""])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(stdout(&output), "MSYS\n");
    }
}

#[test]
fn native_external_launch_preserves_arguments_environment_redirections_and_status() {
    let scratch = Scratch::new();
    let probe = scratch.join("probe child Ω.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
    for name in ["fish", "rash"] {
        let source = if name == "fish" {
            "command $argv 2>&1"
        } else {
            "command \"$@\" 2>&1"
        };
        let mut command = shell(name, &scratch.0);
        command
            .env("FISH_NATIVE_TEST_PROBE", "1")
            .env("NATIVE_CHILD_VALUE", "spaces Ω and $literal");
        command.args(["-c", source]);
        if name == "rash" {
            command.arg("probe-shell");
        }
        let output = command
            .arg(&probe)
            .args([
                "--exact",
                "native_process_probe",
                "--nocapture",
                "--skip",
                "literal spaces & $() \"漢字\"",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(23),
            "{name}: stdout={} stderr={}",
            stdout(&output),
            String::from_utf8_lossy(&output.stderr)
        );
        let text = stdout(&output);
        assert!(
            text.contains("probe:argument:literal spaces & $() \"漢字\""),
            "{text}"
        );
        assert!(text.contains("probe:env:spaces Ω and $literal"), "{text}");
        assert!(text.contains("probe:stderr"), "{text}");
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn native_process_probe() {
    if std::env::var_os("FISH_NATIVE_TEST_PROBE").is_none() {
        return;
    }
    println!("probe:argument:{}", std::env::args().next_back().unwrap());
    println!("probe:env:{}", std::env::var("NATIVE_CHILD_VALUE").unwrap());
    eprintln!("probe:stderr");
    std::process::exit(23);
}

#[test]
fn native_fish_startup_and_mode_queries_use_invocation_state() {
    let scratch = Scratch::new();
    std::fs::create_dir(scratch.join("fish")).unwrap();
    let config = scratch.join("fish/config.fish");
    std::fs::write(&config, "set -g configured yes; return; echo unreachable").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .env("XDG_CONFIG_HOME", &scratch.0)
        .args([
            "--login",
            "-c",
            "echo $configured; status is-login; echo $status; status is-interactive; echo $status",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "yes\n0\n1\n");
    let output = shell("fish", &scratch.0)
        .env("XDG_CONFIG_HOME", &scratch.0)
        .args(["-c", "set -q configured; echo $status"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "1\n");
    std::fs::write(&config, "exit 7").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .env("XDG_CONFIG_HOME", &scratch.0)
        .args(["-c", "echo unreachable"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
}

#[test]
fn bash_interactive_language_keeps_commands_scripts_and_standard_input_in_fish() {
    let scratch = Scratch::new();
    let source = "set words 'two words' 漢字; for word in $words; echo $word; end";
    for options in [
        vec!["--interactive-language=bash"],
        vec!["--interactive-language", "bash"],
        vec!["--interactive-language=fish"],
    ] {
        let output = shell("fish", &scratch.0)
            .args(&options)
            .args(["-c", source])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(stdout(&output), "two words\n漢字\n");

        let script = scratch.join("input mode Ω.fish");
        std::fs::write(&script, source).unwrap();
        let output = shell("fish", &scratch.0)
            .args(&options)
            .arg(&script)
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(stdout(&output), "two words\n漢字\n");
    }

    let mut child = shell("fish", &scratch.0)
        .arg("--interactive-language=bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "two words\n漢字\n");

    let output = shell("fish", &scratch.0)
        .args([
            "--interactive-language=bash",
            "-c",
            "dialect_assignment=wrong",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn bash_interactive_language_keeps_fish_configuration_and_skips_bash_startup() {
    let scratch = Scratch::new();
    std::fs::create_dir(scratch.join("fish")).unwrap();
    std::fs::write(
        scratch.join("fish/config.fish"),
        "set -g configured 'Fish config Ω'; function configured_function; echo $configured; end",
    )
    .unwrap();
    std::fs::write(
        scratch.join(".bashrc"),
        "printf wrong > bash-startup-marker\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .current_dir(&scratch.0)
        .env("HOME", &scratch.0)
        .env("USERPROFILE", &scratch.0)
        .env("XDG_CONFIG_HOME", &scratch.0)
        .args(["--interactive-language=bash", "-c", "configured_function"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "Fish config Ω\n");
    assert!(!scratch.join("bash-startup-marker").exists());
}

#[test]
fn invalid_interactive_languages_fail_before_commands_have_effects() {
    for options in [
        vec!["--interactive-language="],
        vec!["--interactive-language=zsh"],
        vec!["--interactive-language", "rash"],
        vec!["--interactive-language=bash", "--rash-shell"],
        vec!["--rash-shell", "--interactive-language=fish"],
        vec!["--interactive-language=bash", "--script-language=bash"],
    ] {
        let scratch = Scratch::new();
        let output = shell("fish", &scratch.0)
            .args(&options)
            .args(["-c", "echo wrong > marker"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{options:?}");
        assert!(output.stdout.is_empty(), "{options:?}: {}", stdout(&output));
        assert!(!output.stderr.is_empty(), "{options:?}");
        assert!(!scratch.join("marker").exists(), "{options:?}");
    }
    let scratch = Scratch::new();
    let output = shell("fish", &scratch.0)
        .arg("--interactive-language")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn input_mode_changes_require_an_interactive_session_and_an_explicit_language() {
    let scratch = Scratch::new();
    for source in [
        "fish_input_mode bash",
        "fish_input_mode fish",
        "fish_input_mode",
        "fish_input_mode zsh",
        "fish_input_mode fish extra",
    ] {
        let output = shell("fish", &scratch.0)
            .args(["--interactive-language=bash", "-c", source])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{source}");
        assert!(output.stdout.is_empty(), "{source}: {}", stdout(&output));
        assert!(!output.stderr.is_empty(), "{source}");
    }
}
