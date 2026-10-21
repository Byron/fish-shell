#![cfg(windows)]
use std::process::Command;

fn fish(source: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .output()
        .unwrap()
}

#[test]
fn registrations_are_queryable_erasable_and_round_trip_as_fish_source() {
    let output = fish(
        r#"
        complete -c native_probe -f -a 'alpha beta'
        complete -c native_probe -l mode -r -a 'fast slow' -d 'Choose mode'
        complete -C 'native_probe al'
        complete -C 'native_probe --mo'
        complete -C 'native_probe --mode fa'
        set saved (complete)
        complete -c native_probe -e
        complete -C 'native_probe --mo'
        for definition in $saved; eval $definition; end
        complete -C 'native_probe al'
    "#,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text,
        "alpha\n--mode  (Choose mode)\nfast  (Choose mode)\nalpha\n"
    );
}

#[test]
fn registration_does_not_execute_dynamic_completion_source() {
    let output = fish("complete -c native_probe -a '(echo MUST_NOT_RUN)'");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("dynamic completion"));
}

#[test]
fn completion_sees_current_session_variables_and_functions() {
    let output = fish(
        "set native_completion_variable value; function native_completion_function; end; complete -C 'echo $native_completion_v'; complete -C 'native_completion_f'",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "native_completion_variable  (Variable)\nnative_completion_function  (Command)\n"
    );
}
