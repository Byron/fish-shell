#![cfg(windows)]
use std::io::Write;
use std::process::{Command, Output, Stdio};

mod string_cases;

fn command(source: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
    command
        .args(["-N", "-c", source])
        .env(
            "FISH_NATIVE_CONFIG_HOME",
            std::env::temp_dir().join(format!("fish-string-tests-{}", std::process::id())),
        )
        .env_remove("fish_function_path");
    command
}

fn run(source: &str) -> Output {
    command(source).output().unwrap()
}

fn stdout(source: &str) -> String {
    let output = run(source);
    assert!(
        output.status.success(),
        "{}: {}",
        source,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn validate_case(args: &[&str], status: i32, expected: &str) {
    let output = command("string $argv")
        .arg("--")
        .args(&args[1..])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(status),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        expected,
        "{args:?}"
    );
}

#[test]
fn established_posix_match_cases() {
    string_cases::match_cases(validate_case);
}

#[test]
fn established_posix_replace_cases() {
    string_cases::replace_cases(validate_case);
}

#[test]
fn established_posix_escape_cases() {
    string_cases::escape_cases(validate_case);
}

#[test]
fn named_captures_keep_first_matching_input_and_default_variable_scope() {
    assert_eq!(
        stdout(
            r#"
        string match -raq '(?<letter>[a-z])(?<digit>[0-9])?' 'a1b' 'c2'
        printf 'letter:<%s>\n' $letter
        printf 'digit:<%s>\n' $digit
        string match -rq '(?<letter>nope)' 'a1'
        echo status:$status
        count $letter
        set -g captured global
        function capture
            set -l captured local
            string match -rq '(?<captured>new)(?<temporary>value)' newvalue
            echo $captured $temporary
        end
        capture
        echo $captured
        set -q temporary
        echo status:$status
    "#
        ),
        "letter:<a>\nletter:<b>\ndigit:<1>\ndigit:<>\nstatus:1\n0\nnew value\nglobal\nstatus:1\n"
    );
}

#[test]
fn utf32_indices_groups_and_subject_limits_match_posix() {
    assert_eq!(
        stdout(
            r#"
        string match -ran '(文)(😀)' 'a文😀文😀'
        string match -rg '(.+)fish(.*)' catfishcolor
        string match -rea '(a)' aba
        string match -ram1 a aaa aa
        string replace -ram1 a A aaa aa
        string match -rvm1 'at$' dog cat bat
        string replace -rf --max-matches=1 'at$' aught dog cat bat
    "#
        ),
        "2 2\n2 1\n3 1\n4 2\n4 1\n5 1\ncat\ncolor\naba\na\naba\na\na\na\na\nAAA\ndog\ncaught\n"
    );
}

#[test]
fn invalid_patterns_and_options_set_status_without_stopping_the_script() {
    for (command, diagnostic) in [
        ("string match -r '[' value", "compile error"),
        ("string match --entire --index x x", "mutually exclusive"),
        ("string match -rvg x x", "mutually exclusive"),
        ("string match -reg x x", "mutually exclusive"),
        ("string replace --max-matches=0 a b a", "invalid maximum"),
        ("string match --quiet=yes x x", "does not take an argument"),
        ("string match --max-matches", "requires an argument"),
        ("string match -rq '(?<status>x)' x", "read-only"),
        ("string replace -r '(x)' '$2' x", "substitute error"),
        (
            "string escape --style=unknown value",
            "invalid escape style",
        ),
        (
            "string unescape --style=regex value",
            "invalid escape style",
        ),
    ] {
        let output = run(&format!("{command}; echo status:$status; echo continued"));
        assert!(output.status.success(), "{command}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "status:2\ncontinued\n",
            "{command}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = run("string replace -r '(a)' '$2' no a; echo status:$status");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "no\nstatus:2\n");
    let output = run("string match -r '(*LIMIT_MATCH=1)a+' aa; echo status:$status");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "status:1\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("match limit"));
}

#[test]
fn shared_escaping_roundtrips_script_url_and_variable_styles() {
    assert_eq!(
        stdout(
            r#"
        string escape --style=regex 'a[b].* (x)-$' "a
b"
        string unescape --style=url 'a%20b%2F%E6%96%87'
        string unescape --style=var 'a_20_b_2F_E6_96_87_'
        string unescape 'hello\ world' '\u6587' '\Xc3\Xb6'
        string unescape --style=url bad%gg
        echo status:$status
        string unescape --style=var '_'
        echo status:$status
    "#
        ),
        "a\\[b\\]\\.\\* \\(x\\)\\-\\$\na\\nb\na b/文\na b/文\nhello world\n文\nö\nstatus:1\nstatus:1\n"
    );
}

#[test]
fn pipeline_input_keeps_newlines_and_stops_after_quiet_matches() {
    let mut child = command("string replace -r a A")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"a\r\na").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"A\r\nA");
    assert_eq!(
        stdout(
            r#"
        printf 'dog\ncat\nbat\n' | begin
            string match -rq cat
            string replace -r a A
        end
        printf 'a\0b\n' | string match -r '.*b$' | string escape
    "#
        ),
        "bAt\na\\x00b\n"
    );
}

#[test]
fn direct_input_conflicts_with_arguments_but_inherited_input_does_not() {
    assert_eq!(
        stdout(
            r#"
        begin
            string match x x
        end < NUL
        function inherited
            string match x x
        end
        inherited < NUL
        printf 'y\n' | inherited
        string match x x 0<&-
    "#
        ),
        "x\nx\nx\nx\n"
    );
    for source in [
        "string match x x < NUL; echo status:$status",
        "printf 'y\\n' | string match x x; echo status:$status",
    ] {
        let output = run(source);
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "status:2\n");
        assert!(String::from_utf8_lossy(&output.stderr).contains("too many arguments"));
    }
}

#[test]
fn bundled_completion_helpers_use_shared_regex_operations() {
    assert_eq!(
        stdout(
            r#"
        function commandline
            printf '%s\n' cargo nextest
        end
        __fish_seen_subcommand_from nextest
        echo status:$status
        __fish_seen_subcommand_from 'next.*'
        echo status:$status
        printf '    nextest  Run tests\nnoise\n' | string replace -rf '^\s+([^\s]+)\s*(.*)' '$1\t$2'
    "#
        ),
        "status:0\nstatus:1\nnextest\tRun tests\n"
    );
}
