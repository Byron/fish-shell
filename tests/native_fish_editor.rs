#![cfg(windows)]
use std::process::Command;

#[test]
fn set_color_supports_native_prompt_construction_and_reset() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", "printf '%shello%s' (set_color --bold green) (set_color normal); set_color 1af --background black"])
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"\x1b[32;1mhello\x1b[0m\x1b[38;2;17;170;255;40m"
    );
}

#[test]
fn set_color_rejects_invalid_colors_without_printing_them() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", "set_color not-a-color"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown color"));
}

#[test]
fn native_key_bindings_round_trip_and_erase_without_running_actions() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", "bind ctrl-t complete; set saved (bind); bind -q ctrl-t; echo $status; bind --erase --all; bind -q ctrl-t; echo $status; eval $saved; bind -q ctrl-t; echo $status"])
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"0\n1\n0\n");
}
