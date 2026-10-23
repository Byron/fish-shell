//! Shared random behavior and native worker state through the executable.
#![cfg(windows)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-random-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0);
        command
    }

    fn run(&self, source: &str) -> Output {
        self.command().args(["-N", "-c", source]).output().unwrap()
    }

    fn text(&self, source: &str) -> String {
        let output = self.run(source);
        assert!(output.status.success(), "{source}: {:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{source}: {:?}", output.stderr);
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-random-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn seeds_repeat_across_functions_and_command_substitutions() {
    let fixture = Fixture::new();
    let output = fixture.text(
        r#"
        random -- -42
        random
        random -- -10 3 10
        random choice 文 😀 fish
        random -- -42
        function draw --description 'Draw from the caller seed'
            random
        end
        draw
        echo (random -- -10 3 10)
        echo (random choice 文 😀 fish)
    "#,
    );
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[..3], lines[3..]);
    let ordinary: i64 = lines[0].parse().unwrap();
    assert!((0..=32767).contains(&ordinary));
    assert!(["文", "😀", "fish"].contains(&lines[2]));
}

#[test]
fn full_width_reversed_and_stepped_ranges_stay_within_their_boundaries() {
    let fixture = Fixture::new();
    for (start, step, end) in [
        (i64::MIN, 1, i64::MAX),
        (i64::MIN, u64::MAX, i64::MAX),
        (10, 3, -10),
        (5, u64::MAX, 5),
    ] {
        let mut source = "random 42\n".to_owned();
        for _ in 0..64 {
            writeln!(source, "random -- {start} {step} {end}").unwrap();
        }
        let output = fixture.text(&source);
        let (low, high) = (start.min(end), start.max(end));
        let values: Vec<i64> = output.lines().map(|line| line.parse().unwrap()).collect();
        assert_eq!(values.len(), 64);
        for value in &values {
            assert!((low..=high).contains(value));
            assert_eq!(value.abs_diff(low) % step, 0);
        }
        if step == u64::MAX && low != high {
            assert!(values.contains(&low));
            assert!(values.contains(&high));
        }
    }
}

#[test]
fn choices_preserve_empty_unicode_and_option_shaped_values() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.text(
            r#"
            random choice only
            random choice -h
            random -- choice ''
            random choice 文😀
            builtin -q random
            echo builtin:$status
            type -t random
            builtin --names | string match random
        "#,
        ),
        "only\n-h\n\n文😀\nbuiltin:0\nbuiltin\nrandom\n"
    );
}

#[test]
fn malformed_requests_report_status_and_allow_later_commands() {
    let fixture = Fixture::new();
    for (source, status, diagnostic) in [
        ("random a", 2, "invalid integer"),
        ("random -1", 2, "unknown option"),
        ("random --help=yes", 2, "does not take an argument"),
        ("random choice", 2, "nothing to choose from"),
        ("random 9223372036854775808", 2, "invalid integer"),
        ("random -- 0 -1 10", 2, "invalid integer"),
        ("random -- 0 18446744073709551616 10", 2, "invalid integer"),
        ("random -- 0 0 10", 2, "STEP must be a positive integer"),
        ("random -- 1 2 3 4", 1, "too many arguments"),
        ("random 1 --help", 2, "invalid integer"),
    ] {
        let output = fixture.run(&format!("{source}; echo status:$status; echo continued"));
        assert!(output.status.success(), "{source}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("status:{status}\ncontinued\n")
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn invalid_requests_do_not_advance_a_seeded_generator() {
    let fixture = Fixture::new();
    let output =
        fixture.run("random 15; random; random 15; random choice; random -- 0 0 10; random");
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], lines[1]);
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("nothing to choose from"));
    assert!(diagnostic.contains("STEP must be a positive integer"));
}

#[test]
fn workers_inherit_seeds_and_return_only_the_final_foreground_stage_state() {
    let fixture = Fixture::new();
    let output = fixture.text(
        r#"
        random 41
        random
        random
        random 41
        true | random
        random
        random 41
        random | string sub
        random
        random 99
        random
        random 41
        true | random 99
        random
        random 41
        random 99 &
        wait
        random
    "#,
    );
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 9, "{output}");
    assert_eq!(lines[0..2], lines[2..4]);
    assert_eq!(lines[4], lines[0]);
    assert_eq!(lines[5], lines[0]);
    assert_eq!(lines[6], lines[7]);
    assert_eq!(lines[8], lines[0]);
}

#[test]
fn help_is_available_without_consuming_seeded_state() {
    let fixture = Fixture::new();
    let output = fixture.text("random 42; random; random 42; random --help; random");
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[1].starts_with("random [SEED"));
    assert_eq!(lines[0], lines[2]);
}
