//! Function destinations must bypass intervening block and source locals.
#![cfg(windows)]
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn fish(source: &str, input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn output(source: &str, input: &[u8]) -> String {
    let result = fish(source, input);
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn function_scope_bypasses_blocks_at_top_level_and_in_functions() {
    assert_eq!(
        output(
            r#"
            set -g item global
            begin
                set -l item block
                set -f item outer
                echo visible:$item
                set -qf item; echo function:$status
            end
            echo top:$item
            set -qg item; echo global:$status
            function probe
                begin
                    set -l item inner
                    set --function item function
                    echo visible:$item
                    set -ef item
                    set -qf item; echo erased:$status
                    set -f item recreated
                end
                echo function:$item
            end
            probe
            echo top:$item
            set -ef item
            echo global:$item
            set -qf status; echo electric:$status
            "#,
            b"",
        ),
        "visible:block\nfunction:0\ntop:outer\nglobal:0\nvisible:inner\nerased:1\nfunction:recreated\ntop:outer\nglobal:global\nelectric:1\n"
    );
}

#[test]
fn unshadowed_calls_sources_and_substitutions_keep_the_function_boundary() {
    assert_eq!(
        output(
            r#"
            function shared --no-scope-shadowing
                set -f retained changed
                set implicit default
            end
            function private
                set -qf retained; echo hidden:$status
                set -f retained private
            end
            function outer
                set -f retained original
                begin
                    set -l retained block
                    shared
                    echo block:$retained
                end
                private
                echo retained:$retained implicit:$implicit
                set ignored (begin; set -f retained substitution; end)
                source
                echo retained:$retained sourced:$sourced
            end
            outer
            set -q retained; echo gone:$status
            "#,
            b"begin\n set -l retained source-block\n set -f retained source-function\n set -f sourced yes\n echo source:$retained\nend\n",
        ),
        "block:block\nhidden:1\nretained:changed implicit:default\nsource:source-block\nretained:source-function sourced:yes\ngone:1\n"
    );
}

#[test]
fn append_uses_visible_values_but_slices_and_export_use_the_destination() {
    assert_eq!(
        output(
            r#"
            set -gx items global inherited
            function probe
                begin
                    set -l items block two
                    set -f items[2] changed
                    set -qf items[2]; echo slice:$status
                    set -fa items added
                    set -qfx items; echo exported:$status
                    set -qfu items; echo unexported:$status
                end
                echo values:$items
                set -fx items exported
                function child
                    set -qfx items; echo inherited:$status
                    echo child:$items
                end
                child
            end
            probe
            echo global:$items
            "#,
            b"",
        ),
        "slice:0\nexported:1\nunexported:0\nvalues:block values:two values:added\ninherited:0\nchild:exported\nglobal:global global:inherited\n"
    );
}

#[test]
fn function_names_list_only_the_selected_scope() {
    // POSIX currently omits function-scoped names; native follows the documented
    // scope selector instead of preserving that unrelated listing omission.
    assert_eq!(
        output(
            r#"
            set -g global_only yes
            function probe
                set -f function_only yes
                begin
                    set -l block_only yes
                    set -fn | string match '*_only'
                end
            end
            probe
            "#,
            b"",
        ),
        "function_only\n"
    );
}

#[test]
fn read_function_scope_handles_pipeline_array_exports_and_eof() {
    assert_eq!(
        output(
            r#"
            function probe
                begin
                    set -l values block
                    printf 'one two\n' | read --function --array values
                    echo visible:$values
                end
                echo values:$values
                read -fx exported
                set -qfx exported; echo exported:$status
                read -f values
                echo eof:$status count:(count $values)
                echo value:$exported
            end
            probe
            set -q values; echo gone:$status
            "#,
            b"saved\n",
        ),
        "visible:block\nvalues:one values:two\nexported:0\neof:1 count:0\nvalue:saved\ngone:1\n"
    );
}

#[test]
fn conflicting_scope_flags_fail_before_reading_or_changing_values() {
    let result = fish(
        "set -f item original; set -fl item changed; echo set:$status; read -fg item; echo read:$status; read -f item; echo value:$item",
        b"untouched\n",
    );
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "set:2\nread:2\nvalue:untouched\n"
    );
    let errors = String::from_utf8(result.stderr).unwrap();
    assert!(errors.contains("scopes conflict"), "{errors}");
    assert!(errors.contains("multiple variable scopes"), "{errors}");
}
