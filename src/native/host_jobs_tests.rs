use super::*;
use std::time::{Duration, Instant};

fn action(name: &str) -> Command {
    let mut command = tests::probe_command();
    command.env.insert(
        b"RASH_NATIVE_HOST_ACTION".to_vec(),
        name.as_bytes().to_vec(),
    );
    command
}

fn quiet_action(name: &str) -> Command {
    let mut command = action(name);
    command.redirects = vec![
        Redirection {
            fd: 1,
            action: RedirectAction::Opened(temporary_file(&[], false).unwrap()),
        },
        Redirection {
            fd: 2,
            action: RedirectAction::Dup(1),
        },
    ];
    command
}

fn builtin(host: &mut NativeHost, args: &[&str]) -> RunResult {
    let command = Command {
        argv: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
        env: Default::default(),
        redirects: vec![Redirection {
            fd: 2,
            action: RedirectAction::Dup(1),
        }],
        display: None,
        builtin: true,
    };
    host.run_pipeline(&[command], false, true).unwrap()
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "child did not make progress before deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pipelines_stream_more_than_pipe_capacity_and_keep_stage_statuses() {
    let mut host = NativeHost::new().unwrap();
    let output = host
        .run_pipeline(&[action("produce"), action("copy")], false, true)
        .unwrap();
    assert_eq!(output.pipeline, [3, 5]);
    assert_eq!(output.status, 5);
    let start = output
        .stdout
        .windows(11)
        .position(|bytes| bytes == b"DATA_START:")
        .unwrap()
        + 11;
    let end = output
        .stdout
        .windows(9)
        .position(|bytes| bytes == b":DATA_END")
        .unwrap();
    assert_eq!(end - start, 2 * 1024 * 1024);
    assert!(output.stdout[start..end].iter().all(|byte| *byte == b'x'));
}

#[test]
fn missing_pipeline_stage_closes_its_pipe_and_reports_each_status() {
    let mut host = NativeHost::new().unwrap();
    let mut missing = action("produce");
    missing.argv[0] = b"rash-nonexistent-pipeline-stage-2908".to_vec();
    let output = host
        .run_pipeline(&[missing, action("copy")], false, true)
        .unwrap();
    assert_eq!(output.pipeline, [127, 5]);
    assert_eq!(output.status, 5);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("rash-nonexistent-pipeline-stage-2908")
    );
}

#[test]
fn background_returns_immediately_and_wait_uses_owned_pid() {
    let gate = tests::TestFile::new();
    let mut command = quiet_action("gate");
    command.env.insert(
        b"RASH_NATIVE_HOST_FILE".to_vec(),
        rash::os::to_bytes(gate.0.as_os_str()).unwrap(),
    );
    let mut host = NativeHost::new().unwrap();
    let job = host.run_pipeline(&[command], true, false).unwrap();
    assert_eq!(job.status, 0);
    let pid = job.last_pid.unwrap().to_string();
    let jobs = builtin(&mut host, &["jobs", "-l"]);
    assert!(String::from_utf8_lossy(&jobs.stdout).contains(&pid));
    assert!(String::from_utf8_lossy(&jobs.stdout).contains("Running"));
    std::fs::write(&gate.0, b"go").unwrap();
    let waited = builtin(&mut host, &["wait", &pid]);
    assert_eq!(waited.status, 7);
    assert!(host.jobs().is_empty());
}

#[test]
fn stop_resume_and_terminate_control_only_the_owned_job() {
    let file = tests::TestFile::new();
    let mut command = quiet_action("tree");
    command.env.insert(
        b"RASH_NATIVE_HOST_FILE".to_vec(),
        rash::os::to_bytes(file.0.as_os_str()).unwrap(),
    );
    let mut host = NativeHost::new().unwrap();
    host.run_pipeline(&[command], true, false).unwrap();
    wait_until(|| std::fs::metadata(&file.0).unwrap().len() > 3);
    assert_eq!(builtin(&mut host, &["kill", "-STOP", "%1"]).status, 0);
    std::thread::sleep(Duration::from_millis(30));
    let stopped_size = std::fs::metadata(&file.0).unwrap().len();
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(std::fs::metadata(&file.0).unwrap().len(), stopped_size);
    let jobs = builtin(&mut host, &["jobs", "-s"]);
    assert!(String::from_utf8_lossy(&jobs.stdout).contains("Stopped"));
    assert!(builtin(&mut host, &["wait", "%1"]).status >= 128);
    assert_eq!(builtin(&mut host, &["bg", "%1"]).status, 0);
    wait_until(|| std::fs::metadata(&file.0).unwrap().len() > stopped_size);
    assert_eq!(builtin(&mut host, &["kill", "-TERM", "%1"]).status, 0);
    assert_eq!(builtin(&mut host, &["wait", "%1"]).status, 143);
    // Even the current shell PID is rejected unless it belongs to an owned job.
    assert_eq!(
        builtin(&mut host, &["kill", "-0", &std::process::id().to_string()]).status,
        1
    );
}

#[test]
fn disowned_job_survives_host_drop() {
    let file = tests::TestFile::new();
    let mut command = quiet_action("marker");
    command.env.insert(
        b"RASH_NATIVE_HOST_FILE".to_vec(),
        rash::os::to_bytes(file.0.as_os_str()).unwrap(),
    );
    let mut host = NativeHost::new().unwrap();
    host.run_pipeline(&[command], true, false).unwrap();
    assert_eq!(builtin(&mut host, &["disown", "%1"]).status, 0);
    assert!(host.jobs().is_empty());
    drop(host);
    wait_until(|| std::fs::metadata(&file.0).unwrap().len() != 0);
    assert_eq!(std::fs::read(&file.0).unwrap(), b"survived");
}

#[test]
fn fg_resumes_a_stopped_job_and_waits_for_its_status() {
    let mut host = NativeHost::new().unwrap();
    host.run_pipeline(&[quiet_action("sleep")], true, false)
        .unwrap();
    assert_eq!(builtin(&mut host, &["kill", "-STOP", "%1"]).status, 0);
    assert_eq!(builtin(&mut host, &["fg", "%1"]).status, 7);
    assert!(host.jobs().is_empty());
}

#[test]
fn wait_next_reaps_one_job_and_preserves_the_other() {
    let mut host = NativeHost::new().unwrap();
    let mut first = quiet_action("sleep");
    first
        .env
        .insert(b"RASH_NATIVE_HOST_MILLIS".to_vec(), b"100".to_vec());
    host.run_pipeline(&[first], true, false).unwrap();
    host.run_pipeline(&[quiet_action("sleep")], true, false)
        .unwrap();
    assert_eq!(builtin(&mut host, &["wait", "-n"]).status, 7);
    assert_eq!(host.jobs().len(), 1);
    assert_eq!(builtin(&mut host, &["wait", "%2"]).status, 7);
}

#[test]
#[cfg(windows)]
fn foreground_cancellation_latches_until_the_next_evaluation() {
    // Run the console-counter scenario in a separate process so parallel host
    // tests cannot consume its interrupt or cancel unrelated jobs.
    let mut host = NativeHost::new().unwrap();
    let mut command = action("cancel");
    // A nested host needs a writable temporary directory despite env_clear.
    command.env.insert(
        b"TEMP".to_vec(),
        rash::os::to_bytes(std::env::temp_dir().as_os_str()).unwrap(),
    );
    let result = host.run_pipeline(&[command], false, true).unwrap();
    assert_eq!(
        result.status,
        0,
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("cancel:130:latched:cleared"));
}

#[test]
fn setup_failure_reaps_processes_already_started() {
    let file = tests::TestFile::new();
    let mut first = quiet_action("tick");
    first.env.insert(
        b"RASH_NATIVE_HOST_FILE".to_vec(),
        rash::os::to_bytes(file.0.as_os_str()).unwrap(),
    );
    let mut invalid = action("copy");
    invalid.env.insert(b"INVALID=NAME".to_vec(), b"x".to_vec());
    let mut host = NativeHost::new().unwrap();
    assert!(host.run_pipeline(&[first, invalid], false, true).is_err());
    let size = std::fs::metadata(&file.0).unwrap().len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(std::fs::metadata(&file.0).unwrap().len(), size);
    assert!(host.jobs().is_empty());
}

pub(super) fn child_action() {
    let Ok(action) = std::env::var("RASH_NATIVE_HOST_ACTION") else {
        return;
    };
    match action.as_str() {
        "produce" => {
            let mut out = io::stdout().lock();
            out.write_all(b"DATA_START:").unwrap();
            out.write_all(&vec![b'x'; 2 * 1024 * 1024]).unwrap();
            out.write_all(b":DATA_END").unwrap();
            out.flush().unwrap();
            std::process::exit(3);
        }
        "copy" => {
            io::copy(&mut io::stdin().lock(), &mut io::stdout().lock()).unwrap();
            std::process::exit(5);
        }
        "sleep" => {
            let millis = std::env::var("RASH_NATIVE_HOST_MILLIS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(600);
            std::thread::sleep(Duration::from_millis(millis));
            std::process::exit(7);
        }
        "gate" => {
            let path = std::env::var_os("RASH_NATIVE_HOST_FILE").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while std::fs::metadata(&path).unwrap().len() == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::process::exit(7);
        }
        "tree" => {
            let mut child = ProcessCommand::new(std::env::current_exe().unwrap())
                .args(std::env::args_os().skip(1))
                .env("RASH_NATIVE_HOST_ACTION", "tick")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let status = child.wait().unwrap();
            #[cfg(unix)]
            let status = exit_status(status);
            #[cfg(windows)]
            let status = status.code().unwrap_or(1);
            std::process::exit(status);
        }
        "tick" => {
            let mut file = OpenOptions::new()
                .append(true)
                .open(std::env::var_os("RASH_NATIVE_HOST_FILE").unwrap())
                .unwrap();
            for _ in 0..1000 {
                file.write_all(b".").unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
            std::process::exit(0);
        }
        "marker" => {
            std::thread::sleep(Duration::from_millis(150));
            std::fs::write(
                std::env::var_os("RASH_NATIVE_HOST_FILE").unwrap(),
                b"survived",
            )
            .unwrap();
            std::process::exit(0);
        }
        #[cfg(windows)]
        "cancel" => {
            let mut host = NativeHost::new().unwrap();
            let interrupt = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                process::request_interrupt_for_test();
            });
            let result = host
                .run_pipeline(&[quiet_action("sleep")], false, true)
                .unwrap();
            interrupt.join().unwrap();
            assert_eq!(result.status, 130);
            assert!(host.cancelled());
            host.clear_console_requests();
            assert!(!host.cancelled());
            println!("cancel:130:latched:cleared");
            std::process::exit(0);
        }
        _ => panic!("unknown probe action"),
    }
}
