//! Interpreter discovery reuses the bundled helper and checks unversioned Python.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-python-{}-{} space",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn interpreter(&self, name: &str, status: i32) {
        // A fixture subprocess records that discovery actually checked the
        // unversioned interpreter, rather than accepting its filename alone.
        std::fs::write(
            self.0.join(format!("{name}.cmd")),
            format!("@echo %* > \"%TEMP%\\{name}-probe.txt\"\r\n@exit /b {status}\r\n"),
        )
        .unwrap();
    }

    fn run(&self, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("PATH", &self.0)
            .env("PATHEXT", ".EXE;.CMD")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null())
            .output()
            .unwrap()
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-python-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn unversioned_python_must_pass_a_version_probe() {
    for status in [0, 1] {
        let fixture = Fixture::new();
        fixture.interpreter("python", status);
        let output = fixture.run("__fish_anypython");
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        if status == 0 {
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .eq_ignore_ascii_case(&fixture.0.join("python.cmd").to_string_lossy()),
                "{output:?}"
            );
        } else {
            assert!(output.stdout.is_empty(), "{output:?}");
        }
        let probe = std::fs::read_to_string(fixture.0.join("python-probe.txt")).unwrap();
        assert!(probe.contains("-ISc"), "{probe}");
        assert!(probe.contains("sys.version_info < (3, 5)"), "{probe}");
    }
}

#[test]
fn an_existing_python3_keeps_priority_over_the_unversioned_fallback() {
    let fixture = Fixture::new();
    fixture.interpreter("python3", 1);
    fixture.interpreter("python", 0);
    let output = fixture.run("__fish_anypython");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .eq_ignore_ascii_case(&fixture.0.join("python3.cmd").to_string_lossy()),
        "{output:?}"
    );
    assert!(!fixture.0.join("python-probe.txt").exists());
    assert!(!fixture.0.join("python3-probe.txt").exists());
}
