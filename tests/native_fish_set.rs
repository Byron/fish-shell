//! Native `set` indexes follow the shared POSIX index parser.
#![cfg(windows)]
use std::process::{Command, Output};

fn fish(source: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
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
fn queries_count_missing_positions_and_index_electric_variables() {
    assert_eq!(
        run(r#"
        set values a b c d
        set -q 'values[1 4]'; echo present=$status
        set -q 'values[0 5 5 -9]'; echo missing=$status
        set -q 'absent[1 2]'; echo absent=$status
        set -q values[]; echo empty=$status
        set -q status[99]; echo status_index=$status
        set -q 'pipestatus[1 99]'; echo pipeline_index=$status
        set -q fish_pid[-1]; echo pid_index=$status
        set -q 'values[0x2 03]suffix'; echo radix=$status
    "#),
        "present=0\nmissing=4\nabsent=2\nempty=0\nstatus_index=1\npipeline_index=1\npid_index=0\nradix=0\n"
    );
}

#[test]
fn indexed_erasure_keeps_original_positions_and_fires_set_events() {
    assert_eq!(
        run(r#"
        function watcher --on-variable values
            echo "$argv"
        end
        set values a b c d
        false
        set -e 'values[2 2 -1]'
        echo "$values" $status
        false
        set -e 'values[0 99]'
        echo "$values" $status
        set --no-event -e values[1]
        echo "$values"
        set -e values[]
        set -e absent[1]
        echo absent=$status
    "#),
        "VARIABLE SET values\nVARIABLE SET values\na c 0\nVARIABLE SET values\na c 0\nc\nVARIABLE ERASE values\nabsent=1\n"
    );
}

#[test]
fn indexed_assignments_handle_duplicates_ranges_gaps_and_scope() {
    assert_eq!(
        run(r#"
        set values a b c
        false
        set 'values[2 2 5]' x y z
        echo status=$status
        printf '<%s>\n' $values
        set values[-1..-2] last fourth
        echo "$values"
        set -l local_values a b
        begin
            set -el local_values[1]
            echo inner=$local_values
        end
        echo outer=$local_values
        set -gx --path selected 'C:\a;D:\b'
        begin
            set -lu --unpath selected local1 local2 local3
            set -g selected[2] changed
        end
        set -qxg selected; echo exported=$status
        set -qg --unpath selected; echo unpath=$status
        echo "$selected"
    "#),
        "status=1\n<a>\n<y>\n<c>\n<>\n<z>\na y c fourth last\ninner=b\nouter=a outer=b\nexported=0\nunpath=0\nC:\\a changed\n"
    );
}

#[test]
fn invalid_indexes_preserve_values_and_allow_later_commands() {
    let output = fish(
        r#"
        set values original
        set 'values[0]' changed; echo bounds=$status
        set 'values[1 2]' changed; echo count=$status
        set -a values[1] changed; echo append=$status
        set -e 'values[2 1..]'; echo syntax=$status
        set -q 'values['; echo query=$status
        set 'values[1000001]' changed; echo size=$status
        echo $values
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "bounds=2\ncount=2\nappend=2\nsyntax=1\nquery=1\nsize=2\noriginal\n"
    );
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains("array index out of bounds"));
    assert!(errors.contains("given 2 indexes but 1 values"));
    assert!(errors.contains("Invalid index"));
}

#[test]
fn bundled_subcommand_condition_can_remove_the_command_name_unchanged() {
    assert_eq!(
        run(r#"
        function commandline
            printf '%s\n' $fake_tokens
        end
        set fake_tokens tool --flag
        __fish_use_subcommand; echo options=$status
        set fake_tokens tool --flag subcommand
        __fish_use_subcommand; echo subcommand=$status
    "#),
        "options=0\nsubcommand=1\n"
    );
}
