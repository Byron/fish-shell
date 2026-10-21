#![cfg(windows)]

use std::process::{Command, Output};

fn fish(source: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .output()
        .unwrap()
}

fn stdout(source: &str) -> String {
    let output = fish(source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn literal_abbreviations_can_be_defined_queried_replaced_and_erased() {
    assert_eq!(
        stdout(
            "abbr --add gco git checkout; \
             abbr --query absent gco; printf 'query:%s\\n' $status; \
             abbr --add gco git switch; abbr --show gco; \
             abbr --erase gco; printf 'erase:%s\\n' $status; \
             abbr --query gco; printf 'missing:%s\\n' $status"
        ),
        "query:0\nabbr --add -- 'gco' 'git switch'\nerase:0\nmissing:1\n"
    );
}

#[test]
fn abbreviation_listing_round_trips_quotes_and_backslashes() {
    let definition = r#"abbr --add saved 'printf "%s\n" "C:\work\fish"'"#;
    let expected = stdout(&format!("{definition}; abbr --show saved"));
    assert_eq!(
        stdout(&format!(
            "{definition}; set registration (abbr --show saved); \
             abbr --erase saved; eval $registration; abbr --show saved"
        )),
        expected
    );
}

#[test]
fn abbreviations_do_not_change_noninteractive_command_execution() {
    assert_eq!(
        stdout(
            "function gco; echo original-function; end; \
             abbr --add gco echo expanded-text; gco; echo gco"
        ),
        "original-function\ngco\n"
    );
}

#[test]
fn dynamic_and_argument_position_abbreviations_report_the_native_limit() {
    for source in [
        "abbr --add gco --function generate",
        "abbr --add --regex 'g.*' generated 'git status'",
        "abbr --add --position anywhere gco 'git checkout'",
    ] {
        let output = fish(source);
        assert!(!output.status.success(), "{source}");
        assert!(output.stdout.is_empty(), "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("native abbreviations"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn standard_keymap_commands_set_the_fish_mode_variable() {
    assert_eq!(
        stdout(
            "fish_vi_key_bindings; echo $fish_key_bindings; \
             fish_default_key_bindings; echo $fish_key_bindings"
        ),
        "fish_vi_key_bindings\nfish_default_key_bindings\n"
    );
}
