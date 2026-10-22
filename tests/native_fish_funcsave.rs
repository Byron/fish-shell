//! Save through the bundled helpers, then autoload in a fresh native session.
#![cfg(all(windows, feature = "native-utilities"))]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-funcsave-{}-{} space 文",
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
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn output(&self, source: &str) -> String {
        let output = self.run(source);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-funcsave-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn saved_alias_autoloads_with_arguments_and_wrapped_completions_in_new_sessions() {
    let fixture = Fixture::new();
    fixture.output("alias --save saved_git_checkout 'git checkout'");
    let definition = std::fs::read_to_string(
        fixture
            .0
            .join("config/fish/functions/saved_git_checkout.fish"),
    )
    .unwrap();
    assert!(
        definition.starts_with("function saved_git_checkout --wraps='git checkout'"),
        "{definition}"
    );
    assert!(
        definition.contains("--description 'alias saved_git_checkout git checkout'"),
        "{definition}"
    );
    assert_eq!(
        fixture.output(
            r#"
        functions -q saved_git_checkout; or exit 1
        function git; printf '<%s>\n' $argv; end
        saved_git_checkout --quiet 'two words'
    "#
        ),
        "<checkout>\n<--quiet>\n<two words>\n"
    );
    assert_eq!(
        fixture.output(
            r#"
        set -g fish_complete_path "$PWD/completions"
        function git; end
        complete -c git -f -n '__fish_seen_subcommand_from checkout' -a 'probe-topic probe-trail'
        complete -C 'saved_git_checkout probe-'
    "#
        ),
        "probe-topic\nprobe-trail\n"
    );
}

#[test]
fn funcsave_keeps_function_snapshots_in_the_selected_directory() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.output(
            r#"
        set captured 'original value' "a'b" 'C:\path'
        function saved_snapshot -d 'Saved description' -a argument -V captured
            printf '<%s>\n' $argument $captured
        end
        set captured changed
        funcsave --quiet --directory "$PWD/saved functions" saved_snapshot
    "#
        ),
        ""
    );
    assert_eq!(
        fixture.output(
            r#"
        set -g fish_function_path "$PWD/saved functions" $fish_function_path
        saved_snapshot argument
    "#
        ),
        "<argument>\n<original value>\n<a'b>\n<C:\\path>\n"
    );
}
