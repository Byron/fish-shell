#![cfg(windows)]
use std::process::{Command, Output};

fn fish(source: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap()
}

fn run(source: &str) -> String {
    let output = fish(source);
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
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn named_events_preserve_arguments_duplicates_and_registration_order() {
    assert_eq!(
        run(r#"
            function z --on-event 'event name'
                printf 'z=<%s>\n' $argv
            end
            function a --on-event 'event name'
                echo a
            end
            function repeated --on-event 'event name' --on-event 'event name'
                echo repeated
            end
            emit 'event name' '' 'two words' --flag
            function z --on-event 'event name'
                echo new-z
            end
            emit 'event name'
            function empty_name --on-event ''
                echo empty
            end
            emit ''
            emit -- --unknown-event
            echo result=$status
            "#),
        "z=<>\nz=<two words>\nz=<--flag>\na\nrepeated\nrepeated\na\nrepeated\nrepeated\nnew-z\nempty\nresult=0\n"
    );
    for (source, diagnostic) in [
        ("emit", "expected event name"),
        ("emit --bad-option", "unknown option"),
    ] {
        let output = fish(source);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    let output = fish("emit; echo missing=$status; emit --bad-option; echo option=$status");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "missing=2\noption=2\n"
    );
}

#[test]
fn removal_and_redefinition_invalidate_pending_registrations() {
    assert_eq!(
        run(r#"
            function first --on-event change
                echo first
                function later --on-event change
                    echo replacement
                end
                functions -e removed first
            end
            function later --on-event change
                echo stale
            end
            function removed --on-event change
                echo removed
            end
            emit change
            echo next
            emit change
            functions -e later
            emit change
            function self_remove --on-event self_remove
                echo before
                functions -e self_remove
                emit self_remove
                echo after
            end
            emit self_remove
            "#),
        "first\nnext\nreplacement\nbefore\nafter\n"
    );
}

#[test]
fn named_and_variable_events_can_reenter_active_handlers() {
    assert_eq!(
        run(r#"
            set -g depth 0
            function recursive --on-event recursive
                echo enter-$depth
                if test $depth -lt 2
                    set -g depth (math $depth + 1)
                    emit recursive
                end
                echo leave-$depth
            end
            emit recursive
            function variable_recursion --on-variable watched
                echo value=$watched
                if test $watched -lt 2
                    set -g watched (math $watched + 1)
                end
            end
            set -g watched 0
            echo final=$watched
            "#),
        "enter-0\nenter-1\nenter-2\nleave-2\nleave-2\nleave-2\nvalue=0\nvalue=1\nvalue=2\nfinal=2\n"
    );
}

#[test]
fn variable_events_cover_assignments_without_emitting_internal_bindings() {
    assert_eq!(
        run(r#"
            function watcher --on-variable watched
                echo $argv
            end
            set -g watched same
            set -g watched same
            set --no-event watched quiet
            set -e watched
            set -e watched
            begin
                set -l watched scoped
            end
            set --no-event watched silent
            set -e --no-event watched
            function prefix_watcher --on-variable prefixed
                echo prefix-event=$prefixed $argv
            end
            prefixed=value echo command=$prefixed
            function loop_watcher --on-variable loop_value
                echo loop $argv
            end
            for loop_value in a b
                true
            end
            function bindings --on-variable argv --on-variable named --on-variable captured
                echo unexpected-binding
            end
            function argument -a named
                echo argument=$named
            end
            argument value
            string match -rq '(?<captured>a)' a
            echo captured=$captured
            "#),
        "VARIABLE SET watched\nVARIABLE SET watched\nVARIABLE ERASE watched\nVARIABLE SET watched\nprefix-event=value VARIABLE SET prefixed\ncommand=value\nloop VARIABLE SET loop_value\nloop VARIABLE SET loop_value\nargument=value\ncaptured=a\n"
    );
}

#[test]
fn handlers_preserve_status_and_pipeline_until_the_triggering_command_finishes() {
    assert_eq!(
        run(r#"
            function named --on-event status_event
                echo named=$status pipeline=$pipestatus
                return 77
            end
            function variable --on-variable watched
                echo variable=$status pipeline=$pipestatus $argv
                return 78
            end
            false | true
            emit status_event
            echo emit=$status pipeline=$pipestatus
            false | true
            set watched one
            echo set=$status pipeline=$pipestatus
            false | true
            set --no-event watched quiet
            echo quiet-set=$status pipeline=$pipestatus
            false | true
            set -q watched
            echo query-found=$status pipeline=$pipestatus
            false
            set -e watched
            echo erase=$status pipeline=$pipestatus
            false | true
            set -q missing_event_variable
            echo query=$status pipeline=$pipestatus
            function directory --on-variable PWD
                echo cd=$status pipeline=$pipestatus $argv
            end
            false | true
            builtin cd .
            echo after-cd=$status pipeline=$pipestatus
            function reader --on-variable read_value
                echo read=$status pipeline=$pipestatus $argv
            end
            false
            read -g read_value < Cargo.toml
            echo after-read=$status pipeline=$pipestatus
            "#),
        "named=0 pipeline=1 pipeline=0\nemit=0 pipeline=0\nvariable=0 pipeline=1 pipeline=0 VARIABLE SET watched\nset=0 pipeline=1 pipeline=0\nquiet-set=0 pipeline=1 pipeline=0\nquery-found=0 pipeline=0\nvariable=1 pipeline=1 VARIABLE ERASE watched\nerase=0 pipeline=0\nquery=1 pipeline=1\ncd=0 pipeline=1 pipeline=0 VARIABLE SET PWD\nafter-cd=0 pipeline=0\nread=1 pipeline=1 VARIABLE SET read_value\nafter-read=0 pipeline=0\n"
    );
}

#[test]
fn handlers_ignore_trigger_redirections_and_substitution_capture() {
    assert_eq!(
        run(r#"
            function output --on-event output
                echo event-output
            end
            emit output >NUL
            set capture "$(emit output)"
            echo captured=$capture
            begin
                emit output
            end >NUL
            "#),
        "event-output\nevent-output\ncaptured=\nevent-output\n"
    );
}

#[test]
fn printed_function_definitions_and_workers_preserve_event_properties() {
    assert_eq!(
        run(r#"
            set captured snapshot
            function handler -d 'Event description' --on-variable watched --on-event 'named event' -V captured
                set -g event_result $argv
                echo handler=$captured $argv
            end
            set definition "$(functions handler)"
            functions -e handler
            set captured changed
            eval $definition
            emit 'named event' first | string upper
            set -q event_result
            echo left-worker-result=$status
            true | emit 'named event' 'two words'
            echo result=$event_result
            set watched changed
            "#),
        "HANDLER=SNAPSHOT FIRST\nleft-worker-result=1\nhandler=snapshot two words\nresult=two words\nhandler=snapshot VARIABLE SET watched\n"
    );
}

#[test]
fn handler_failures_do_not_abort_other_handlers_and_exit_still_stops_execution() {
    let output = fish(
        r#"
        function bad --on-event failure
            set --unsupported option
        end
        function good --on-event failure
            echo later
        end
        emit failure
        echo after=$status
        "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "later\nafter=0\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("set --unsupported"));
    let output =
        fish("function stop --on-event stop; exit 37; end; false; emit stop; echo unreachable");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let output = fish(
        "function stop --on-variable watched; exit 37; end; false; set watched value; echo unreachable",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    for command in ["builtin echo unreachable", "not builtin echo unreachable"] {
        let output = fish(&format!(
            "function stop --on-variable prefixed; exit 37; end; false; prefixed=value {command}"
        ));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn unbounded_event_recursion_reports_a_limit_and_unwinds() {
    let output = fish(
        "function recursive --on-event recursive; emit recursive; end; emit recursive; echo after",
    );
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "after\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("maximum Fish evaluation depth exceeded")
    );
}
