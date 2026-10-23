#![cfg(windows)]
use std::io::Write as _;
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
fn split_fields_support_ranges_repetition_and_missing_fields() {
    for (args, status, expected) in [
        (
            vec!["string", "split", "-f3,2,2", "", "abc"],
            0,
            "c\nb\nb\n",
        ),
        (
            vec!["string", "split", "--fields=1-3,5,9-7", "", "123456789"],
            0,
            "1\n2\n3\n5\n9\n8\n7\n",
        ),
        (vec!["string", "split", "-f2,9", "", "abc"], 1, ""),
        (vec!["string", "split", "-af2,9", "", "abc"], 0, "b\n"),
        (vec!["string", "split", "-qf9", "", "abc"], 0, ""),
        (
            vec!["string", "split", "-f1", " ", "a b", "c d"],
            0,
            "a\nc\n",
        ),
        (vec!["string", "split", "-f1", ":", "abc"], 1, "abc\n"),
        (
            vec!["string", "split", "-rm1", "-f1", ":", "a:b:c"],
            0,
            "a:b\n",
        ),
        (vec!["string", "split", "-nf2", ":", "a::b"], 0, "b\n"),
    ] {
        validate_case(&args, status, expected);
    }
    assert_eq!(
        stdout("printf '%s\\n' a:b c:d | string split -f2 :"),
        "b\nd\n"
    );
}

#[test]
fn split_field_errors_are_bounded_and_do_not_stop_following_commands() {
    for fields in ["0", "1-0", "-1", "1--2", "1a", "1-2147483647", "1-100000,1"] {
        let output = run(&format!(
            "string split --fields='{fields}' : a:b; echo status:$status"
        ));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "status:2\n",
            "{fields}"
        );
        assert!(!output.stderr.is_empty());
    }
    validate_case(&["string", "split", "--allow-empty", ":", "a:b"], 2, "");
}

#[test]
fn substring_end_uses_shared_unicode_character_ranges() {
    for (args, expected) in [
        (vec!["string", "sub", "--end=3", "abcde"], "abc\n"),
        (vec!["string", "sub", "-e-4", "abcde"], "a\n"),
        (vec!["string", "sub", "-s2", "-e-2", "abcde"], "bc\n"),
        (vec!["string", "sub", "-s4", "-e2", "abcde"], "\n"),
        (vec!["string", "sub", "-s2", "-e3", "a文😀z"], "文😀\n"),
        (
            vec!["string", "sub", "-s-9223372036854775808", "-e99", "abc"],
            "abc\n",
        ),
    ] {
        validate_case(&args, 0, expected);
    }
    validate_case(&["string", "sub", "-e0", "abc"], 2, "");
    validate_case(&["string", "sub", "-e2", "-l1", "abc"], 2, "");
    assert_eq!(stdout("printf abcd | string sub -s2 -e3"), "bc");
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
    let mut child = command("string replace -r a A <&0")
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
            string match -rq cat <&0
            string replace -r a A <&0
        end
        printf 'a\0b\n' | string match -r '.*b$' | string escape
    "#
        ),
        "bAt\na\\x00b\n"
    );
}

#[test]
fn basic_options_accept_compact_grouped_and_long_argument_forms() {
    for (args, status, expected) in [
        (vec!["string", "sub", "-s2", "-l2", "a文😀z"], 0, "文😀\n"),
        (
            vec!["string", "sub", "--start=-2", "--length=1", "abcde"],
            0,
            "d\n",
        ),
        (vec!["string", "sub", "abcde", "-l2"], 0, "ab\n"),
        (vec!["string", "sub", "-qs2", "abcde"], 0, ""),
        (vec!["string", "trim", "-lc_", "__value__"], 0, "value__\n"),
        (
            vec!["string", "trim", "--chars=_", "--right", "__value__"],
            0,
            "__value\n",
        ),
        (vec!["string", "split", "-rm1", ":", "a:b:c"], 0, "a:b\nc\n"),
        (
            vec!["string", "split", "--max=1", "", "a文😀"],
            0,
            "a\n文😀\n",
        ),
        (vec!["string", "split", "-rm1", "", "a文😀"], 0, "a文\n😀\n"),
        (vec!["string", "split", "-n", ":", ":one:"], 1, "one\n"),
        (vec!["string", "split", "-n", ":", "", "a:b"], 1, "a\nb\n"),
        (vec!["string", "split", "-qn", ":", "", "a:b"], 0, ""),
        (vec!["string", "join", "-nq", ",", "", "one", "two"], 0, ""),
        (vec!["string", "lower", "--", "-UPPER"], 0, "-upper\n"),
    ] {
        validate_case(&args, status, expected);
    }
}

#[test]
fn basic_option_errors_do_not_stop_the_script() {
    for (source, diagnostic) in [
        ("string length --chars=x value", "unknown option"),
        ("string lower -N value", "unknown option"),
        ("string sub -n value", "unknown option"),
        ("string trim --left=yes value", "does not take an argument"),
        ("string sub --length", "requires an argument"),
        ("string sub -s0", "indexes start at one"),
        ("string split -m-1 : value", "invalid maximum"),
        (
            "string repeat --count=9223372036854775808 value",
            "invalid count",
        ),
        ("string unknown", "unsupported subcommand"),
    ] {
        let output = run(&format!("{source}; echo status:$status; echo continued"));
        assert!(output.status.success(), "{source}");
        assert_eq!(output.stdout, b"status:2\ncontinued\n", "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn repeat_supports_maximum_only_and_suppresses_only_the_final_newline() {
    for (args, status, expected) in [
        (vec!["string", "repeat", "-m5", "ab"], 0, "ababa\n"),
        (vec!["string", "repeat", "--max=3", "文😀"], 0, "文😀文\n"),
        (vec!["string", "repeat", "-n3", "-m0", "ab"], 0, "ababab\n"),
        (vec!["string", "repeat", "-n0", "-m3", "xy"], 0, "xyx\n"),
        (vec!["string", "repeat", "-m0", "ab"], 1, ""),
        (vec!["string", "repeat", "-n0", "ab"], 1, ""),
        (
            vec!["string", "repeat", "-n2", "", "a", "", "b", ""],
            0,
            "aa\nbb\n",
        ),
        (vec!["string", "repeat", "-Nn2", "a", "b"], 0, "aa\nbb"),
        (
            vec!["string", "repeat", "-n9223372036854775807", "-m3", "xy"],
            0,
            "xyx\n",
        ),
    ] {
        validate_case(&args, status, expected);
    }
}

#[test]
fn basic_streams_preserve_carriage_returns_and_unterminated_final_records() {
    for (operation, expected) in [
        ("lower", b"a\r\nb".as_slice()),
        ("upper", b"A\r\nB".as_slice()),
        ("sub -s1", b"A\r\nb".as_slice()),
        ("length", b"2\n1\n".as_slice()),
        ("join ,", b"A\r,b".as_slice()),
        ("repeat -n2", b"A\rA\r\nbb".as_slice()),
    ] {
        let output = run(&format!("printf 'A\\r\\nb' | string {operation}"));
        assert!(output.status.success(), "{operation}: {:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{operation}: {:?}", output.stderr);
        assert_eq!(output.stdout, expected, "{operation}");
    }
    assert_eq!(stdout("printf ' a \\r\\n b ' | string trim"), "a\nb");
    assert_eq!(
        stdout("printf 'a:b\\r\\nc:d' | string split :"),
        "a\nb\r\nc\nd\n"
    );
}

#[test]
fn inherited_stdin_requires_an_explicit_inner_redirection() {
    assert_eq!(
        stdout(
            r#"
        printf 'untouched\n' | begin
            string length
            echo basic:$status
            string match '*'
            echo match:$status
            string upper <&0
        end
        function inherited
            string trim
            echo function:$status
            string lower <&0
        end
        printf 'INPUT\n' | inherited
    "#,
        ),
        "basic:1\nmatch:1\nUNTOUCHED\nfunction:1\ninput\n"
    );
    for source in [
        "printf 'stream\\n' | string sub -s1 argument; echo status:$status",
        "string length argument < NUL; echo status:$status",
        "string repeat -m2 argument < NUL; echo status:$status",
    ] {
        let output = run(source);
        assert_eq!(output.stdout, b"status:2\n", "{source}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("too many arguments"));
    }
}

#[test]
fn quiet_basic_stream_operations_leave_input_for_the_next_command() {
    assert_eq!(
        stdout(
            r#"
        printf 'first\nsecond\n' | begin
            string length -q <&0
            string upper <&0
        end
        printf 'same\n CHANGE \nlast\n' | begin
            string trim -q <&0
            string upper <&0
        end
        printf 'one\ntwo\nthree\nfour\n' | begin
            string join -q , <&0
            string upper <&0
        end
    "#,
        ),
        "SECOND\nLAST\nFOUR\n"
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
