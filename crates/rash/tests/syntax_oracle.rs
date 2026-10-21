use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::process::Command;

use rash::syntax::{self, SyntaxStatus};

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn syntax_acceptance_matches_bash() {
    let oracle = std::env::var_os("RASH_BASH_ORACLE").expect("RASH_BASH_ORACLE must be set");
    // These inputs exercise the implemented grammar, without executing either interpreter.
    let cases: &[&[u8]] = &[
        b"",
        b"# comment\n",
        b"printf '%s' 'unicode \xc3\xbc' raw-\xff",
        b"echo one\\ two \"$value\" ${value:-default}",
        b"x=one y='two three' command arg >out 2>&1",
        b"true &&\nfalse || :",
        b"! false | true & echo next",
        b"{ echo one; echo two; } >out",
        b"(echo one; echo two)",
        b"if true; then :; elif false; then :; else :; fi",
        b"for x in one 'two three'; do echo \"$x\"; done",
        b"for x; do :; done",
        b"while true; do break; done",
        b"until false; do break; done",
        b"case $x in (one|two) :;; *) :;; esac",
        b"f() { :; }; function g { :; }",
        b"a=(one 'two three' [4]=four)",
        b"(( x += 1 )); echo $((x * (2 + 3)))",
        b"[[ -n \"$v\" && ( x < y || ! z ) ]]",
        b"[[ value =~ ^(foo|bar(a b))$|other && x == y ]]",
        b"echo \"$(case x in (x|y) echo \"${v:-$(echo nested)}\";; *) :;; esac)\"",
        b"cat <<'EOF' 3<<-END\nliteral $(false)\nEOF\n\tsecond\n\tEND\n",
        b"cat <<EOF\none\\\ntwo\nEOF\n",
        b"read -r line <<< 'text'",
        b"echo 'unfinished",
        b"echo ${value",
        b"echo $(echo value",
        b"true |",
        b"if true; then :",
        b"for x in a; do :",
        b"[[ value =~ (foo ]]",
        b")",
        b"echo x ;;",
        b"if ; then :; fi",
        b"{ }",
        b"[[ -n ]]",
        b"[[ x == ]]",
        b"[[ x && ]]",
        b"[[ x unexpected y ]]",
    ];
    for &input in cases {
        let bash = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-n", "-c"])
            .arg(OsStr::from_bytes(input))
            .env_remove("BASH_ENV")
            .output()
            .expect("run Bash syntax oracle");
        let parsed = syntax::check(input);
        assert_eq!(
            matches!(parsed, SyntaxStatus::Complete),
            bash.status.success(),
            "source: {:?}\nrash: {parsed:?}\nbash: {}",
            String::from_utf8_lossy(input),
            String::from_utf8_lossy(&bash.stderr),
        );
    }
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn function_printing_matches_bash() {
    let oracle = std::env::var_os("RASH_BASH_ORACLE").expect("RASH_BASH_ORACLE must be set");
    let cases: &[&[u8]] = &[
        b"f() { printf 'value:%s\\n' \"$x\"; }",
        b"f() { echo one; echo two; }",
        b"f() { echo a && echo b || echo c; echo a | cat & wait; }",
        b"f() { if true; then :; elif false; then echo no; else echo yes; fi; }",
        b"f() { for x in one \"two three\"; do echo \"$x\"; done; for x; do :; done; }",
        b"f() { while true; do :; done; until false; do break; done; }",
        b"f() { case $x in (a|b) echo one;; c) :;; *) echo other;; esac; }",
        b"f() { (echo sub; echo shell); { echo group; }; }",
        b"f() { [[ x == y && z ]]; (( x += 1 )); }",
        b"f() { echo hi >out 2>&1; }",
        b"f() { g() { :; }; g; }",
        b"f() ( echo subshell )",
        b"f() { :; } >out",
        b"f() { cat <<EOF\ntext\nEOF\n}",
        b"f() { cat <<\"END\" >out\ntext\nEND\n}",
        b"f() { cat 3<<-EOF <<ZZ\n\ttext\nEOF\nother\nZZ\n}",
        b"f() { echo >&out; echo <>file; echo <&-; }",
    ];
    for &source in cases {
        let parsed = syntax::parse_one(source).unwrap();
        let syntax::CommandKind::Function { name, body } =
            &parsed.body[0].chain.first.commands[0].kind
        else {
            panic!("expected function");
        };
        let program = [source, b"; declare -f f"].concat();
        let bash = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c"])
            .arg(OsStr::from_bytes(&program))
            .env_remove("BASH_ENV")
            .output()
            .expect("run Bash function-printing oracle");
        assert!(
            bash.status.success(),
            "{}",
            String::from_utf8_lossy(&bash.stderr)
        );
        let actual = syntax::format_function(&name.raw, body);
        assert_eq!(
            actual,
            bash.stdout,
            "source: {}\nrash:\n{}\nbash:\n{}",
            String::from_utf8_lossy(source),
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&bash.stdout)
        );
    }
}
