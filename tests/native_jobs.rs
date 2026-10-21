//! Native shell workers, pipelines and owned background jobs.
#![cfg(windows)]

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(language: &str, source: &str) -> (i32, String, String) {
    let executable = if language == "fish" {
        env!("CARGO_BIN_EXE_fish")
    } else {
        env!("CARGO_BIN_EXE_rash")
    };
    let mut child = Command::new(executable)
        .args(["-N", "-c", source])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("native {language} timed out: {source}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (
        status.code().unwrap(),
        String::from_utf8(out.join().unwrap()).unwrap(),
        String::from_utf8(err.join().unwrap()).unwrap(),
    )
}

#[test]
fn fish_pipelines_run_functions_blocks_and_preserve_last_stage_variables() {
    let result = run(
        "fish",
        "function greet; echo 'hello Ω'; end; greet | string upper; begin; echo value; end | read saved; echo $saved; false | true; echo $pipestatus; not false | true; echo $status",
    );
    assert_eq!(
        result,
        (0, "HELLO Ω\nvalue\n1 0\n1\n".into(), String::new())
    );
}

#[test]
fn native_pipeline_streams_more_than_a_pipe_buffer() {
    let result = run("fish", "string repeat -n 131072 x | string length");
    assert_eq!(result, (0, "131072\n".into(), String::new()));
}

#[test]
fn rash_pipeline_workers_keep_bash_subshell_scope_and_pipefail() {
    let result = run(
        "rash",
        "f() { printf 'hello\\n'; }; f | { read value; printf '%s\\n' \"$value\"; }; printf '%s\\n' \"${value-unset}\"; set -o pipefail; false | true; printf '%s %s\\n' \"$?\" \"${PIPESTATUS[*]}\"",
    );
    assert_eq!(result, (0, "hello\nunset\n1 1 0\n".into(), String::new()));
}

#[test]
fn background_workers_are_owned_and_waitable_by_pid() {
    for (language, source) in [
        (
            "fish",
            "begin; exit 7; end &; set job $last_pid; wait $job; echo $status; jobs",
        ),
        (
            "rash",
            "{ exit 7; } & pid=$!; wait $pid; printf '%s\\n' \"$?\"; jobs",
        ),
    ] {
        assert_eq!(run(language, source), (0, "7\n".into(), String::new()));
    }
}

#[test]
fn native_job_control_stops_resumes_and_terminates_owned_workers() {
    for (language, source) in [
        (
            "fish",
            "while true; end &; kill -STOP %1; jobs -s; bg %1; kill -TERM %1; wait %1; echo $status; jobs",
        ),
        (
            "rash",
            "while :; do :; done & kill -STOP %1; jobs -s; bg %1; kill -TERM %1; wait %1; printf '%s\\n' \"$?\"; jobs",
        ),
    ] {
        let (status, stdout, stderr) = run(language, source);
        assert_eq!(status, 0, "{language}: {stderr}");
        assert!(stderr.is_empty(), "{language}: {stderr}");
        assert!(stdout.contains("Stopped"), "{language}: {stdout}");
        assert!(stdout.ends_with("143\n"), "{language}: {stdout}");
    }
}

#[test]
fn job_operations_reject_unowned_processes() {
    for language in ["fish", "rash"] {
        let (status, _, stderr) = run(language, "kill -TERM 4294967295");
        assert_ne!(status, 0);
        assert!(stderr.contains("not owned"), "{language}: {stderr}");
    }
}

#[test]
fn rash_background_and_or_list_is_one_job() {
    assert_eq!(
        run("rash", "false && printf wrong & wait; printf done"),
        (0, "done".into(), String::new())
    );
}

#[test]
fn fish_negation_preserves_pipeline_status_and_worker_shell_identity() {
    assert_eq!(
        run(
            "fish",
            "not false; echo $status $pipestatus; set parent $fish_pid; echo $fish_pid | read worker; test $worker = $parent; echo $status"
        ),
        (0, "0 1\n0\n".into(), String::new())
    );
}
