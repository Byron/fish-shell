#![cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

mod string_display_cases;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fish-native-display-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self, source: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"));
        command
    }
    fn run(&self, source: &str) -> Output {
        self.command(source).output().unwrap()
    }
    fn check(&self, source: &str, status: i32, expected: &str) {
        let output = self.run(source);
        assert_eq!(output.status.code(), Some(status), "{source}: {output:?}");
        assert!(output.stderr.is_empty(), "{source}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{source}"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let (Ok(path), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && path.parent() == Some(temp.as_path())
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fish-native-display-")
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[test]
fn established_posix_display_cases() {
    let fixture = Fixture::new();
    string_display_cases::check(|args, status, expected| {
        let output = fixture
            .command("string $argv")
            .arg("--")
            .args(&args[1..])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{args:?}"
        );
        if status != 2 {
            assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
        }
    });
}

#[test]
fn widths_follow_local_and_temporary_settings_without_leaking() {
    Fixture::new().check(
        r#"
        string length -V 💩
        fish_emoji_width=1 string length -V 💩
        string length -V 💩
        fish_emoji_width=1 string shorten -m6 abcde💩
        fish_emoji_width=2 string shorten -m6 abcde💩
        fish_ambiguous_width=2 string length -V …
        fish_ambiguous_width=0 string length -V …
        function narrow
            set -l fish_emoji_width 1
            string pad -w3 --char=💩 x
        end
        narrow
        string length -V 💩
    "#,
        0,
        "2\n1\n2\nabcde💩\nabcde…\n2\n0\n💩💩x\n2\n",
    );
}

#[test]
fn pipelines_preserve_display_newlines_and_quiet_read_boundaries() {
    Fixture::new().check(
        r#"
        printf x | string pad -w3; echo end
        printf 'x\nyy' | string pad -w3; echo end
        printf x | string length -V
        printf foobar | string shorten -m4
        printf 'a\nb\n' | begin
            string length -qV <&0
            read -l line
            echo $line
        end
    "#,
        0,
        "  xend\n  x yyend\n1\nfoo…\nb\n",
    );
}

#[test]
fn colored_backspaces_never_produce_negative_widths() {
    Fixture::new().check(
        r#"
        string length -V \e\[31m\b\b\b\b
        string pad -w2 \e\[31m\b\b\b\b
        string shorten -m1 \e\[31m\b\b\b\b
        echo status:$status
    "#,
        0,
        "0\n  \x1b[31m\x08\x08\x08\x08\n\x1b[31m\x08\x08\x08\x08\nstatus:1\n",
    );
}

#[test]
fn display_errors_are_bounded_and_leave_following_commands_running() {
    let fixture = Fixture::new();
    for source in [
        "string pad -w9223372036854775807 x",
        "string pad -w",
        "string shorten --bogus x",
        "string length --visible=yes x",
        "printf x | string length -V y",
    ] {
        let output = fixture.run(&format!("{source}; echo status:$status"));
        assert_eq!(output.stdout, b"status:2\n", "{source}: {output:?}");
        assert!(!output.stderr.is_empty(), "{source}: {output:?}");
    }
    std::fs::write(fixture.0.join("records"), "\n".repeat(100_001)).unwrap();
    let output = fixture.run("string shorten < records; echo status:$status");
    assert_eq!(output.stdout, b"status:2\n", "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("input limit exceeded"));
}
