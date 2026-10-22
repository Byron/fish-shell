#![cfg(windows)]
use std::io::Write;
use std::process::{Command, Stdio};

fn run(source: &str, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .env(
            "FISH_NATIVE_CONFIG_HOME",
            std::env::temp_dir().join(format!("fish-read-tests-{}", std::process::id())),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn output(source: &str, input: &[u8]) -> String {
    let result = run(source, input);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn nul_records_preserve_newlines_empty_records_and_following_input() {
    assert_eq!(
        output(
            "read -lz first; read -lz empty; read -lz third; read last; printf '<%s>\\n' $first $empty $third $last",
            b"alpha\nbeta\0\0tail\r\0unused\n"
        ),
        "<alpha\nbeta>\n<>\n<tail\r>\n<unused>\n"
    );
    assert_eq!(
        output(
            "read -alz -d \\n lines; printf '<%s>\\n' $lines",
            b"one\ntwo\n\0"
        ),
        "<one>\n<two>\n<>\n"
    );
}

#[test]
fn tokenize_uses_fish_quoting_without_executing_expansions() {
    let input = b"'one two' $variable (echo unsafe) a\\ b\n";
    assert_eq!(
        output("read -lat fields; printf '<%s>\\n' $fields", input),
        "<one two>\n<$variable>\n<(echo unsafe)>\n<a b>\n"
    );
    assert_eq!(
        output(
            "read -la --tokenize-raw fields; printf '<%s>\\n' $fields",
            input
        ),
        "<'one two'>\n<$variable>\n<(echo unsafe)>\n<a\\ b>\n"
    );
    assert_eq!(
        output("read -lt first rest; printf '<%s>\\n' $first $rest", input),
        "<one two>\n<$variable (echo unsafe) a\\ b>\n"
    );
}

#[test]
fn line_mode_reads_only_as_many_records_as_variables() {
    assert_eq!(
        output(
            "read -lL first second; read third; printf '<%s>\\n' $first $second $third",
            b"one two\r\nthree four\nfive\n"
        ),
        "<one two>\n<three four>\n<five>\n"
    );
    assert_eq!(
        output(
            "read -L first second; echo status:$status; printf '%s:%s\\n' (count $first) (count $second)",
            b"one\n"
        ),
        "status:1\n1:0\n"
    );
}

#[test]
fn character_limits_keep_utf8_and_leave_the_next_character_unread() {
    assert_eq!(
        output(
            "read -P'not shown' -n2 first; read rest; printf '<%s>\\n' $first $rest",
            "文😀tail\n".as_bytes()
        ),
        "<文😀>\n<tail>\n"
    );
    assert_eq!(
        output(
            "read -n1 -d '' first; read -d '' rest; printf '<%s>\\n' $first $rest",
            b"\r\ntail\n"
        ),
        "<\r>\n"
    );
    assert_eq!(
        output("read -n0 value; echo $value", b"whole line\n"),
        "whole line\n"
    );
}

#[test]
fn bare_read_writes_no_extra_delimiter_and_eof_clears_variables() {
    assert_eq!(output("read -z; read", b"one\ntwo\0tail\n"), "one\ntwotail");
    assert_eq!(
        output(
            "set values old; read -a values; echo status:$status count:(count $values)",
            b""
        ),
        "status:1 count:0\n"
    );
}

#[test]
fn delimiter_and_ifs_modes_share_posix_field_rules() {
    assert_eq!(
        output(
            "set -e IFS; read -a values; printf '<%s>\\n' $values",
            b"a b\n"
        ),
        "<a>\n< >\n<b>\n"
    );
    assert_eq!(
        output(
            "set IFS :; read -a values; printf '<%s>\\n' $values",
            b"a::b:\n"
        ),
        "<a>\n<b>\n"
    );
    assert_eq!(
        output("read -ad: values; printf '<%s>\\n' $values", b"a::b:\n"),
        "<a>\n<>\n<b>\n<>\n"
    );
    assert_eq!(
        output(
            "read -d '' first rest; printf '<%s>\\n' $first $rest",
            "文😀ab\n".as_bytes()
        ),
        "<文>\n<😀ab>\n"
    );
}

#[test]
fn failed_records_clear_only_the_remaining_variables() {
    let result = run(
        "set first old; set second old; read -L first second; echo status:$status; printf '%s:%s\\n' $first (count $second)",
        b"saved\n\xff\n",
    );
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "status:1\nsaved:0\n"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("not UTF-8"));
}

#[test]
fn invalid_options_do_not_consume_input_or_abort_the_script() {
    for flags in [
        "-zL",
        "-Lt",
        "-td:",
        "--tokenize --tokenize-raw",
        "-lg",
        "-xu",
        "-n-1",
        "--unknown",
    ] {
        let result = run(
            &format!("read {flags} first; echo status:$status; read next; echo next:$next"),
            b"untouched\n",
        );
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            "status:2\nnext:untouched\n",
            "{flags}"
        );
        assert!(!result.stderr.is_empty());
    }
}

#[test]
fn nul_and_token_reads_survive_final_pipeline_workers() {
    assert_eq!(
        output(
            "printf 'one\\ntwo\\0' | read -lz value; printf '<%s>\\n' $value; printf '%s\\n' \"'a b' c\" | read -lat fields; printf '<%s>\\n' $fields",
            b""
        ),
        "<one\ntwo>\n<a b>\n<c>\n"
    );
}
