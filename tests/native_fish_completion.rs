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
        complete -c native_probe -f -a stale -a 'alpha beta'
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
        "alpha\n--mode\tChoose mode\nfast\tChoose mode\nalpha\n"
    );
}

#[test]
fn registration_defers_dynamic_completion_source_until_query() {
    let output = fish("complete -c native_probe -a '(echo MUST_NOT_RUN)'");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
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
        "native_completion_variable\tVariable\nnative_completion_function\tCommand\n"
    );
}

fn output(source: &str) -> String {
    let result = fish(source);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn conditions_are_cached_short_circuited_and_generators_are_lazy() {
    assert_eq!(
        output(
            r#"
        set -g condition_runs 0
        set -g generated 0
        function enabled
            set -g condition_runs (math $condition_runs + 1)
            echo discarded-condition-output
            true
        end
        function generate
            set -g generated (math $generated + 1)
            printf 'fast\tGenerated mode\nslow\n'
        end
        complete -c native_probe -f -n enabled -a alpha
        complete -c native_probe -f -n enabled -a beta
        complete -c native_probe -n false -n 'set -g must_not_run yes' -a wrong
        complete -c native_probe -l mode -x -a '(generate)'
        complete -C 'native_probe a'
        echo $condition_runs:$generated
        complete -C 'native_probe --mode fa'
        echo $condition_runs:$generated
        set -q must_not_run; echo blocked:$status
    "#
        ),
        "alpha\n1:0\nfast\tGenerated mode\n2:1\nblocked:1\n"
    );
}

#[test]
fn completion_mutations_persist_but_registration_snapshot_is_stable() {
    assert_eq!(
        output(
            r#"
        function revise
            set -g live_value changed
            complete -c native_probe -e
            complete -c native_probe -f -a new
            true
        end
        complete -c native_probe -f -n revise -a old
        complete -c native_probe -f -a previous
        complete -C 'native_probe '
        echo live:$live_value
        complete -C 'native_probe '
    "#
        ),
        "old\nprevious\nlive:changed\nnew\n"
    );
}

#[test]
fn commandline_uses_shared_extents_and_never_executes_typed_substitutions() {
    assert_eq!(
        output(
            r#"
        set -g known expanded
        set -l typed 'echo before; thing aa | thing "two words" $known unquoted(set -g typed_ran yes) '
        commandline -I "$typed" -p
        commandline -I "$typed" -j
        commandline -I "$typed" -opc
        commandline -I "$typed" -xpc
        commandline -I 'thing "$(set -g typed_ran yes)" $(echo bad)[2] a(echo bad)b ' -xpc
        commandline -I 'thing "two words" > ignored ' --tokens-raw
        set -q typed_ran; echo safe:$status
    "#
        ),
        concat!(
            " thing \"two words\" $known unquoted(set -g typed_ran yes) \n",
            " thing aa | thing \"two words\" $known unquoted(set -g typed_ran yes) \n",
            "thing\ntwo words\n$known\nunquoted(set -g typed_ran yes)\n",
            "thing\ntwo words\nexpanded\nunquoted(set -g typed_ran yes)\n",
            "thing\n\"$(set -g typed_ran yes)\"\n$(echo bad)[2]\na(echo bad)b\n",
            "thing\n\"two words\"\nsafe:1\n",
        )
    );
}

#[test]
fn completion_helpers_see_wrapped_context_in_pipeline_workers() {
    assert_eq!(
        output(
            r#"
        function wrapped --wraps 'native_probe fixed'
        end
        complete -c native_probe -f -n '__fish_seen_subcommand_from fixed' -a alpha
        complete -c native_probe -f -n '__fish_use_subcommand' -a top
        complete -C 'wrapped '
        complete -C 'native_probe '
        complete -C 'native_probe fixed '
        complete -C 'echo ö; wrapped '
        complete -C 'echo unused | wrapped '
        complete -C 'echo (wrapped '
    "#
        ),
        "alpha\ntop\nalpha\nalpha\nalpha\nalpha\n"
    );
}

#[test]
fn nested_queries_use_the_live_session_without_reader_reentry() {
    assert_eq!(
        output(
            r#"
        complete -c inside -f -a nested
        complete -c outside -f -a '(set -g nested_mutation yes; complete -C "inside ")'
        complete -C 'outside '
        echo $nested_mutation
    "#
        ),
        "nested\nyes\n"
    );
    let recursion = fish(
        r#"
        complete -c recursive_probe -f -a '(complete -C "recursive_probe ")'
        complete -C 'recursive_probe '
    "#,
    );
    assert!(!recursion.status.success());
    assert!(String::from_utf8_lossy(&recursion.stderr).contains("completion recursion depth"));
}

#[test]
fn completion_exit_ends_the_shell_with_the_completion_builtin_status() {
    for registration in ["-n 'exit 23' -a alpha", "-a '(exit 24)'"] {
        let result = fish(&format!(
            "complete -c native_probe -f {registration}; false; complete -C 'native_probe '; echo MUST_NOT_RUN"
        ));
        assert_eq!(result.status.code(), Some(0));
        assert!(!String::from_utf8_lossy(&result.stdout).contains("MUST_NOT_RUN"));
    }
}

#[test]
fn callback_local_scopes_expire_but_caller_variables_can_be_updated() {
    assert_eq!(
        output(
            r#"
        function caller
            set -l existing before
            complete -c native_probe -f -n 'set existing changed; set -l temporary hidden; true' -a '(set -l generated_local hidden; echo yes)'
            complete -C 'native_probe '
            echo $existing
            set -q temporary; echo condition-local:$status
            set -q generated_local; echo generator-local:$status
        end
        caller
    "#
        ),
        "yes\nchanged\ncondition-local:1\ngenerator-local:1\n"
    );
}

#[test]
fn cli_completions_do_not_consume_pending_standard_input() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", "complete -c native_probe -f; complete -c native_probe -n 'read stolen' -a wrong; complete -C 'native_probe '; read pending; echo input:$pending"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"available\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "input:available\n"
    );
}

#[test]
fn windows_generator_crlf_output_produces_plain_choices() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -a '(printf "alpha\r\nbeta\r\n")'
        complete -C 'native_probe '
    "#
        ),
        "alpha\nbeta\n"
    );
}

#[test]
fn cli_completion_events_preserve_channels_and_bypass_query_redirections() {
    let result = fish(
        r#"
        function changed --on-variable completion_trigger
            echo event-output
            echo event-error >&2
        end
        complete -c native_probe -f -n 'set -g completion_trigger yes; true' -a alpha
        complete -C 'native_probe ' >NUL
        set choices (complete -C 'native_probe ')
        echo choice:$choices
    "#,
    );
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "event-output\nevent-output\nchoice:alpha\n"
    );
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "event-error\nevent-error\n"
    );
}
