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
fn bundled_git_option_specs_preserve_global_routing_arguments() {
    let script = include_str!("../share/completions/git.fish");
    let start = script
        .find("function __fish_git_global_optspecs\n")
        .unwrap();
    let definition = &script[start..start + script[start..].find("\nend").unwrap() + 4];
    assert_eq!(
        run(&format!(
            r#"
        {definition}
        argparse -s (__fish_git_global_optspecs) -- -C 'repo path' -c a=1 -c b=2 --git-dir=storage status --short
        printf 'route=<%s>\n' $_flag_C $_flag_c $_flag_git_dir
        printf 'rest=<%s>\n' $argv
        printf 'opts=<%s>\n' $argv_opts
    "#
        )),
        "route=<repo path>\nroute=<a=1>\nroute=<b=2>\nroute=<storage>\nrest=<status>\nrest=<--short>\nopts=<-C>\nopts=<repo path>\nopts=<-c>\nopts=<a=1>\nopts=<-c>\nopts=<b=2>\nopts=<--git-dir=storage>\n"
    );
}

#[test]
fn optional_accumulating_long_only_and_deleted_flags_share_posix_grammar() {
    assert_eq!(
        run(r#"
        argparse v/verbose q/quiet=+ 'c/color=?' 't/tag=*' long-name= 'd/drop=&' -- -vv --verbose -q first --quiet=second --color --tag=one --tag --long-name=value --drop=gone positional
        printf 'verbose=<%s>\n' $_flag_v $_flag_verbose
        printf 'quiet=<%s>\n' $_flag_q
        printf 'color-count=%s\n' (count $_flag_color)
        printf 'tags=<%s>\n' $_flag_tag
        printf 'long=<%s> drop=<%s>\n' $_flag_long_name $_flag_drop
        printf 'argv=<%s>\n' $argv
        contains -- --drop=gone $argv_opts
        echo dropped=$status
    "#),
        "verbose=<-v>\nverbose=<-v>\nverbose=<--verbose>\nverbose=<-v>\nverbose=<-v>\nverbose=<--verbose>\nquiet=<first>\nquiet=<second>\ncolor-count=0\ntags=<one>\ntags=<>\nlong=<value> drop=<gone>\nargv=<positional>\ndropped=1\n"
    );
}

#[test]
fn unknown_option_policies_preserve_remaining_order_and_option_values() {
    assert_eq!(
        run(r#"
        argparse -i known= -- first --other=two -x value --known=yes last
        printf 'ignore=<%s>\n' $argv
        printf 'known=<%s>\n' $_flag_known
        argparse --move-unknown --unknown-arguments=required known= -- first --other two -x value --known=yes last
        printf 'move=<%s>\n' $argv
        printf 'options=<%s>\n' $argv_opts
        argparse --strict-longopts long-name -- --long 2>/dev/null
        echo strict=$status
    "#),
        "ignore=<first>\nignore=<--other=two>\nignore=<-x>\nignore=<value>\nignore=<last>\nknown=<yes>\nmove=<first>\nmove=<two>\nmove=<value>\nmove=<last>\noptions=<--other>\noptions=<-x>\noptions=<--known=yes>\nstrict=2\n"
    );
}

#[test]
fn failures_preserve_previous_results_and_use_function_name_in_diagnostics() {
    let output = fish(
        r#"
        function usage
            set -l argv before
            set -l _flag_a saved
            argparse -x a,b a b -- -a -b
            echo exclusive=$status
            printf 'argv=<%s> a=<%s>\n' $argv $_flag_a
            argparse --min-args=2 a -- -a only
            echo minimum=$status
            argparse --max-args=0 a -- -a extra
            echo maximum=$status
            argparse a= -- -a
            echo missing=$status
            argparse '' --
            echo invalid=$status
        end
        usage
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "exclusive=2\nargv=<before> a=<saved>\nminimum=1\nmaximum=1\nmissing=2\ninvalid=1\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "usage: a/a b/b: options cannot be used together\nusage: expected >= 2 arguments; got 1\nusage: expected <= 0 arguments; got 1\nusage: -a: option requires an argument\nusage: An option spec must have at least a short or a long flag\n"
    );
}

#[test]
fn successful_results_are_local_to_the_callers_block_without_variable_events() {
    assert_eq!(
        run(r#"
        set -g argv outer
        set -g _flag_x global
        function changed --on-variable _flag_x
            echo unexpected-event
        end
        function outer
            set -l argv caller
            begin
                argparse x -- -x rest
                printf 'inner=<%s> flag=<%s>\n' $argv $_flag_x
            end
            printf 'caller=<%s> flag=<%s>\n' $argv $_flag_x
        end
        outer
        printf 'global=<%s> flag=<%s>\n' $argv $_flag_x
    "#),
        "inner=<rest> flag=<-x>\ncaller=<caller> flag=<global>\nglobal=<outer> flag=<global>\n"
    );
}

#[test]
fn validators_get_exported_context_and_restore_scopes_status_and_results() {
    let output = fish(
        r#"
        set -g global untouched
        function caller
            set -l hidden secret
            set -lx visible exported
            set -l _flag_value old
            set -l argv unchanged
            false
            argparse 'x/value=!echo hidden=(count $hidden) visible=$visible context=$_argparse_cmd:$_flag_name:$_flag_value status=$status; set validator_local temporary; set -g global changed; exit 7' -- --value=sample
            echo failure=$status
            printf 'saved=<%s> argv=<%s> global=<%s>\n' $_flag_value $argv $global
            set -q validator_local
            echo leak=$status
            argparse 'x/value=!test $_flag_value = okay' -- -x okay rest
            echo accepted=$status
            printf 'result=<%s> argv=<%s>\n' $_flag_value $argv
        end
        caller
    "#,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "failure=7\nsaved=<old> argv=<unchanged> global=<changed>\nleak=1\naccepted=0\nresult=<okay> argv=<rest>\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "hidden=0 visible=exported context=caller:value:sample status=1\n"
    );
}

#[test]
fn implicit_integer_flags_use_the_bundled_validator_and_help_leaves_argv_alone() {
    let source = include_str!("../share/functions/_validate_int.fish");
    assert_eq!(
        run(&format!(
            r#"
        {source}
        argparse 'n#number' -- -42 rest
        printf 'number=<%s> argv=<%s>\n' $_flag_number $argv
        argparse 'n#number' -- --number=17
        echo number=$_flag_number
        set -l argv saved
        argparse --help >/dev/null
        echo help=$status argv=$argv
        false
        argparse --
        echo empty=$status
    "#
        )),
        "number=<42> argv=<rest>\nnumber=17\nhelp=0 argv=saved\nempty=0\n"
    );
}
