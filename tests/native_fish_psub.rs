//! Process substitution files belong to the consuming command, not the capture.
#![cfg(windows)]
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fish-psub-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("tmp")).unwrap();
        Self(path)
    }

    fn command(&self, source: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("TMP", self.0.join("tmp"))
            .env("TEMP", self.0.join("tmp"))
            .env_remove("TMPDIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, source: &str, input: &[u8]) -> Output {
        let mut child = self.command(source).spawn().unwrap();
        let written = child.stdin.take().unwrap().write_all(input);
        let result = child.wait_with_output().unwrap();
        assert!(written.is_ok(), "stdin failed: {written:?}; {result:?}");
        result
    }

    fn assert_clean(&self) {
        let remaining = fs::read_dir(self.0.join("tmp"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(
            remaining.is_empty(),
            "temporary files left behind: {remaining:?}"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(result: Output) -> String {
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn source_and_function_consumers_keep_files_until_return() {
    let fixture = Fixture::new();
    let text = stdout(fixture.run(
        r#"
        source (printf 'set -g initialized yes\n' | psub)
        echo init:$initialized
        function consumer --argument-names filename
            set -g last_file $filename
            test -f $filename; echo exists:$status
            string collect --no-trim-newlines <$filename
            return 42
        end
        consumer (printf 'hello\nworld\n' | psub --suffix .txt)
        echo status:$status
        test -e $last_file; echo removed:$status
        string match -q '*.txt' $last_file; echo suffix:$status
        set saved (printf orphan | psub)
        test -e $saved; echo assignment:$status
    "#,
        b"",
    ));
    assert_eq!(
        text,
        "init:yes\nexists:0\nhello\nworld\nstatus:42\nremoved:1\nsuffix:0\nassignment:1\n"
    );
    fixture.assert_clean();
}

#[test]
fn nested_substitutions_follow_the_nearest_caller_and_external_consumers_work() {
    let fixture = Fixture::new();
    let text = stdout(fixture.run(
        r#"
        function identity --argument-names filename
            test -f $filename; or return 90
            echo $filename
        end
        function outer --argument-names filename
            test -e $filename; echo nested:$status
        end
        outer (identity (printf nested | psub))
        function external --argument-names filename
            set -g last_file $filename
            (status fish-path) -N $filename
        end
        external (printf 'echo external; exit 17\n' | psub)
        echo status:$status
        test -e $last_file; echo removed:$status
        string collect <(printf 'redirected\n' | psub); echo
    "#,
        b"",
    ));
    assert_eq!(
        text,
        "nested:1\nexternal\nstatus:17\nremoved:1\nredirected\n"
    );
    fixture.assert_clean();
}

#[test]
fn failures_clean_up_and_fifo_mode_reports_its_platform_limit() {
    for source in [
        "echo (printf data | psub) no-match-for-this-test-*",
        "source (printf 'echo data' | psub) <missing-input",
        "echo (printf data | psub --fifo)",
        "echo (printf data | psub --suffix ../invalid)",
    ] {
        let fixture = Fixture::new();
        let result = fixture.run(source, b"");
        assert!(!result.stderr.is_empty(), "{source}: {result:?}");
        fixture.assert_clean();
    }
}

#[test]
fn binary_input_larger_than_capture_limit_is_preserved() {
    let fixture = Fixture::new();
    let bytes: Vec<u8> = (0..17 * 1024 * 1024)
        .map(|index| (index % 256) as u8)
        .collect();
    let result = fixture.run("printf '%s' (psub --testing --suffix .bin)", &bytes);
    let path = PathBuf::from(stdout(result));
    assert!(path.starts_with(fixture.0.join("tmp")), "{path:?}");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    fs::remove_file(&path).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
    fixture.assert_clean();
}

#[test]
fn background_consumers_retain_files_and_forced_job_cancellation_cleans_them() {
    for cancel in [false, true] {
        let fixture = Fixture::new();
        let source = r#"
            function consumer --argument-names filename
                printf '%s' $filename >ready
                while not test -e gate
                end
                source $filename
            end
            consumer (printf 'echo background\n' | psub) &
            while not test -e action
            end
            read action <action
            if test "$action" = cancel
                kill -KILL %+
            end
            wait
        "#;
        let mut child = fixture.command(source).spawn().unwrap();
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let ready = fixture.0.join("ready");
        let path = loop {
            if let Ok(text) = fs::read_to_string(&ready) {
                if !text.is_empty() {
                    break PathBuf::from(text);
                }
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                panic!(
                    "consumer did not become ready: {:?}",
                    child.wait_with_output()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(path.exists(), "consumer's file disappeared: {path:?}");
        if !cancel {
            fs::write(fixture.0.join("gate"), b"").unwrap();
        }
        // Publish the command atomically so the waiting shell cannot read an
        // empty record between file creation and the write completing.
        fs::write(
            fixture.0.join("action.pending"),
            if cancel { b"cancel\n" } else { b"finish\n" },
        )
        .unwrap();
        fs::rename(fixture.0.join("action.pending"), fixture.0.join("action")).unwrap();
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("consumer did not finish: {:?}", child.wait_with_output());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let result = child.wait_with_output().unwrap();
        if !cancel {
            assert_eq!(stdout(result), "background\n");
        }
        assert!(
            !path.exists(),
            "consumer's file survived completion: {path:?}"
        );
        fixture.assert_clean();
    }
}

#[test]
fn foreground_pipeline_snapshots_preserve_the_enclosing_consumers_file() {
    let fixture = Fixture::new();
    let text = stdout(fixture.run(
        r#"
        function consumer --argument-names filename
            set -g last_file $filename
            printf ignored | read -l ignored
            test -f $filename; echo exists:$status
            source $filename
        end
        consumer (printf 'echo after-pipeline\n' | psub)
        test -e $last_file; echo removed:$status
        "#,
        b"",
    ));
    assert_eq!(text, "exists:0\nafter-pipeline\nremoved:1\n");
    fixture.assert_clean();
}

#[test]
fn disowned_worker_cleans_its_scratch_after_the_original_shell_exits() {
    let fixture = Fixture::new();
    let source = r#"
        function consumer --argument-names filename
            printf '%s' $filename >ready
            while not test -e gate
            end
            source $filename
            echo done >finished
        end
        consumer (printf 'echo detached\n' | psub) >consumer-output 2>consumer-errors &
        while not test -s ready
        end
        disown
    "#;
    let mut child = fixture.command(source).spawn().unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let parent_finished = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let path = fs::read_to_string(fixture.0.join("ready"))
        .ok()
        .map(PathBuf::from);
    let survived_parent = path.as_ref().is_some_and(|path| path.exists());
    // Always release a detached consumer before asserting a failure.
    fs::write(fixture.0.join("gate"), b"").unwrap();
    while std::time::Instant::now() < deadline {
        if fixture.0.join("finished").exists()
            && fs::read_dir(fixture.0.join("tmp"))
                .unwrap()
                .next()
                .is_none()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(parent_finished, "original shell did not exit");
    assert!(survived_parent, "detached consumer lost its file: {path:?}");
    assert!(
        fixture.0.join("finished").exists(),
        "consumer did not finish"
    );
    assert_eq!(stdout(child.wait_with_output().unwrap()), "");
    assert_eq!(
        fs::read_to_string(fixture.0.join("consumer-output")).unwrap(),
        "detached\n"
    );
    assert!(
        fs::read(fixture.0.join("consumer-errors"))
            .unwrap()
            .is_empty()
    );
    fixture.assert_clean();
}
