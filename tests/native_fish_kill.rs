//! Native kill queries and completions describe the host's owned jobs.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-kill-{}-{} space",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn run(&self, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        let mut child = command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("native kill fixture timed out: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-kill-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn successful(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "\\'")
    )
}

#[test]
fn list_reports_only_supported_signals_and_converts_names_numbers_and_statuses() {
    let fixture = Fixture::new();
    assert_eq!(
        successful(fixture.run(
            "kill -l; kill --list 2 9 15 18 19 20; kill -l sigint TERM stop TSTP; kill -l 130 143"
        )),
        "INT KILL TERM CONT STOP TSTP\nINT\nKILL\nTERM\nCONT\nSTOP\nTSTP\n2\n15\n19\n20\nINT\nTERM\n"
    );
}

#[test]
fn invalid_options_are_redirectable_command_failures() {
    let fixture = Fixture::new();
    for source in [
        "kill -L",
        "kill --list HUP",
        "kill --list 1",
        "kill -s",
        "kill --signal=HUP %1",
    ] {
        assert_eq!(
            successful(fixture.run(&format!(
                "{source} 2>diagnostic; echo status:$status; if kill -L >/dev/null 2>/dev/null; echo table; end; echo survived"
            ))),
            "status:2\nsurvived\n",
            "{source}"
        );
        assert!(
            std::fs::read_to_string(fixture.0.join("diagnostic"))
                .unwrap()
                .starts_with("kill: ")
        );
    }
    // Signal zero checks ownership without controlling this unrelated parent.
    assert_eq!(
        successful(fixture.run(&format!(
            "kill -0 {} 2>/dev/null; echo status:$status",
            std::process::id()
        ))),
        "status:1\n"
    );
}

#[test]
fn bundled_completion_uses_native_options_and_only_owned_job_pids() {
    let fixture = Fixture::new();
    let flags = successful(fixture.run("complete -C 'kill -'"));
    let flags: Vec<_> = flags
        .lines()
        .map(|line| line.split('\t').next().unwrap())
        .collect();
    for expected in [
        "--list", "--signal", "-2", "-15", "-INT", "-STOP", "-s", "-l",
    ] {
        assert!(flags.contains(&expected), "{flags:?}");
    }
    for unsupported in ["-L", "--table", "--all", "--pid", "--queue", "-HUP", "-1"] {
        assert!(!flags.contains(&unsupported), "{flags:?}");
    }
    let signals = successful(fixture.run("complete -C 'kill --signal '"));
    let signals: Vec<_> = signals
        .lines()
        .map(|line| line.split('\t').next().unwrap())
        .collect();
    assert_eq!(signals, ["INT", "KILL", "TERM", "CONT", "STOP", "TSTP"]);
    let executable = quote(&std::env::current_exe().unwrap());
    let result = successful(fixture.run(&format!(
        r#"
        function __fish_complete_pids; echo FOREIGN_PROCESS; end
        FISH_KILL_TEST_CHILD=1 {executable} --exact native_kill_test_child --nocapture >NUL 2>NUL &
        set pid $last_pid
        set choices (complete -C 'kill ')
        contains -- $pid $choices; echo owned:$status
        contains -- FOREIGN_PROCESS $choices; echo foreign:$status
        kill --signal=term $pid; echo sent:$status
        wait $pid
        echo survived
        "#
    )));
    assert_eq!(result, "owned:0\nforeign:1\nsent:0\nsurvived\n");
}

#[test]
fn native_kill_test_child() {
    if std::env::var_os("FISH_KILL_TEST_CHILD").is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
