//! Check native startup and saved aliases through the public executable.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "fish-native-startup-{}-{} space 文",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    let fixture = Self(root);
                    for directory in [
                        "native config/fish/conf.d",
                        "native config/fish/functions",
                        "home",
                        "roaming",
                        "local",
                        "xdg",
                        "data",
                    ] {
                        std::fs::create_dir_all(fixture.path(directory)).unwrap();
                    }
                    return fixture;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create startup fixture: {error}"),
            }
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn write(&self, relative: &str, source: &str) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    fn config(&self, relative: &str, source: &str) {
        self.write(&format!("native config/fish/{relative}"), source);
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        // Keep the Windows runtime location, without inheriting shell startup,
        // function-path, or Git configuration from the developer's environment.
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .current_dir(&self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.path("native config"))
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("APPDATA", self.path("roaming"))
            .env("LOCALAPPDATA", self.path("local"))
            .env("XDG_CONFIG_HOME", self.path("xdg"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, source: &str) -> Output {
        self.command().args(["-c", source]).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove only the private fixture directory created directly in temp.
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        let Ok(root) = self.0.canonicalize() else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-startup-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        stdout(output),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn user_snippets_run_in_name_order_before_config() {
    let fixture = Fixture::new();
    fixture.config("conf.d/20-second.fish", "echo second:$startup_value");
    fixture.config(
        "conf.d/10-first.fish",
        "set -g startup_value ready; echo first",
    );
    fixture.config("conf.d/ignored.txt", "echo wrong-extension");
    fixture.config("config.fish", "echo config:$startup_value");
    fixture.write("xdg/fish/config.fish", "echo wrong-config-home");

    let output = fixture.run("echo command:$startup_value");
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "first\nsecond:ready\nconfig:ready\ncommand:ready\n"
    );
}

#[test]
fn described_wrappers_from_snippets_and_saved_aliases_forward_arguments() {
    let fixture = Fixture::new();
    fixture.config(
        "conf.d/aliases.fish",
        r#"
        function git
            printf '<%s>\n' $argv
        end
        function conf_alias --wraps='git checkout' --description 'alias conf_alias=git checkout'
            git checkout $argv
        end
        "#,
    );
    fixture.config(
        "functions/saved_alias.fish",
        r#"
        function saved_alias --wraps 'git status' --description 'alias saved_alias=git status'
            git status $argv
        end
        "#,
    );
    fixture.config("config.fish", "conf_alias 'branch 文'");

    let output = fixture.run("saved_alias --short 'two words'");
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "<checkout>\n<branch 文>\n<status>\n<--short>\n<two words>\n"
    );
}

#[test]
fn invocation_and_function_queries_load_only_requested_files() {
    let fixture = Fixture::new();
    for (name, value) in [
        ("invoke_alias", "invoked"),
        ("query_alias", "queried"),
        ("type_alias", "typed"),
    ] {
        fixture.config(
            &format!("functions/{name}.fish"),
            &format!(
                "echo loading-{name}; function {name} --description '{value} lazily'; echo {value}; end"
            ),
        );
    }
    fixture.config(
        "functions/unused.fish",
        "echo should-not-run > unused-marker; function unused; end",
    );

    let output = fixture.run(
        "echo ready; invoke_alias; invoke_alias; functions -q query_alias; echo query:$status; query_alias; type -t type_alias; type_alias",
    );
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "ready\nloading-invoke_alias\ninvoked\ninvoked\nloading-query_alias\nquery:0\nqueried\nloading-type_alias\nfunction\ntyped\n"
    );
    assert!(!fixture.path("unused-marker").exists());
}

#[test]
fn explicit_config_definitions_take_precedence_over_function_files() {
    let fixture = Fixture::new();
    fixture.config(
        "functions/preferred.fish",
        "echo wrong > shadowed-marker; function preferred; echo file; end",
    );
    fixture.config(
        "config.fish",
        "function preferred --description 'explicit configuration'; echo configured; end",
    );

    let output = fixture.run("functions -q preferred; echo query:$status; preferred");
    assert_success(&output);
    assert_eq!(stdout(&output), "query:0\nconfigured\n");
    assert!(!fixture.path("shadowed-marker").exists());
}

#[test]
fn erasing_an_autoloaded_function_prevents_implicit_reloading() {
    let fixture = Fixture::new();
    fixture.config(
        "functions/erasable.fish",
        "echo loading; function erasable; echo called; end",
    );

    let output = fixture.run(
        "erasable; functions -e erasable; functions -q erasable; echo query:$status; type -q erasable; echo type:$status",
    );
    assert_success(&output);
    assert_eq!(stdout(&output), "loading\ncalled\nquery:1\ntype:1\n");
}

#[test]
fn changing_function_paths_refreshes_misses_and_respects_path_order() {
    let fixture = Fixture::new();
    for (directory, value) in [
        ("first functions", "first"),
        ("second functions 文", "second"),
    ] {
        fixture.write(
            &format!("{directory}/path_alias.fish"),
            &format!("function path_alias; echo {value}; end"),
        );
    }
    fixture.write(
        "second functions 文/later_alias.fish",
        "function later_alias; echo discovered; end",
    );

    let output = fixture.run(
        "set -g fish_function_path 'first functions'; functions -q later_alias; echo missing:$status; set -g fish_function_path 'second functions 文' 'first functions'; functions -q later_alias; echo found:$status; later_alias; path_alias",
    );
    assert_success(&output);
    assert_eq!(stdout(&output), "missing:1\nfound:0\ndiscovered\nsecond\n");
}

#[test]
fn no_config_skips_snippets_and_config_but_keeps_function_autoloading() {
    let fixture = Fixture::new();
    fixture.config("conf.d/startup.fish", "echo snippet > snippet-marker");
    fixture.config("config.fish", "echo config > config-marker");
    fixture.config(
        "functions/without_startup.fish",
        "function without_startup --description 'autoload with -N'; echo autoloaded; end",
    );

    let output = fixture
        .command()
        .args(["-N", "-c", "without_startup"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output), "autoloaded\n");
    assert!(!fixture.path("snippet-marker").exists());
    assert!(!fixture.path("config-marker").exists());
}

#[test]
fn invalid_snippets_are_diagnosed_without_blocking_later_startup() {
    let fixture = Fixture::new();
    fixture.config("conf.d/10-broken.fish", "if true\n");
    fixture.config("conf.d/20-good.fish", "echo later-snippet");
    fixture.config("config.fish", "echo config");

    let output = fixture.run("echo command");
    assert!(output.status.success());
    assert_eq!(stdout(&output), "later-snippet\nconfig\ncommand\n");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("10-broken.fish"), "{diagnostic}");
}

#[test]
fn syntax_checks_do_not_run_startup_or_autoload_function_bodies() {
    let fixture = Fixture::new();
    fixture.config("conf.d/startup.fish", "echo snippet > snippet-marker");
    fixture.config("config.fish", "echo config > config-marker");
    fixture.config(
        "functions/syntax_alias.fish",
        "echo loaded > autoload-marker; function syntax_alias; echo body > body-marker; end",
    );

    let output = fixture
        .command()
        .args(["-n", "-c", "echo command > command-marker; syntax_alias"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(output.stdout.is_empty());
    for marker in [
        "snippet-marker",
        "config-marker",
        "autoload-marker",
        "body-marker",
        "command-marker",
    ] {
        assert!(!fixture.path(marker).exists(), "unexpected {marker}");
    }
}

#[test]
fn completion_inherits_saved_wrapper_targets_without_running_bodies() {
    let fixture = Fixture::new();
    fixture.config(
        "functions/saved_wrapper.fish",
        "function saved_wrapper --wraps 'target --mode' --description 'saved alias'; echo wrong > wrapper-marker; end",
    );
    let output = fixture.run(
        "complete -c target -l mode -x -a 'fast slow'; functions -q saved_wrapper; complete -C 'saved_wrapper f'; function multi --wraps saved_wrapper --wraps 'other'; end; complete -c other -f -a farther; complete -C 'multi fa'",
    );
    assert_success(&output);
    assert_eq!(stdout(&output), "fast\nfarther\nfast\n");
    assert!(!fixture.path("wrapper-marker").exists());
}

#[test]
fn changing_function_paths_removes_loaded_definitions_and_finds_replacements() {
    let fixture = Fixture::new();
    fixture.write(
        "first functions/swappable.fish",
        "function swappable; echo first; end",
    );
    fixture.write("empty functions/.keep", "");
    fixture.write(
        "replacement functions 文/swappable.fish",
        "function swappable; echo replacement; end",
    );

    let output = fixture.run(
        "set -g fish_function_path 'first functions'; swappable; set -g fish_function_path 'empty functions'; functions -q swappable; echo removed:$status; set -g fish_function_path 'replacement functions 文'; swappable",
    );
    assert_success(&output);
    assert_eq!(stdout(&output), "first\nremoved:1\nreplacement\n");
}

#[test]
fn startup_sources_isolate_locals_and_arguments_but_share_globals() {
    let fixture = Fixture::new();
    fixture.config(
        "conf.d/10-first.fish",
        "set -l first_local private; set -l argv first arguments; set -g shared first",
    );
    fixture.config(
        "conf.d/20-second.fish",
        "set -q first_local; echo second-local:$status; echo second-args:(count $argv); echo second-global:$shared; set -l second_local private; set -g shared second",
    );
    fixture.config(
        "config.fish",
        "set -q second_local; echo config-local:$status; echo config-args:(count $argv); echo config-global:$shared; set -l config_local private; set -l argv config arguments; set -g shared configured",
    );

    let output = fixture
        .command()
        .args([
            "-c",
            "set -q config_local; echo command-local:$status; printf 'command-arg:<%s>\\n' $argv; echo command-global:$shared",
            "two words",
            "文",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "second-local:1\nsecond-args:0\nsecond-global:first\nconfig-local:1\nconfig-args:0\nconfig-global:second\ncommand-local:1\ncommand-arg:<two words>\ncommand-arg:<文>\ncommand-global:configured\n"
    );
}

#[test]
fn pipeline_workers_preserve_the_autoloaded_file_snapshot() {
    let fixture = Fixture::new();
    fixture.config("config.fish", "set -g load_count 0");
    fixture.config(
        "functions/once.fish",
        "set -g load_count (math \"$load_count + 1\"); function once; echo loaded:$load_count; end",
    );

    let output = fixture.run("once; once | string upper; once; echo count:$load_count");
    assert_success(&output);
    assert_eq!(stdout(&output), "loaded:1\nLOADED:1\nloaded:1\ncount:1\n");
}

#[test]
fn exit_in_an_autoloaded_file_preserves_status_and_stops_the_command() {
    let fixture = Fixture::new();
    fixture.config(
        "functions/exiting.fish",
        "exit 42; function exiting; echo unreachable; end",
    );

    let output = fixture.run("exiting; echo wrong > trailing-marker");
    assert_eq!(output.status.code(), Some(42));
    assert!(output.stdout.is_empty());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.path("trailing-marker").exists());
}
