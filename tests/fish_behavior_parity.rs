//! Identical source, output and status expectations for both execution backends.

#[path = "support/fish_fixture.rs"]
mod fish_fixture;

use fish_fixture::FishFixture;

#[test]
fn function_scope_bypasses_block_shadows_and_preserves_global_values() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g values global
            function probe
                set -fx values function original
                begin
                    set -l values block two
                    set -qf values; echo query:$status
                    set -fa values appended
                    echo block:$values
                    set -f values[2] changed
                    set -qfx values; echo exported:$status
                end
                echo function:$values
                set -ef values[1]
                echo erased:$values
            end
            probe
            echo global:$values
            "#
        ),
        "query:0\nblock:block block:two\nexported:0\nfunction:block function:changed function:appended\nerased:changed erased:appended\nglobal:global\n"
    );
}

#[test]
fn function_scope_survives_source_and_transparent_helper_calls() {
    let fixture = FishFixture::new();
    fixture.write("scope.fish", "set -f saved from-source\n");
    assert_eq!(
        fixture.run(
            r#"
            function helper --no-scope-shadowing
                set -fa saved from-helper
                set implicit from-helper
                set -l private_value hidden
            end
            function caller
                begin
                    set -l saved block
                    source scope.fish
                    helper
                    echo block:$saved
                end
                echo function:$saved
                echo implicit:$implicit
                set -q private_value; echo private:$status
            end
            caller
            set -q saved; echo escaped:$status
            begin
                set -f top outermost
            end
            echo top:$top
            set -qg top; echo global:$status
            "#
        ),
        "block:block\nfunction:block function:from-helper\nimplicit:from-helper\nprivate:1\nescaped:1\ntop:outermost\nglobal:1\n"
    );
}

#[test]
fn function_scoped_reads_in_final_pipeline_stage_update_the_caller() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g fields global
            function probe
                begin
                    set -l fields block
                    printf 'one:two::four\n' | read -fa -d : fields
                    echo read:$status
                    echo block:$fields
                end
                printf '<%s>\n' $fields
                printf '' | read -f fields
                echo eof:$status count:(count $fields)
            end
            probe
            echo global:$fields
            "#
        ),
        "read:0\nblock:block\n<one>\n<two>\n<>\n<four>\neof:1 count:0\nglobal:global\n"
    );
}

#[test]
fn explicit_records_survive_mixed_output_pipelines_and_quoted_slices() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set fields (printf prefix; path basename dir/alpha dir/beta)
            printf '<%s>\n' $fields
            set fields (printf 'one\ntwo\0\0last\0' | string split0)
            printf '<%s>\n' $fields
            printf 'slice:<%s>\n' "$(printf 'alpha\0beta\0gamma\0' | string split0)[2..3]"
            set fields (path extension a b.txt c)
            printf 'extension:<%s>\n' $fields
            "#
        ),
        "<prefix>\n<alpha>\n<beta>\n<one\ntwo>\n<>\n<last>\nslice:<beta\ngamma>\nextension:<>\nextension:<.txt>\nextension:<>\n"
    );
}

#[test]
fn argparse_validates_exclusive_options_and_preserves_quoted_arguments() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            function parse
                argparse --exclusive a,b 'a/alpha' 'b/beta' 'n/name=' 'I/include=+' -- $argv
                or return
                printf 'name:<%s>\n' $_flag_name
                printf 'include:<%s>\n' $_flag_include
                printf 'arg:<%s>\n' $argv
            end
            parse -n 'two words' -I first --include=second -- 'literal*' --flag
            parse -ab 2>/dev/null; echo exclusive:$status
            "#
        ),
        "name:<two words>\ninclude:<first>\ninclude:<second>\narg:<literal*>\narg:<--flag>\nexclusive:2\n"
    );
}

#[test]
fn regex_captures_display_width_and_escape_round_trips_agree() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            string match -rg '(a)(b+)' abb
            string replace -ra '(a)(b+)' '$2:$1' abb_ab
            set escaped (string escape -- 'two words' 'a;echo wrong' 'quote\'slash\\')
            string unescape -- $escaped
            set -g fish_emoji_width 2
            string length --visible -- (printf '\e[31mabc\e[0m') '文😀'
            string pad --width 5 --char . -- abc
            "#
        ),
        "a\nbb\nbb:a_b:a\ntwo words\na;echo wrong\nquote'slash\\\n3\n4\n..abc\n"
    );
}

#[test]
fn function_redirects_do_not_replace_internal_pipe_descriptors() {
    let fixture = FishFixture::new();
    fixture.write("input", "outer input\n");
    assert_eq!(
        fixture.run(
            r#"
            function probe
                printf 'inner input\n' | read -l value
                printf '<%s>\n' $value > result
            end
            probe < input > discarded
            string collect -N < result
            "#
        ),
        "<inner input>\n"
    );
}

#[test]
fn inherited_duplication_is_resolved_before_inner_pipes_and_explicit_redirects() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            function emit_both
                echo normal
                echo diagnostic >&2
            end
            function outer
                emit_both | string upper > piped
            end
            outer > inherited 2>&1
            string collect -N < piped
            string collect -N < inherited
            function explicit
                echo explicit > override | read -l value
                echo empty:(count $value) > explicit-result
            end
            explicit > discarded
            string collect -N < override
            string collect -N < explicit-result
            "#
        ),
        "NORMAL\ndiagnostic\nexplicit\nempty:0\n"
    );
}
