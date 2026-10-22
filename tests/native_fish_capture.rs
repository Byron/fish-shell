//! Explicit command-substitution fields survive native worker processes.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn run(&self, source: &str) -> std::process::Output {
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
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("FISH_CAPTURE_EXECUTABLE", env!("CARGO_BIN_EXE_fish"))
            .output()
            .unwrap()
    }

    fn output(&self, source: &str) -> String {
        let result = self.run(source);
        assert!(
            result.status.success(),
            "{:?}: {}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize()) {
            if root.parent() == Some(temp.as_path())
                && root
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-capture-"))
            {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }
}

#[test]
fn split_preserves_newlines_and_empty_fields_in_direct_substitutions() {
    assert_eq!(
        Fixture::new().output("set v (string split , 'a\nb,c,'); printf '<%s>\\n' $v; count $v"),
        "<a\nb>\n<c>\n<>\n3\n"
    );
}

#[test]
fn quoted_substitutions_preserve_inferred_carriage_returns() {
    assert_eq!(
        Fixture::new().output("printf '<%s>' \"$(printf 'a\\r\\nb\\r\\n')\""),
        "<a\r\nb\r>"
    );
}

#[test]
fn external_capture_rejects_excessive_output_without_returning_partial_fields() {
    let result = Fixture::new().run(
        "set v (command \"$FISH_CAPTURE_EXECUTABLE\" -N -c 'for n in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17; string repeat -n1048576 x; end'); echo MUST_NOT_RUN",
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("output limit exceeded"));
}

#[test]
fn final_pipeline_workers_preserve_explicit_fields_and_session_updates() {
    assert_eq!(Fixture::new().output("set v (printf 'a,b,' | string split ,); printf '<%s>\\n' $v; count $v; echo ignored | begin; read ignored; set -g touched yes; end; echo $touched"), "<a>\n<b>\n<>\n3\nyes\n");
}

#[test]
fn worker_mixed_output_and_inherited_stderr_share_ordered_capture() {
    assert_eq!(Fixture::new().output("set v (begin; echo ignored | begin; read ignored; printf pre; string split , 'a\nb,c,'; printf err >&2; printf post; end; end 2>&1); printf '<%s>\\n' $v; count $v"), "<pre>\n<a\nb>\n<c>\n<>\n<errpost>\n5\n");
}

#[test]
fn nested_substitution_stdout_and_inherited_stderr_keep_distinct_destinations() {
    assert_eq!(Fixture::new().output("set v (begin; echo ignored | begin; read ignored; printf pre; set inner (begin; printf err >&2; string split , 'i,j,'; end); printf '<%s>' $inner; string split , 'a,b'; end; end 2>&1); printf '<%s>\\n' $v; count $v"), "<preerr<i><j><>>\n<a>\n<b>\n3\n");
}

#[test]
fn nested_pipeline_capture_keeps_empty_fields_separate_from_outer_output() {
    assert_eq!(Fixture::new().output("set v (echo ignored | begin; read ignored; printf pre; set inner (printf 'i,j,' | string split ,); printf '<%s>' $inner; string split , 'a,b'; end); printf '<%s>\\n' $v; count $v"), "<pre<i><j><>>\n<a>\n<b>\n3\n");
}

#[test]
fn external_bytes_between_fields_keep_merged_stdout_and_stderr_order() {
    assert_eq!(Fixture::new().output("set v (begin; echo ignored | begin; read ignored; string split , 'a,b'; command \"$FISH_CAPTURE_EXECUTABLE\" -N -c 'printf external; printf error >&2'; string split , 'c,d'; end; end 2>&1); printf '<%s>\\n' $v; count $v"), "<a>\n<b>\n<externalerror>\n<c>\n<d>\n5\n");
}

#[test]
fn redirected_fields_render_newlines_and_do_not_create_captured_empty_fields() {
    assert_eq!(Fixture::new().output("set v (string split , '' > fields); count $v; read line < fields; echo read:$status; set v (begin; string split , 'a,b' 2>&1 > fields; printf tail; end); printf '<%s>\\n' $v; printf 'a,b,' | string split , > fields; string length < fields"), "0\nread:0\n<tail>\n1\n1\n0\n");
}
