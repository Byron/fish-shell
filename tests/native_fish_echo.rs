//! Echo parity matters to shared helpers, including Git's completion cache.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-echo-{}-{}",
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
        command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.0.join("gitconfig"))
            .env("PATH", "")
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-echo-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn successful(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "status {:?}, stderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn echo_accepts_fish_options_and_preserves_literal_byte_escapes() {
    let fixture = Fixture::new();
    let output = successful(fixture.run(
        r#"
        echo -- -n log
        echo -ns one two
        echo -neE '\n'
        echo -nex text
        echo -e '\q\\'
        echo -e '\xFF\377\0\cbeyond'
        echo after
        "#,
    ));
    assert_eq!(
        output,
        b"-n log\nonetwo\\n-nex text\n\\q\\\n\xff\xff\0after\n"
    );
}

#[test]
fn bundled_git_conditions_reuse_the_cached_subcommand_without_a_separator() {
    let fixture = Fixture::new();
    let output = successful(fixture.run(
        r#"
        complete -C 'git log --oneline' | string split -f1 \t
        complete -C 'git log --oneline' | string split -f1 \t
        complete -C 'git status --short' | string split -f1 \t
        complete -C 'git checkout --ours' | string split -f1 \t
        "#,
    ));
    assert_eq!(output, b"--oneline\n--oneline\n--short\n--ours\n");
}
