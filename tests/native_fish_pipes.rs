//! Explicit stdout/stderr pipelines keep redirect order and captured fields.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fish-native-pipes-{}-{} space 文",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let mut child = command
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("APPDATA", &self.0)
            .env("LOCALAPPDATA", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("PATH", "")
            .args(["-N", "-c", source])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                panic!(
                    "native pipeline fixture timed out: {:?}",
                    child.wait_with_output()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-pipes-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "status {:?}, stdout {}, stderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
fn stderr_pipe_preserves_stdout_and_each_stage_status() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        function emit
            echo visible
            echo piped >&2
            return 7
        end
        emit 2>| string upper
        echo $pipestatus
    "#,
        )),
        "visible\nPIPED\n7 0\n"
    );
}

#[test]
fn stderr_pipe_retains_explicit_stdout_fields_inside_substitution() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        function emit
            string split , 'a,,b'
            echo err >&2
        end
        set got (emit 2>| string upper)
        count $got
        printf '<%s>\n' $got
    "#,
        )),
        "4\n<a>\n<>\n<b>\n<ERR>\n"
    );
}

#[test]
fn stderr_pipe_accepts_external_commands_and_explicit_redirects_win() {
    let fixture = Fixture::new();
    let source = format!(
        r#"
        {} -N -c 'echo direct; echo external >&2' 2>| string upper
        begin
            echo stored >&2
        end 2>error.txt 2>| string upper
        echo $pipestatus
        begin
            echo first
            echo second >&2
        end 1>&2 2>| string upper
    "#,
        quote(Path::new(env!("CARGO_BIN_EXE_fish")))
    );
    assert_eq!(
        success(fixture.run(&source)),
        "direct\nEXTERNAL\n0 1\nFIRST\nSECOND\n"
    );
    assert_eq!(
        std::fs::read(fixture.0.join("error.txt")).unwrap(),
        b"stored\n"
    );
}

#[test]
fn bundled_apm_probe_can_select_completions_from_stderr() {
    let fixture = Fixture::new();
    let apm = Path::new(env!("CARGO_MANIFEST_DIR")).join("share/completions/apm.fish");
    let source = format!(
        r#"
        function apm; echo 'Atom Package Manager' >&2; end
        source {}
        complete -C 'apm ins'
    "#,
        quote(&apm)
    );
    assert!(
        success(fixture.run(&source))
            .lines()
            .any(|line| line.starts_with("install\t"))
    );
}
