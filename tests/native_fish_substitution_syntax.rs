//! Command-substitution syntax and evaluation through the native executable.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fish-native-expansion-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .arg("-N")
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0);
        command
    }

    fn run(&self, source: &str, syntax_only: bool) -> Output {
        let mut command = self.command();
        if syntax_only {
            command.arg("-n");
        }
        command.args(["-c", source]).output().unwrap()
    }

    fn assert_expansion(&self, source: &str, expected: &str) {
        for syntax_only in [false, true] {
            let result = self.run(source, syntax_only);
            assert!(
                result.status.success(),
                "syntax_only={syntax_only}, source={source:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(result.stderr.is_empty());
            assert_eq!(
                String::from_utf8(result.stdout).unwrap(),
                if syntax_only { "" } else { expected },
                "syntax_only={syntax_only}, source={source:?}"
            );
        }
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-expansion-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn substitution_comments_cannot_close_or_extend_the_command() {
    let fixture = Fixture::new();
    for comment in ["(", ")", "\"", "'", "( ) ( \" ' ignored"] {
        fixture.assert_expansion(
            &format!(
                "printf '<%s>\\n' (printf '%s\\n' alpha # {comment}\n    printf '%s\\n' beta)"
            ),
            "<alpha>\n<beta>\n",
        );
    }
    fixture.assert_expansion(
        "printf '<%s>\\n' 文$(# ) ( \" ' ignored\n    printf '%s' value)尾",
        "<文value尾>\n",
    );
}

#[test]
fn nested_quoted_substitutions_have_independent_quote_contexts() {
    let fixture = Fixture::new();
    for (source, expected) in [
        (
            r#"printf '<%s>\n' "$(printf '%s' "$(printf '%s' ")")")""#,
            "<)>\n",
        ),
        (
            r#"printf '<%s>\n' (printf '%s' "$(printf '%s' "(")")"#,
            "<(>\n",
        ),
        (
            "printf '<%s>\\n' \"$(printf '%s' \"$(# ) ( \" ' ignored\n    printf '%s' \"quoted value\")\")\"",
            "<quoted value>\n",
        ),
        (
            r#"printf '<%s>\n' "pre$(printf '%s' "$(printf '%s' ")")")mid$(printf '%s' after)post""#,
            "<pre)midafterpost>\n",
        ),
    ] {
        fixture.assert_expansion(source, expected);
    }
}

#[test]
fn quoted_and_escaped_hashes_remain_literal_substitution_arguments() {
    Fixture::new().assert_expansion(
        r##"printf '<%s>\n' (printf '%s\n' '#)' "#(" \#\) word#\))"##,
        "<#)>\n<#(>\n<#)>\n<word#)>\n",
    );
}

#[test]
fn incomplete_and_invalid_substitutions_still_fail_syntax_checks() {
    let fixture = Fixture::new();
    for source in [
        "echo (echo value # ) ignored",
        "echo (echo value # ) ignored\n",
        "echo (echo 'unterminated)",
        "echo (echo $)",
        r#"echo "$(echo "$(echo 'unterminated)")""#,
    ] {
        for syntax_only in [false, true] {
            let result = fixture.run(source, syntax_only);
            assert!(
                !result.status.success(),
                "syntax_only={syntax_only}, source={source:?}"
            );
            assert!(result.stdout.is_empty());
            assert!(!result.stderr.is_empty());
        }
    }
}

#[test]
fn bundled_completions_with_substitution_comments_pass_syntax_checking() {
    let fixture = Fixture::new();
    for name in ["rustc.fish", "zfs.fish"] {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("share/completions")
            .join(name);
        let result = fixture.command().arg("-n").arg(source).output().unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
}
