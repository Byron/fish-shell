//! Query parent jobs from native pipeline workers without inheriting process control.
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fish-job-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn run(&self, language: &str, source: &str) -> Output {
        let executable = match language {
            "fish" => env!("CARGO_BIN_EXE_fish"),
            "rash" => env!("CARGO_BIN_EXE_rash"),
            _ => unreachable!(),
        };
        let mut child = Command::new(executable)
            .args(["-N", "-c", source])
            .env("FISH_NATIVE_CONFIG_HOME", &self.0)
            .env(
                "FISH_JOB_SNAPSHOT_NULL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env_remove("fish_function_path")
            .current_dir(&self.0)
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
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("native {language} snapshot probe timed out: {source}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(root) = self.0.canonicalize() else {
            return;
        };
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-job-snapshot-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn cases(fish: &'static str, rash: &'static str) -> Vec<(&'static str, &'static str)> {
    let mut cases = vec![("rash", rash)];
    if cfg!(windows) {
        cases.push(("fish", fish));
    }
    cases
}

fn stdout(output: Output, language: &str) -> String {
    assert!(
        output.status.success(),
        "{language}: {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{language}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn helper_pipelines_do_not_count_themselves_as_jobs() {
    for (language, source) in cases(
        r#"
        jobs -g | count
        begin; jobs -g | count; end | read value
        echo nested:$value
    "#,
        r#"
        count_groups() { n=0; while IFS= read -r line; do n=$((n+1)); done; printf '%s\n' "$n"; }
        jobs -g | count_groups
        { jobs -g | count_groups; } | { read value; printf 'nested:%s\n' "$value"; }
    "#,
    ) {
        assert_eq!(
            stdout(Fixture::new().run(language, source), language),
            "0\nnested:0\n"
        );
    }
}

#[test]
fn nested_workers_query_live_and_stopped_parent_groups() {
    for (language, source) in cases(
        r#"
        while true; end &
        set pid $last_pid
        printf 'parent:%s\n' $pid
        jobs -g | read value
        printf 'worker:%s\n' $value
        begin; jobs -g | read value; printf 'nested:%s\n' $value; end | string join ''
        kill -STOP $pid
        jobs --group | read value
        printf 'stopped:%s\n' $value
        kill -CONT $pid
        kill -TERM $pid
        wait $pid
        jobs -g
        echo empty:$status
    "#,
        r#"
        while :; do :; done & pid=$!
        printf 'parent:%s\n' "$pid"
        jobs -g | { read value; printf 'worker:%s\n' "$value"; }
        { jobs -g | { read value; printf 'nested:%s\n' "$value"; }; } | { read line; printf '%s\n' "$line"; }
        kill -STOP "$pid"
        jobs --group | { read value; printf 'stopped:%s\n' "$value"; }
        kill -CONT "$pid"
        kill -TERM "$pid"
        wait "$pid"
        jobs -g
        printf 'empty:%s\n' "$?"
    "#,
    ) {
        let output = stdout(Fixture::new().run(language, source), language);
        let lines: Vec<_> = output.lines().collect();
        let pid: u32 = lines[0].strip_prefix("parent:").unwrap().parse().unwrap();
        assert_eq!(
            lines,
            [
                format!("parent:{pid}"),
                format!("worker:{pid}"),
                format!("nested:{pid}"),
                format!("stopped:{pid}"),
                "empty:1".into()
            ]
        );
    }
}

#[test]
fn worker_queries_never_grant_control_over_parent_processes() {
    for (language, source) in cases(
        r#"
        while true; end &
        set pid $last_pid
        begin
            jobs -g %1
            kill -0 $pid 2>$FISH_JOB_SNAPSHOT_NULL
            echo denied:$status
            jobs -g $pid
        end | string join ,
        kill -0 $pid
        echo owner:$status
        kill -TERM $pid
        wait $pid
        true
    "#,
        r#"
        while :; do :; done & pid=$!
        { jobs -g %1; kill -0 "$pid" 2>"$FISH_JOB_SNAPSHOT_NULL"; printf 'denied:%s\n' "$?"; jobs -g "$pid"; } |
            { read first; read denied; read last; printf '%s,%s,%s\n' "$first" "$denied" "$last"; }
        kill -0 "$pid"
        printf 'owner:%s\n' "$?"
        kill -TERM "$pid"
        wait "$pid"
        true
    "#,
    ) {
        let output = stdout(Fixture::new().run(language, source), language);
        let lines: Vec<_> = output.lines().collect();
        let fields: Vec<_> = lines[0].split(',').collect();
        assert_eq!(fields.len(), 3, "{output}");
        assert!(fields[0].parse::<u32>().is_ok(), "{output}");
        assert_eq!(fields[1], "denied:1", "{output}");
        assert_eq!(fields[0], fields[2], "{output}");
        assert_eq!(lines[1], "owner:0", "{output}");
    }
}
