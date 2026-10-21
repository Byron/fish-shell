//! Native Fish compound statements retain Fish matching and redirect semantics.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fish-native-blocks-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, source: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fish"))
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .args(["-N", "-c", source])
            .output()
            .unwrap()
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(name)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let (Ok(parent), Ok(path)) = (std::env::temp_dir().canonicalize(), self.0.canonicalize())
        else {
            return;
        };
        if path.parent() == Some(parent.as_path())
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fish-native-blocks-")
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn switch_matches_first_case_and_preserves_scope_status_and_flow() {
    let scratch = Scratch::new();
    let output = scratch.run(
        r#"
        switch alpha.txt
            case '*.log' other
                echo wrong
            case '*.txt' README
                set -l inside private
                echo first
            case '*'
                echo wrong
        end
        set -q inside
        echo scope:$status
        switch $missing
            case ''
                echo empty
        end
        false
        switch absent
            case present
                echo wrong
        end
        echo unchanged:$status
        switch '漢字🌊'
            case '漢字*'
                echo unicode
        end
        switch ax
            case 'a?'
                echo wrong
            case 'a*'
                echo star
        end
        switch '*'
            case '\*'
                echo literal-star
        end
        function choose
            switch $argv
                case one
                    return 7
            end
            echo wrong
        end
        choose one
        echo returned:$status
        for value in one two
            switch $value
                case one
                    break
            end
            echo wrong
        end
        echo done
    "#,
    );
    success(&output);
    assert_eq!(
        output.stdout,
        b"first\nscope:1\nempty\nunchanged:1\nunicode\nstar\nliteral-star\nreturned:7\ndone\n"
    );
}

#[test]
fn failed_case_expansion_reports_and_continues_to_default() {
    let scratch = Scratch::new();
    let output =
        scratch.run("switch value; case no-such-file-*; echo wrong; case '*'; echo fallback; end");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"fallback\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("No matches for wildcard"));
    let output = scratch.run("set values one two; switch $values; case '*'; echo wrong; end");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Expected at most one argument, got 2")
    );
}

#[test]
fn block_redirects_share_offsets_and_restore_surrounding_descriptors() {
    let scratch = Scratch::new();
    let output = scratch.run(
        r#"
        begin
            echo one
            echo two
            begin
                echo nested
            end > inner.txt
            echo three
        end > outer.txt
        echo outside
        printf 'a\nb\n' > input.txt
        while read value
            echo got:$value
        end < input.txt > copied.txt
        if read value
            echo if:$value
        end < input.txt > if.txt
        for value in one two
            echo loop:$value
        end > loop.txt
        { echo brace; echo second; } > brace.txt
        switch value
            case value
                echo switched
        end > switch.txt
        function greet
            echo greeting
        end > definition.txt
        greet
        begin
            echo mixed
            printf 'error\n' >&2
        end &> combined.txt
        begin
            echo console >&2
            echo stored
        end 2>&1 > ordered.txt
        set calls
        function target
            set -g calls $calls called
            echo target.txt
        end
        begin
            echo left
            echo right
        end > (target)
        count $calls
    "#,
    );
    success(&output);
    assert_eq!(output.stdout, b"outside\ngreeting\nconsole\n1\n");
    for (name, expected) in [
        ("outer.txt", "one\ntwo\nthree\n"),
        ("inner.txt", "nested\n"),
        ("copied.txt", "got:a\ngot:b\n"),
        ("if.txt", "if:a\n"),
        ("loop.txt", "loop:one\nloop:two\n"),
        ("brace.txt", "brace\nsecond\n"),
        ("switch.txt", "switched\n"),
        ("definition.txt", ""),
        ("combined.txt", "mixed\nerror\n"),
        ("ordered.txt", "stored\n"),
        ("target.txt", "left\nright\n"),
    ] {
        assert_eq!(scratch.read(name), expected, "{name}");
    }
}

#[test]
fn return_and_break_unwind_block_redirects() {
    let scratch = Scratch::new();
    let output = scratch.run(
        r#"
        function choose
            begin
                echo captured
                return 6
            end > return.txt
            echo wrong
        end
        choose
        echo status:$status
        for value in one two
            begin
                echo once
                break
            end >> break.txt
            echo wrong
        end
        echo restored
    "#,
    );
    success(&output);
    assert_eq!(output.stdout, b"status:6\nrestored\n");
    assert_eq!(scratch.read("return.txt"), "captured\n");
    assert_eq!(scratch.read("break.txt"), "once\n");
}
