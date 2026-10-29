#![cfg(windows)]
use std::process::Command;

fn fish(source: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .output()
        .unwrap()
}

struct Directory(std::path::PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fish-completion-options-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn query(&self, source: &str) -> String {
        let result = Command::new(env!("CARGO_BIN_EXE_fish"))
            .args(["-N", "-c", source])
            .current_dir(self.path())
            .env("FISH_NATIVE_CONFIG_HOME", self.path().join("config"))
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(result.stderr.is_empty(), "{result:?}");
        String::from_utf8(result.stdout).unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn shared_directory_helpers_receive_plain_files_and_keep_explicit_descriptions() {
    let directory = Directory::new();
    std::fs::create_dir(directory.path().join("nested")).unwrap();
    assert_eq!(
        directory.query(
            r#"
        complete -C 'echo nest'
        __fish_complete_directories nest
        complete -c native_probe -x -a '(__fish_complete_directories)'
        complete -C 'native_probe nest'
        complete -c native_probe -l directory -x -a '(__fish_complete_directories)'
        complete -C 'native_probe --directory=nest'
        complete -c described -f -a 'nested/' -d 'My directory'
        complete -C 'described nest'
    "#
        ),
        "nested/\nnested/\tDirectory\nnested/\tDirectory\n--directory=nested/\tDirectory\nnested/\tMy directory\n"
    );
}

#[test]
fn shared_process_extents_complete_quoted_substitutions_and_continued_lines() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -a choice
        complete -C 'echo "$(native_probe ch'
        complete -C 'echo "$(echo x | native_probe ch'
        complete -C 'echo (echo "$(native_probe ch'
        complete -C "native_probe \\\n ch"
        complete -C "native_probe \\\r\n ch"
        function native_wrapper --wraps native_probe; end
        complete -C 'echo "$(native_wrapper ch'
    "#
        ),
        "choice\nchoice\nchoice\nchoice\nchoice\nchoice\n"
    );
}

#[test]
fn completion_matching_uses_fish_smartcase_and_natural_order() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -a 'item10 item2 Item3 item1'
        complete -C 'native_probe item'
        complete -C 'native_probe Item'
        complete -c native_probe -e
        complete -c native_probe -f -a 'xAlpha alphaTail'
        complete -C 'native_probe Alpha'
        complete -c native_probe -e
        complete -c native_probe -f -a 'axb axB'
        complete -C 'native_probe ab'
    "#
        ),
        "item1\nitem2\nitem10\nItem3\nItem3\nalphaTail\naxb\n"
    );
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
fn repeated_aliases_and_grouped_short_options_share_their_arguments() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f
        complete -c native_probe -s ab -s c -l alpha -l beta -o old -o older -d Flag
        complete -C 'native_probe -'
        complete -C 'native_probe -a'
        complete -c native_probe -e -s ac -l beta -o older
        complete -C 'native_probe -'
        complete -c native_probe -s I -x -a 'include imports'
        complete -C 'native_probe -bIim'
        complete -C 'native_probe -bI im'
        complete -C 'native_probe -bI'
    "#
        ),
        concat!(
            "-a\tFlag\n-b\tFlag\n-c\tFlag\n-old\tFlag\n-older\tFlag\n--alpha\tFlag\n--beta\tFlag\n",
            "-ab\tFlag\n-ac\tFlag\n",
            "-b\tFlag\n-old\tFlag\n--alpha\tFlag\n",
            "-bIimports\nimports\n-bIimports\n-bIinclude\n",
        )
    );
}

#[test]
fn optional_long_arguments_require_equals_and_required_arguments_claim_the_token() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -a positional
        complete -c native_probe -l maybe -a 'yes no'
        complete -c native_probe -l must -x -a 'yes no'
        complete -C 'native_probe --may'
        complete -C 'native_probe --maybe y'
        complete -C 'native_probe --maybe=y'
        complete -C 'native_probe --must '
        complete -C 'native_probe -- --m'
    "#
        ),
        "--maybe\n--maybe=\n--maybe=yes\nno\nyes\n"
    );
}

#[test]
fn keep_order_preserves_newest_groups_before_sorted_candidates() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -fa 'alpha delta bravo'
        complete -c native_probe -fka 'golf charlie echo'
        complete -c native_probe -fka 'india foxtrot hotel'
        complete -c native_probe -fa 'kilo juliett lima'
        complete -C 'native_probe '
    "#
        ),
        "india\nfoxtrot\nhotel\ngolf\ncharlie\necho\nalpha\nbravo\ndelta\njuliett\nkilo\nlima\n"
    );
}

#[test]
fn positional_commands_abbreviated_options_and_filtered_definitions_round_trip() {
    assert_eq!(
        output(
            r#"
        complete native_probe -f --au --una -s ab --l alpha -a one
        complete native_other -f -a two
        set saved (complete native_probe)
        echo definitions:(count $saved)
        complete native_probe -e
        for definition in $saved; eval $definition; end
        complete -C'native_probe --alpha=o'
        complete -c ignored -C 'native_other t'
        complete -c native_probe -e -s ab -l alpha
        echo erased:(count (complete native_probe))
    "#
        ),
        "definitions:3\n--alpha=one\ntwo\nerased:0\n"
    );
}

#[test]
fn completion_wrappers_keep_context_and_can_be_erased_independently() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -n '__fish_seen_subcommand_from fixed' -a wrapped
        complete -c native_outer -f -w 'native_probe fixed'
        complete -C 'native_outer w'
        complete -C 'command native_outer w'
        complete -c native_outer -e -w 'native_probe fixed'
        complete -C 'native_outer w'
        complete -c native_outer -w 'native_probe fixed'
        complete -c native_probe -w native_outer
        complete -C 'native_outer w'
        set saved (complete native_outer)
        echo wrappers:(count $saved)
        complete -c native_outer -e
        echo erased:(count (complete native_outer))
        complete -C 'native_outer w'
    "#
        ),
        "wrapped\nwrapped\nwrapped\nwrappers:3\nerased:0\n"
    );
}

#[test]
fn explicit_registrations_and_erase_replace_native_suggestions() {
    assert_eq!(
        output(
            r#"
        complete -c cargo -e
        complete -c cargo -f -a 'custom check' -d Script
        complete -C 'cargo ch'
        complete -c cargo -e
        complete -C 'cargo chec'
        complete -c git -e
        complete -c git -f -a custom
        complete -C 'git cu'
        complete -c git -e
        complete -C 'git checko'
    "#
        ),
        "check\tScript\ncustom\n"
    );
}

#[test]
fn malformed_registrations_fail_before_changing_state() {
    for flags in [
        "-fF",
        "-xF",
        "-s ''",
        "-l ''",
        "-o ''",
        "--color invalid",
        "-c one two",
        "-C one two",
    ] {
        let result = fish(&format!("complete -c native_probe {flags}"));
        assert_eq!(
            result.status.code(),
            Some(2),
            "{flags}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    for flags in ["-n 'if true'", "-a '(echo unclosed'", "-a '\"unclosed'"] {
        let result = fish(&format!("complete -c native_probe {flags}"));
        assert_eq!(
            result.status.code(),
            Some(1),
            "{flags}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let result = fish("complete -C");
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("no commandline"));
}

#[test]
fn escaped_queries_match_builtin_behavior() {
    assert_eq!(
        output(
            r#"
        complete -c native_probe -f -a "'two words'"
        complete --escape -C 'native_probe t'
        complete -C 'native_probe t'
    "#
        ),
        "two\\ words\ntwo words\n"
    );
}

#[test]
fn definition_colors_use_the_editor_palette_without_coloring_query_output() {
    let colored = output(
        "set -g fish_color_command red; complete -c native_probe -fa alpha; complete --color always native_probe",
    );
    assert!(colored.contains("\u{1b}["));
    assert!(colored.contains("31mcomplete"), "{colored:?}");
    let plain = output("complete -c native_probe -fa alpha; complete --color never native_probe");
    assert!(!plain.contains('\u{1b}'));
    assert_eq!(
        output("complete -c native_probe -fa alpha; complete --color always -C 'native_probe a'"),
        "alpha\n"
    );
}

#[test]
fn path_registrations_and_file_policy_follow_command_and_wrapper_precedence() {
    let directory = Directory::new();
    std::fs::write(directory.path().join("native_probe.exe"), b"").unwrap();
    std::fs::write(directory.path().join("sample.txt"), b"").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            r#"
            complete -p '*/native_probe.exe' -f -a path_choice
            complete -C './native_probe.exe path_'
            complete -C 'native_probe path_'
            complete -c 'native_f*' -f -a fuzzy_command
            complete -C 'native_foo fuzzy_'
            complete -c native_probe -F
            complete -C 'native_probe sam'
            complete -c native_outer -f -w native_probe
            complete -C 'native_outer sam'
            complete -c native_outer -F
            complete -C 'native_outer sam'
            complete -c native_probe -s I -r
            complete -C 'native_probe -Isam'
        "#,
        ])
        .current_dir(directory.path())
        .env("PATH", directory.path())
        .output()
        .unwrap();
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
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "path_choice\npath_choice\nfuzzy_command\nsample.txt\nsample.txt\n-Isample.txt\n"
    );
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

#[test]
fn command_inventory_distinguishes_empty_path_lists_and_preserves_path_elements() {
    let directory = Directory::new();
    std::fs::write(directory.path().join("native_inventory_probe.exe"), b"").unwrap();
    let tools = directory.path().join("tool;bin");
    std::fs::create_dir(&tools).unwrap();
    std::fs::write(tools.join("native_semicolon_probe.exe"), b"").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args([
            "-N",
            "-c",
            r#"
            set -g PATH
            echo absent
            complete -C native_inventory_
            set -g PATH ''
            echo current
            complete -C native_inventory_
            set -g PATH
            echo explicit
            complete -C './native_inventory_'
            set --unpath PATH "$PWD/tool;bin"
            echo semicolon
            complete -C native_semicolon_
            "#,
        ])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        concat!(
            "absent\ncurrent\nnative_inventory_probe\tCommand\n",
            "explicit\n./native_inventory_probe.exe\n",
            "semicolon\nnative_semicolon_probe\tCommand\n"
        )
    );
}

#[test]
fn variable_directory_queries_keep_expressions_and_use_current_home() {
    let directory = Directory::new();
    std::fs::write(directory.path().join("sample file.txt"), b"").unwrap();
    assert_eq!(
        directory.query(
            r#"
            set -l base $PWD
            complete -C 'echo $base/sam'
            complete --escape -C 'echo "$base/sam'
            complete -C 'echo $base[1]/sam'
            set -l HOME $PWD
            complete -C 'echo ~/sam'
        "#
        ),
        concat!(
            "$base/sample file.txt\n",
            "\"$base/sample file.txt\"\n",
            "$base[1]/sample file.txt\n",
            "~/sample file.txt\n"
        )
    );
}

#[test]
fn variable_directory_queries_refresh_lists_and_function_local_values() {
    let directory = Directory::new();
    for (folder, name) in [("first", "first.txt"), ("second", "final.txt")] {
        let path = directory.path().join(folder);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join(name), b"").unwrap();
    }
    assert_eq!(
        directory.query(
            r#"
            set base "$PWD/first"
            function native_path_query --argument-names base
                complete -C 'echo $base/fi'
            end
            native_path_query "$PWD/second"
            complete -C 'echo $base/fi'
            set base "$PWD/second"
            complete -C 'echo $base/fi'
            set roots "$PWD/first" "$PWD/second"
            complete -C 'echo $roots/fi'
            complete -C 'echo $roots[-1]/fi'
        "#
        ),
        concat!(
            "$base/final.txt\n$base/first.txt\n$base/final.txt\n",
            "$roots/final.txt\n$roots/first.txt\n$roots[-1]/final.txt\n"
        )
    );
}

#[test]
fn variable_directories_autoload_file_policy_without_running_typed_substitutions() {
    let directory = Directory::new();
    std::fs::write(directory.path().join("final.txt"), b"").unwrap();
    std::fs::create_dir(directory.path().join("completions")).unwrap();
    std::fs::write(
        directory.path().join("completions/native_files.fish"),
        b"complete -c native_files -f\n",
    )
    .unwrap();
    assert_eq!(
        directory.query(
            r#"
            set base $PWD
            set fish_complete_path "$PWD/completions"
            function native_files; end
            complete -C 'native_files $base/fi'
            complete -C 'native_files > $base/fi'
            complete -c native_files -F
            complete -C 'native_files $base/fi'
            complete -C 'echo $base/(set -g typed_ran yes)/fi'
            complete -C 'echo $base[(set -g typed_ran yes)]/fi'
            complete -C 'echo $base/*/fi'
            set -q typed_ran
            echo untouched:$status
        "#
        ),
        "$base/final.txt\n$base/final.txt\nuntouched:1\n"
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
    use std::io::Write as _;
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
