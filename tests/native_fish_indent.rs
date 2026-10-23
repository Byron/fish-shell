#![cfg(windows)]

use std::{
    fs,
    io::Write as _,
    process::{Command, Output, Stdio},
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fish-native-indent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-indent-"))
        {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn indent(args: &[&str], source: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fish_indent"))
        .args(args)
        .env_remove("fish_features")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn formats_shared_grammar_and_is_idempotent() {
    let cases = [
        ("if true;echo hi;end", "if true\n    echo hi\nend\n"),
        (
            "function f --description 'a b';echo $argv;end",
            "function f --description 'a b'\n    echo $argv\nend\n",
        ),
        ("begin\necho 🐟\nend\n", "begin\n    echo 🐟\nend\n"),
        (
            "# hello\n\nif true\n# inside\necho 'quoted space'\nend",
            "# hello\n\nif true\n    # inside\n    echo 'quoted space'\nend\n",
        ),
        ("echo foo > out", "echo foo >out\n"),
        ("echo (begin;echo hi;end)", "echo (begin;echo hi;end)\n"),
        ("if true\necho hi", "if true\n    echo hi\n"),
        ("begin\r\necho hi\r\nend\r\n", "begin\n    echo hi\nend\n"),
        ("echo 'two\nlines'", "echo 'two\nlines'\n"),
    ];
    for (source, expected) in cases {
        let result = indent(&[], source.as_bytes());
        assert!(result.status.success(), "{source:?}: {:?}", result.stderr);
        assert_eq!(result.stdout, expected.as_bytes(), "{source:?}");
        assert_eq!(indent(&[], &result.stdout).stdout, result.stdout);
    }
}

#[test]
fn supports_indentation_only_and_no_indent() {
    assert_eq!(
        indent(&["--no-indent"], b"if true;echo hi;end").stdout,
        b"if true\necho hi\nend\n"
    );
    assert_eq!(
        indent(&["--only-indent"], b"if true\necho  hi\nend\n").stdout,
        b"if true\n    echo  hi\nend\n"
    );
    assert_eq!(
        indent(&["--only-unindent"], b"if true\n    echo  hi\nend\n").stdout,
        b"if true\necho  hi\nend\n"
    );
    assert_eq!(
        indent(&["--only-unindent"], b"if true\n echo  hi\nend\n").stdout,
        b"if true\n echo  hi\nend\n"
    );
}

#[test]
fn checks_and_writes_unicode_file_paths() {
    let temp = Fixture::new();
    let first = temp.0.join("one 🐟.fish");
    let second = temp.0.join("two.fish");
    fs::write(&first, b"begin;echo hi;end").unwrap();
    fs::write(&second, b"echo two").unwrap();
    let paths = [first.to_str().unwrap(), second.to_str().unwrap()];
    let check = indent(&["--check", paths[0], paths[1]], b"");
    assert_eq!(check.status.code(), Some(2));
    assert!(check.stdout.is_empty());
    assert!(
        String::from_utf8(check.stderr)
            .unwrap()
            .contains("one 🐟.fish")
    );
    assert_eq!(fs::read(&first).unwrap(), b"begin;echo hi;end");
    assert!(
        indent(&["--write", paths[0], paths[1]], b"")
            .status
            .success()
    );
    assert_eq!(fs::read(&first).unwrap(), b"begin\n    echo hi\nend\n");
    assert!(
        indent(&["--check", paths[0], paths[1]], b"")
            .status
            .success()
    );
    assert_eq!(indent(&["--check"], b"echo hi").status.code(), Some(1));
    assert!(indent(&["--check"], b"echo hi\n").status.success());
}

#[test]
fn writes_diagnostics_and_highlight_output_without_evaluation() {
    let tree = indent(&["--dump-parse-tree"], b"echo hi");
    assert!(tree.status.success());
    assert_eq!(tree.stdout, b"echo hi\n");
    assert!(
        String::from_utf8(tree.stderr)
            .unwrap()
            .contains("AstSizeMetrics")
    );
    let html = indent(&["--html"], b"echo '<&>'");
    let text = String::from_utf8(html.stdout).unwrap();
    assert!(text.starts_with("<pre><code>"));
    assert!(text.contains("&lt;&amp;&gt;"));
    assert!(text.contains("fish_color_quote"));
    let ansi = indent(&["--ansi"], b"echo hi");
    assert!(ansi.stdout.windows(2).any(|window| window == b"\x1b["));
    let csv = indent(&["--pygments"], "echo 🐟".as_bytes());
    assert!(String::from_utf8(csv.stdout).unwrap().contains("5,6,param"));
    let temp = Fixture::new();
    let file = temp.0.join("must-not-exist");
    let source = format!("echo (echo unsafe > '{}')", file.display());
    assert!(indent(&["--html"], source.as_bytes()).status.success());
    assert!(!file.exists());
}

#[test]
fn cli_errors_and_raw_bytes_are_preserved() {
    for args in [
        &["--no-such-option"][..],
        &["--check=yes"],
        &["--write"],
        &["does-not-exist.fish"],
    ] {
        let result = indent(args, b"");
        assert!(!result.status.success());
        assert!(!result.stderr.is_empty());
    }
    let result = indent(&[], b"echo '\xff'\n");
    assert!(result.status.success());
    assert_eq!(result.stdout, b"echo '\xff'\n");
    assert!(indent(&["--help"], b"").status.success());
    assert!(
        String::from_utf8(indent(&["--version"], b"").stdout)
            .unwrap()
            .starts_with("fish_indent, version ")
    );
}
