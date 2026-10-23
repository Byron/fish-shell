#![cfg(windows)]
use std::process::{Command, Output};

fn fish(source: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .output()
        .unwrap()
}

fn run(source: &str) -> String {
    let output = fish(source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn described_aliases_accept_punctuation_and_preserve_wrapper_metadata() {
    let output = run(r#"
        function gcan! --wraps='git commit --amend --no-edit' --description 'Amend the current commit'
            printf '<%s>\n' $argv
        end
        gcan! 'two words' --flag
        functions gcan!
        "#);
    assert!(output.starts_with("<two words>\n<--flag>\n"), "{output}");
    assert!(
        output.contains("function gcan! --wraps='git commit --amend --no-edit' --description 'Amend the current commit'"),
        "{output}"
    );
}

#[test]
fn named_arguments_and_inherited_snapshots_follow_fish_precedence() {
    assert_eq!(
        run(r#"
            set saved before 'two words'
            set empty
            function snapshot -a first second third -V saved -V empty -V missing
                printf 'first=<%s> second=<%s>\n' $first $second
                count $third $empty $missing
                printf 'saved=<%s>\n' $saved
                printf 'argv=<%s>\n' $argv
            end
            set saved after
            snapshot one two
            function named_argv --argument-names first argv third
                printf '<%s>\n' $argv
            end
            named_argv one two three
            set argv captured 'captured again'
            function inherited_argv -a argv -V argv
                printf '<%s>\n' $argv
            end
            inherited_argv ignored
            "#),
        "first=<one> second=<two>\n0\nsaved=<before>\nsaved=<two words>\nargv=<one>\nargv=<two>\n<two>\n<captured>\n<captured again>\n"
    );
}

#[test]
fn unshadowed_functions_share_the_callers_function_scope() {
    assert_eq!(
        run(r#"
            set -g label global
            function private_label
                echo $label
            end
            function shared_label --no-scope-shadowing
                echo $label
                set label changed
                set function_only hidden
            end
            function outer
                set -l label local
                private_label
                shared_label
                echo $label
                set -q function_only
                echo $status
            end
            outer
            echo $label
            "#),
        "global\nlocal\nchanged\n0\nglobal\n"
    );
}

#[test]
fn printed_definitions_roundtrip_description_wraps_and_captured_values() {
    let output = run(r#"
        set captured 'original value' "a'b" 'C:\path'
        function snapshot -d 'A quoted description' -w 'git log' -a argument -V captured -S
            printf '<%s>\n' $argument $captured
        end
        set captured changed
        set definition "$(functions snapshot)"
        functions -e snapshot
        eval $definition
        snapshot argument
        functions snapshot
        "#);
    assert!(
        output.starts_with("<argument>\n<original value>\n<a'b>\n<C:\\path>\n"),
        "{output}"
    );
    assert!(
        output.contains("--wraps='git log' --description 'A quoted description' --no-scope-shadowing --argument-names argument"),
        "{output}"
    );
}

#[test]
fn function_options_use_shared_getopt_ordering_and_validate_names() {
    assert_eq!(
        run(r#"
            function attached -dDescription --wraps=echo -Sa first -- second
                echo $first $second
            end
            attached one two
            if false
                function unused --on-event event
                end
            end
            echo after
            "#),
        "one two\nafter\n"
    );
    for (source, diagnostic) in [
        (
            "function invalid argument; end",
            "unexpected positional argument",
        ),
        (
            "function invalid -a first -d description second; end",
            "unexpected positional argument",
        ),
        (
            "function invalid --description; end",
            "requires an argument",
        ),
        (
            "function invalid --no-scope-shadowing=value; end",
            "does not take an argument",
        ),
        ("function invalid --unknown; end", "unknown option"),
        ("function invalid -a status; end", "read-only"),
        (
            "function invalid -a 'not a variable'; end",
            "invalid variable name",
        ),
        ("function test; end", "reserved keyword"),
        ("function 'invalid/name'; end", "invalid function name"),
        ("function -d description; end", "invalid function name"),
    ] {
        let output = fish(source);
        assert_eq!(output.status.code(), Some(2), "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn platform_specific_event_handlers_are_rejected_explicitly() {
    for option in ["--on-signal", "--on-job-exit", "--on-process-exit"] {
        let output = fish(&format!("function handler {option} value; end"));
        assert_eq!(output.status.code(), Some(2), "{option}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("function event handlers"), "{error}");
        assert!(error.contains(option), "{error}");
    }
}

#[test]
fn pipeline_workers_preserve_function_properties_and_return_scope() {
    assert_eq!(
        run(r#"
            set captured snapshot
            function pipeline_function -d Description -a argument -V captured
                echo $argument $captured
                return 0
                echo unreachable
            end
            pipeline_function value | string upper
            echo after
            "#),
        "VALUE SNAPSHOT\nafter\n"
    );
}
