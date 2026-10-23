#![cfg(windows)]
use std::process::{Command, Stdio};

#[test]
fn documents_options_without_a_console_and_rejects_noninteractive_reads() {
    for argument in ["--help", "--version", "-h", "-v"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fish_key_reader"))
            .arg(argument)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.starts_with(b"fish_key_reader"));
    }
    for arguments in [
        &[][..],
        &["--unknown"],
        &["file"],
        &["-"],
        &["--", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fish_key_reader"))
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
}
