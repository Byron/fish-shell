//! Differential leaf tests. These do not establish complete shell conformance.

use rash::{
    Error, Result, arithmetic,
    expand::{self, ExpansionContext, Mode},
};
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::process::Command;

struct Context {
    variables: BTreeMap<Vec<u8>, Vec<u8>>,
    cwd: PathBuf,
}
impl arithmetic::Variables for Context {
    fn get(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.variables.get(name).cloned())
    }
    fn set(&mut self, name: &[u8], value: Vec<u8>) -> Result<()> {
        self.variables.insert(name.to_vec(), value);
        Ok(())
    }
}
impl ExpansionContext for Context {
    fn positional(&self) -> Vec<Vec<u8>> {
        vec![b"a b".to_vec(), Vec::new(), b"c".to_vec()]
    }
    fn arg0(&self) -> Vec<u8> {
        b"rash".to_vec()
    }
    fn status(&self) -> i32 {
        0
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn last_pid(&self) -> Option<u32> {
        None
    }
    fn cwd(&self) -> Vec<u8> {
        self.cwd.as_os_str().as_bytes().to_vec()
    }
    fn command_substitute(&mut self, _: &[u8]) -> Result<Vec<u8>> {
        Err(Error::new(
            "leaf oracle cases do not execute command substitutions",
        ))
    }
}

fn oracle() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("RASH_BASH_ORACLE").expect("set RASH_BASH_ORACLE"));
    let version = Command::new(&path)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "printf '%s' \"$BASH_VERSION\"",
        ])
        .output()
        .unwrap();
    assert!(
        version.stdout.starts_with(b"3.2.57("),
        "requires Bash 3.2.57"
    );
    path
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn word_expansion_matches_bash() {
    let oracle = oracle();
    let cwd = std::env::temp_dir().join(format!(
        "rash-expansion-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&cwd).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(cwd.clone());
    for name in ["alpha", "a b", "a;$(x)", "é", "🐟", ".hidden"] {
        std::fs::write(cwd.join(name), b"").unwrap();
    }
    std::fs::create_dir(cwd.join("dir")).unwrap();
    std::fs::write(cwd.join("dir/child"), b"").unwrap();
    let words = [
        "$x",
        "\"$x\"",
        "pre${x}post",
        "\"pre${x}post\"",
        "${empty}",
        "\"${empty}\"",
        "${missing:-a b}",
        "${missing:-\"a b\"}",
        "\"${missing:-a b}\"",
        "${missing:=a b}",
        "${empty-default}",
        "${empty:-default}",
        "${x+yes}",
        "${missing+yes}",
        "${x:+\"yes no\"}",
        "${x#* }",
        "${x##* }",
        "${x%:*}",
        "${x%%:*}",
        "${x//:/X}",
        "${x/:/X}",
        "${x:1:3}",
        "${x: -2}",
        "${#x}",
        "\"$@\"",
        "$@",
        "pre\"$@\"post",
        "\"$*\"",
        "$*",
        "${2}",
        "$2",
        "$10",
        "$x[0]",
        "'a b'",
        "a\\ b",
        "$'a\\xff'",
        "$'a\\n b'",
        "x{a,b}{2..1}",
        "{a,b{c,d}}",
        "{01..03}",
        "\\{a,b\\}",
        "\"{a,b}\"",
        "a*",
        "a\\ *",
        "[ad]*",
        "?",
        "é",
        "🐟",
        "dir/*",
        "*/",
        "missing*",
        "\"*\"",
        ".*",
    ];
    for word in words {
        let mut context = Context {
            variables: BTreeMap::from([
                (b"x".to_vec(), b"a b::c".to_vec()),
                (b"empty".to_vec(), Vec::new()),
            ]),
            cwd: cwd.clone(),
        };
        let actual = expand::expand_word(word.as_bytes(), &mut context, Mode::Word).unwrap();
        let source = format!(
            "set -- 'a b' '' c; x='a b::c'; empty=; unset missing; for word in {word}; do printf '%s\\0' \"$word\"; done"
        );
        let output = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c", &source, "rash"])
            .env("LC_ALL", "C")
            .current_dir(&cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{word}: {:?}", output);
        let expected: Vec<_> = if output.stdout.is_empty() {
            Vec::new()
        } else {
            output.stdout[..output.stdout.len() - 1]
                .split(|&c| c == 0)
                .map(|v| v.to_vec())
                .collect()
        };
        assert_eq!(actual, expected, "word: {word}");
    }
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn arithmetic_matches_bash() {
    let oracle = oracle();
    for expression in [
        "",
        "1+2*3",
        "-2**2",
        "2**3**2",
        "0 && 1/0",
        "1 || 1/0",
        "0?1/0:8",
        "n=2,n++ + ++n*3",
        "n+=3",
        "n<<=2",
        "n=7,n%=3",
        "n=1,n|=8,n^=3",
        "n=3,n&=2",
        "~0",
        "1<<63",
        "-8>>2",
        "64#_",
        "36#Z",
        "16#ff",
        "010",
        "9223372036854775807+1",
        "a=n+1,a+2",
        "1?2:3?4:5",
        "n=4,n-- + n",
        "n=n=8",
        "++(n)",
        "--1",
        "n=1,-n++",
        "(n)=1",
        "(n)++",
        "1?2:n=3",
    ] {
        let mut context = Context {
            variables: BTreeMap::from([(b"n".to_vec(), b"4".to_vec())]),
            cwd: std::env::temp_dir(),
        };
        let actual = arithmetic::eval(expression.as_bytes(), &mut context);
        let source = format!("n=4; printf '%s\\n' \"$(({expression}))\" \"$n\"");
        let output = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c", &source])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            actual.is_ok(),
            "{expression}: {:?}, {:?}",
            output,
            actual
        );
        let Ok(actual) = actual else {
            continue;
        };
        let expected = format!(
            "{actual}\n{}\n",
            String::from_utf8_lossy(context.variables.get(b"n".as_slice()).unwrap())
        );
        assert_eq!(
            output.stdout,
            expected.as_bytes(),
            "arithmetic: {expression}"
        );
    }
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn printf_matches_bash() {
    let oracle = oracle();
    let cases: &[(&[u8], &[&[u8]])] = &[
        (b"%s", &[b"abc", b"\xff"]),
        (b"%s:%s", &[b"only"]),
        (b"%b%s", &[b"before\\cafter", b"ignored"]),
        (b"[%10b]", &[b""]),
        (b"%b", &[b"\\1\\01\\001\\0001\\x41\\'\\?\\z"]),
        (b"%s:%b", &[b"\xff", b"a\\000b"]),
        (b"%c", &[]),
        (b"<%5.2s>", &[b"abc"]),
        (b"[%*.*s]", &[b"-6", b"3", b"abcde"]),
        (b"%#08x", &[b"255"]),
        (b"%d ", &[b"010", b"-2", b"0xff", b"'A"]),
        (b"%*.*f", &[b"8", b"2", b"1.25"]),
        (b"%g", &[b"1000000"]),
        (b"%+08d", &[b"-12", b"12"]),
        (b"%#.0o", &[b"0"]),
        (b"%u", &[b"-1"]),
        (b"%d", &[b"123bad"]),
        (b"%d", &[b"9999999999999999999999999999"]),
        (b"%d", &[b"08"]),
        (b"%d:%u:%f", &[b"'\xff", b"'\xff", b"'\xff"]),
        (b"literal:%Z:ignored", &[b"test"]),
        (b"%q|", &[b"", b"a b", b"a'b", b"\xff\n", b"~", b"#comment"]),
        (b"\\x41\\101\\n%q", &[b"done"]),
        (b"%a", &[b"1.5"]),
        (b"%.3e", &[b"0x1.4p2"]),
        (b"%.4g", &[b"12345.67", b"0.0000123456"]),
        (b"%#.0f", &[b"12.0"]),
        (b"%F", &[b"inf"]),
    ];
    for (format, args) in cases {
        let args: Vec<_> = args.iter().map(|v| v.to_vec()).collect();
        let mut context = Context {
            variables: BTreeMap::new(),
            cwd: std::env::temp_dir(),
        };
        let actual = rash::builtins_printf::format(format, &args, &mut context).unwrap();
        let output = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c", "printf -- \"$@\"", "rash"])
            .arg(std::ffi::OsStr::from_bytes(format))
            .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert_eq!(
            actual.bytes, output.stdout,
            "format: {format:?}, args: {args:?}"
        );
        assert_eq!(
            actual.status,
            output.status.code().unwrap(),
            "format: {format:?}, args: {args:?}"
        );
    }
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn extended_patterns_match_bash() {
    let oracle = oracle();
    for pattern in [
        "@(a|b)",
        "+(a|b|c)",
        "*(a|bc)",
        "?(a|b)",
        "!(a|b)",
        "a!(b)c",
        "!(foo|bar)*",
        "@(@(a|b)|c)",
        "+(a|?(b))",
        "*(@(a|b)c)",
        "@(a|)",
        "+(|a)",
        "@(a\\|b|c)",
        "@(a[)]|b)",
    ] {
        for value in [
            "", "a", "b", "c", "abc", "aaaa", "bc", "ac", "abcc", "foo", "foobar", "a|b", "a)",
        ] {
            let expected = Command::new(&oracle)
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "shopt -s extglob; [[ $1 == $2 ]]",
                    "rash",
                    value,
                    pattern,
                ])
                .env("LC_ALL", "C")
                .status()
                .unwrap();
            assert_eq!(
                expand::pattern_matches(pattern.as_bytes(), value.as_bytes()),
                expected.success(),
                "pattern {pattern:?}, value {value:?}"
            );
        }
    }
}

#[test]
#[ignore = "requires RASH_BASH_ORACLE pointing to Bash 3.2.57"]
fn expansion_error_status_matches_bash() {
    let oracle = oracle();
    for word in [
        "${missing?problem}",
        "${missing:?problem}",
        "${}",
        "${x:0:-1}",
        "$((1/0))",
        "$((2**-1))",
        "$((08))",
    ] {
        let mut context = Context {
            variables: BTreeMap::from([(b"x".to_vec(), b"abc".to_vec())]),
            cwd: std::env::temp_dir(),
        };
        let error = expand::expand_word(word.as_bytes(), &mut context, Mode::Word).unwrap_err();
        let source = format!("x=abc; unset missing; : {word}");
        let output = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c", &source])
            .output()
            .unwrap();
        assert_eq!(Some(error.status), output.status.code(), "word {word}");
    }
    for expression in ["1/0", "2**-1", "08", "1+", "n="] {
        let mut context = Context {
            variables: BTreeMap::new(),
            cwd: std::env::temp_dir(),
        };
        let error = arithmetic::eval(expression.as_bytes(), &mut context).unwrap_err();
        let source = format!(": $(({expression}))");
        let output = Command::new(&oracle)
            .args(["--noprofile", "--norc", "-c", &source])
            .output()
            .unwrap();
        assert_eq!(
            Some(error.status),
            output.status.code(),
            "arithmetic {expression}"
        );
    }
}
