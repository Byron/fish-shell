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
