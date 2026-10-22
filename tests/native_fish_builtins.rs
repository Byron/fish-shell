#![cfg(windows)]
use std::process::Command;

#[test]
fn fish_native_common_builtins() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            r#"
        printf '[%s] %04d %.2f\n' 'two words' 7 1.25
        math 'sqrt(81) + 1'
        string upper hello
        string join : a b c
        string split -n : a::b
        string sub -s 2 -l 2 a文bc
        string repeat 3 x
        contains -i b a b c
        type -t math
        function builtin_probe; echo function; end
        functions -q builtin_probe; echo $status
        functions -e builtin_probe
        functions -q builtin_probe; echo $status
    "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "[two words] 0007 1.25\n10\nHELLO\na:b:c\na\nb\n文b\nxxx\n2\nbuiltin\n0\n1\n"
    );
}

#[test]
fn fish_native_read_assigns_fields() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            "read first rest; printf '[%s][%s]\n' $first $rest; read -a items; count $items",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        child.stdin.take().as_mut().unwrap(),
        "one two words\nα β γ\n".as_bytes(),
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "[one][two words]\n3\n"
    );
}

#[test]
fn read_list_alias_preserves_local_scope_delimiters_and_empty_input_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            r#"
            set -g words outer
            function read_lists
                printf ' α   β\tγ\n' | read -l --list words
                printf '<%s>\n' $words
                printf 'a::文\n' | read -l --list -d : delimited
                printf '[%s]\n' $delimited
                printf '\n' | read -l --list empty
                printf 'empty:%s:%s\n' $status (count $empty)
                read -l --list eof < NUL
                printf 'eof:%s:%s\n' $status (count $eof)
            end
            read_lists
            printf 'outer:%s\n' $words
            set -q delimited
            echo leaked:$status
        "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "<α>\n<β>\n<γ>\n[a]\n[]\n[文]\nempty:0:0\neof:1:0\nouter:outer\nleaked:1\n"
    );
}

#[test]
fn builtin_inventory_and_queries_report_supported_native_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", "builtin --names"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let names: Vec<_> = output.lines().collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "{names:?}");
    for name in ["echo", "read", "path", "realpath", "_", "__fish_git"] {
        assert!(names.contains(&name), "missing {name}");
    }
    for name in ["if", "while", "function", "alias", "native_missing_builtin"] {
        assert!(!names.contains(&name), "unexpected {name}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            r#"
            builtin -q native_missing_builtin echo
            echo any:$status
            builtin --query native_missing_builtin
            echo missing:$status
            builtin -q
            echo empty:$status
            function native_function_only; end
            builtin -q native_function_only
            echo function:$status
            function echo; printf 'shadow\n'; end
            builtin echo bypass
        "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "any:0\nmissing:1\nempty:1\nfunction:1\nbypass\n"
    );
    for (source, diagnostic) in [
        ("builtin -qn", "mutually exclusive"),
        ("builtin --names=yes", "does not take an argument"),
        ("builtin --unknown", "unknown option"),
        ("builtin native_missing_builtin", "unknown builtin"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fish"))
            .args(["-N", "-c", source])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
