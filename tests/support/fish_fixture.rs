//! Isolated executable-level fixtures shared by native and POSIX behavior tests.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct FishFixture(pub PathBuf);

impl FishFixture {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-parity-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn command(&self, source: &str) -> Command {
        // -N skips POSIX config.fish, which normally adds the user function path.
        // Select the isolated path explicitly so these tests compare evaluation,
        // rather than the intentional differences in startup initialization.
        let source = format!("set -p fish_function_path $__fish_config_dir/functions;\n{source}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .args(["-N", "-c", &source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("TMPDIR", &self.0)
            .env("LC_ALL", "C.UTF-8")
            .env("TERM", "dumb")
            .env("PATH", "")
            .stdin(Stdio::null());
        command
    }

    pub fn run(&self, source: &str) -> String {
        Self::stdout(self.command(source).output().unwrap())
    }

    pub fn stdout(output: Output) -> String {
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for FishFixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-parity-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
