//! Query bundled generators with deterministic command output on both backends.

#[path = "support/fish_fixture.rs"]
mod fish_fixture;

use fish_fixture::FishFixture;

const CARGO: &str = r#"
function cargo
    if test "$RUSTUP_AUTO_INSTALL" != 0
        echo unexpected-auto-install >> unexpected-query
        return 91
    end
    set -l arguments (string join ' ' -- $argv)
    switch $arguments
        case '--color=never --list'
            printf 'Installed Commands:\n    build    Compile a local package\n    custom   A user command\n'
        case '--color=never run --bin'
            printf 'error: --bin takes one argument.\nAvailable binaries:\n    app_alpha\n    app_beta\n' >&2
            return 101
        case '--color=never run --example'
            printf 'error: --example takes one argument.\nAvailable examples:\n    example_alpha\n    example_beta\n' >&2
            return 101
        case '--color=never test --test'
            printf 'error: --test takes one argument.\nAvailable tests:\n    suite_alpha\n    suite_beta\n' >&2
            return 101
        case '*'
            printf 'unexpected-cargo-query <%s>\n' $argv >> unexpected-query
            return 91
    end
end
function rustc
    if test "$argv" != '--print target-list'
        echo unexpected-rustc-query >> unexpected-query
        return 91
    end
    printf '%s\n' x86_64-pc-windows-msvc x86_64-unknown-linux-gnu wasm32-unknown-unknown
end
"#;

fn candidates(output: &str) -> Vec<&str> {
    let mut result: Vec<_> = output
        .lines()
        .map(|line| line.split('\t').next().unwrap())
        .collect();
    result.sort_unstable();
    result
}

#[test]
fn cargo_generators_parse_stderr_and_keep_option_context() {
    for (line, expected) in [
        ("cargo cu", vec!["custom"]),
        ("cargo run --bin app_", vec!["app_alpha", "app_beta"]),
        (
            "cargo run --example example_",
            vec!["example_alpha", "example_beta"],
        ),
        (
            "cargo test --test suite_",
            vec!["suite_alpha", "suite_beta"],
        ),
        (
            "cargo build --target x86_64-",
            vec!["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"],
        ),
        ("cargo build --fea", vec!["--features"]),
    ] {
        let fixture = FishFixture::new();
        let output = fixture.run(&format!("{CARGO}\ncomplete -C '{line}'"));
        assert_eq!(candidates(&output), expected, "query: {line}");
        assert!(
            !fixture.0.join("unexpected-query").exists(),
            "{}",
            std::fs::read_to_string(fixture.0.join("unexpected-query")).unwrap_or_default()
        );
    }
}

#[test]
fn saved_aliases_autoload_and_complete_wrapped_generators_on_first_query() {
    let fixture = FishFixture::new();
    fixture.write("config/fish/functions/cargo.fish", CARGO);
    let output = fixture.run(
        r#"
            function mkdir
                test "$argv" = "-p $__fish_config_dir/functions"
                and test -d "$__fish_config_dir/functions"
            end
            alias --save parity_run 'cargo run' > saved-message
            functions -q parity_run
            "#,
    );
    assert!(output.is_empty(), "{output}");
    let saved = std::fs::read_to_string(fixture.0.join("saved-message")).unwrap();
    assert!(saved.starts_with("funcsave: wrote "), "{saved}");
    assert!(
        fixture
            .0
            .join("config/fish/functions/parity_run.fish")
            .is_file()
    );
    let output = fixture.run("complete -C 'parity_run --bin app_'");
    assert_eq!(candidates(&output), ["app_alpha", "app_beta"]);
    assert!(!fixture.0.join("unexpected-query").exists());
}

#[test]
fn nested_subcommand_helpers_forward_options_and_quoted_command_context() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            function parity_tool; end
            function parity_wrap; end
            complete -c parity_tool -f -n '__fish_seen_subcommand_from run' -l flavor -x -a 'alpha beta'
            complete -c parity_wrap -f -a '(__fish_complete_subcommand -- --config)'
            complete -C 'parity_wrap --config "two words" parity_tool run --fl'
            complete -C 'parity_wrap --config "two words" parity_tool run --flavor al'
            "#
        ),
        "--flavor\nalpha\n"
    );
}

#[test]
fn completion_conditions_observe_current_tokens_and_generators_run_lazily() {
    let fixture = FishFixture::new();
    assert_eq!(
        fixture.run(
            r#"
            function parity_tool; end
            function parity_generate
                printf called >> generator-calls
                printf 'alpha\tFirst value\nbeta\tSecond value\n'
            end
            complete -c parity_tool -f -n '__fish_seen_subcommand_from run' -a '(parity_generate)'
            test -e generator-calls; echo registered:$status
            complete -C 'parity_tool stop al'
            test -e generator-calls; echo inactive:$status
            complete -C 'parity_tool run al'
            test -e generator-calls; echo active:$status
            "#
        ),
        "registered:1\ninactive:1\nalpha\tFirst value\nactive:0\n"
    );
}
