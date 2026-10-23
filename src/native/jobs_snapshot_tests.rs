use super::super::tests::{TestFile, probe_command};
use super::*;

fn gated(gate: &TestFile) -> Command {
    let mut command = probe_command();
    command
        .env
        .insert(b"RASH_NATIVE_HOST_ACTION".to_vec(), b"gate".to_vec());
    command.env.insert(
        b"RASH_NATIVE_HOST_FILE".to_vec(),
        rash::os::to_bytes(gate.0.as_os_str()).unwrap(),
    );
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

fn builtin(host: &mut NativeHost, args: &[&str]) -> Result<RunResult> {
    host.run_pipeline(
        &[Command {
            argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            display: None,
            env: Default::default(),
            redirects: vec![Redirection {
                fd: 2,
                action: RedirectAction::Dup(1),
            }],
            pipe_fd: 1,
            builtin: true,
        }],
        false,
        true,
    )
}

fn groups(host: &mut NativeHost, args: &[&str]) -> Vec<u32> {
    let result = builtin(host, args).unwrap();
    let pids: Vec<_> = std::str::from_utf8(&result.stdout)
        .unwrap()
        .lines()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(result.status, i32::from(pids.is_empty()));
    pids
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "job did not reach expected state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn group_identifier_survives_first_process_exit_and_includes_stopped_jobs() {
    let first = TestFile::new();
    let last = TestFile::new();
    let mut host = NativeHost::new().unwrap();
    let result = host
        .run_pipeline(&[gated(&first), gated(&last)], true, false)
        .unwrap();
    let first_pid = host.jobs[&1].processes[0].child.as_ref().unwrap().id();
    let last_pid = result.last_pid.unwrap();
    assert_ne!(first_pid, last_pid);
    assert_eq!(groups(&mut host, &["jobs", "-g"]), [first_pid]);
    std::fs::write(&first.0, b"go").unwrap();
    wait_until(|| {
        host.refresh_jobs().unwrap();
        matches!(host.jobs[&1].processes[0].state, State::Done(_))
    });
    assert_eq!(groups(&mut host, &["jobs", "--group"]), [first_pid]);
    assert_eq!(
        builtin(&mut host, &["jobs", "-p"]).unwrap().stdout,
        format!("{last_pid}\n").as_bytes()
    );
    builtin(&mut host, &["kill", "-STOP", "%1"]).unwrap();
    assert_eq!(groups(&mut host, &["jobs", "-g", "-s"]), [first_pid]);
    assert!(groups(&mut host, &["jobs", "-g", "-r"]).is_empty());
    assert!(matches!(
        host.worker_job_views().unwrap()[0].state,
        State::Stopped(_)
    ));
    builtin(&mut host, &["kill", "-CONT", "%1"]).unwrap();
    std::fs::write(&last.0, b"go").unwrap();
    wait_until(|| {
        host.refresh_jobs().unwrap();
        matches!(host.jobs[&1].state(), State::Done(_))
    });
    assert!(groups(&mut host, &["jobs", "-g"]).is_empty());
    assert!(host.worker_job_views().unwrap().is_empty());
    assert_eq!(builtin(&mut host, &["wait", "%1"]).unwrap().status, 7);
}

#[test]
fn inherited_queries_are_separate_from_owned_controls_and_job_ids() {
    let parent_gate = TestFile::new();
    let worker_gate = TestFile::new();
    let mut parent = NativeHost::new().unwrap();
    let parent_pid = parent
        .run_pipeline(&[gated(&parent_gate)], true, false)
        .unwrap()
        .last_pid
        .unwrap();
    let parent_views = parent.worker_job_views().unwrap();
    let mut worker = NativeHost::new_worker(parent_views.clone()).unwrap();
    assert_eq!(worker.next_job, 2);
    assert_eq!(groups(&mut worker, &["jobs", "-g", "%1"]), [parent_pid]);
    assert_eq!(
        groups(&mut worker, &["jobs", "-g", &parent_pid.to_string()]),
        [parent_pid]
    );
    assert_eq!(worker.jobs(), parent.jobs());
    assert_eq!(
        builtin(&mut worker, &["kill", "-0", &parent_pid.to_string()])
            .unwrap()
            .status,
        1
    );
    assert_eq!(
        builtin(&mut worker, &["kill", "-0", "%1"]).unwrap().status,
        1
    );
    for name in ["fg", "bg", "wait", "disown"] {
        assert!(
            builtin(&mut worker, &[name, "%1"]).is_err(),
            "{name} controlled an inherited job"
        );
    }
    assert_eq!(builtin(&mut worker, &["wait"]).unwrap().status, 0);
    assert_eq!(builtin(&mut worker, &["disown", "-a"]).unwrap().status, 0);
    assert_eq!(groups(&mut worker, &["jobs", "-g"]), [parent_pid]);

    let worker_pid = worker
        .run_pipeline(&[gated(&worker_gate)], true, false)
        .unwrap()
        .last_pid
        .unwrap();
    assert_eq!(worker.jobs.keys().copied().collect::<Vec<_>>(), [2]);
    assert_eq!(
        groups(&mut worker, &["jobs", "-g"]),
        [parent_pid, worker_pid]
    );
    assert_eq!(groups(&mut worker, &["jobs", "-g", "%+"]), [worker_pid]);
    assert_eq!(groups(&mut worker, &["jobs", "-g", "%-"]), [parent_pid]);
    let nested_views = worker.worker_job_views().unwrap();
    let encoded = crate::native::worker::encode(&nested_views).unwrap();
    let mut nested =
        NativeHost::new_worker(crate::native::worker::decode(&encoded).unwrap()).unwrap();
    assert_eq!(nested.next_job, 3);
    assert_eq!(
        groups(&mut nested, &["jobs", "-g"]),
        [parent_pid, worker_pid]
    );
    assert!(builtin(&mut nested, &["wait", "%2"]).is_err());
    assert_eq!(
        builtin(&mut nested, &["kill", "-0", "%2"]).unwrap().status,
        1
    );
    drop(nested);
    builtin(&mut worker, &["kill", "-STOP", "%+"]).unwrap();
    parent.refresh_jobs().unwrap();
    assert_eq!(parent.jobs[&1].state(), State::Running);
    assert!(matches!(worker.jobs[&2].state(), State::Stopped(_)));
    builtin(&mut worker, &["kill", "-CONT", "%2"]).unwrap();
    std::fs::write(&worker_gate.0, b"go").unwrap();
    builtin(&mut worker, &["wait", "%2"]).unwrap();
    drop(worker);
    assert_eq!(
        builtin(&mut parent, &["kill", "-0", "%1"]).unwrap().status,
        0
    );
    std::fs::write(&parent_gate.0, b"go").unwrap();
    builtin(&mut parent, &["wait", "%1"]).unwrap();
    // A view remains the captured state; later parent events do not grant control.
    assert_eq!(parent_views[0].state, State::Running);
}

#[test]
fn stopped_results_identify_the_job_and_background_completion_releases_shared_resources() {
    for consumer_finishes_first in [false, true] {
        let gate = TestFile::new();
        let directory =
            super::super::super::temporary::Directory::create(&std::env::temp_dir()).unwrap();
        let path = directory.path().join("consumer-file");
        std::fs::write(&path, b"still needed").unwrap();
        let mut consumer: Option<JobResource> = Some(Arc::new(directory));
        let mut host = NativeHost::new().unwrap();
        let pid = host
            .run_pipeline(&[gated(&gate)], true, false)
            .unwrap()
            .last_pid
            .unwrap();
        builtin(&mut host, &["kill", "-STOP", "%1"]).unwrap();
        let stopped = host.wait_job(1, false).unwrap();
        assert!(stopped.stopped);
        assert_eq!(stopped.last_pid, Some(pid));
        host.retain_job_resources(pid, &[consumer.as_ref().unwrap().clone()])
            .unwrap();
        if consumer_finishes_first {
            consumer.take();
        }
        assert!(path.exists(), "stopped job lost its consumer's file");
        builtin(&mut host, &["bg", "%1"]).unwrap();
        std::fs::write(&gate.0, b"go").unwrap();
        wait_until(|| {
            host.refresh_jobs().unwrap();
            matches!(host.jobs[&1].state(), State::Done(_))
        });
        // The exit status stays queryable, but cleanup must not wait for wait.
        assert_eq!(path.exists(), !consumer_finishes_first);
        consumer.take();
        assert!(!path.exists());
        assert_eq!(builtin(&mut host, &["wait", "%1"]).unwrap().status, 7);
    }
}
