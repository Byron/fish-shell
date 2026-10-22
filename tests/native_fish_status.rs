#![cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fish-status-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, source: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fish"))
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .output()
            .unwrap()
    }
    fn output(&self, source: &str) -> String {
        let result = self.run(source);
        assert!(result.status.success(), "{result:?}");
        assert!(result.stderr.is_empty(), "{result:?}");
        String::from_utf8(result.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn feature_queries_report_native_semantics_and_unknown_names_silently() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.output(
            r#"
        for feature in regex-easyesc qmark-noglob remove-percent-self query-term absent_feature
            status test-feature $feature
            echo $feature:$status
        end
        status features | string match -rq '^regex-easyesc +on '
        echo table:$status
    "#
        ),
        "regex-easyesc:0\nqmark-noglob:0\nremove-percent-self:0\nquery-term:1\nabsent_feature:2\ntable:0\n"
    );
}

#[test]
fn build_and_embedded_metadata_support_the_actual_bundled_helpers() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join("custom")).unwrap();
    std::fs::write(fixture.0.join("custom/__fish_pwd.fish"), "# user helper\n").unwrap();
    let result = fixture.output(
        r#"
        __fish_pwd
        __fish_posix_shell
        __fish_config_files --user-dir "$PWD/custom" functions .fish __fish_pwd
        status get-file functions/__fish_pwd.fish | string match -q '*function __fish_pwd*'
        echo embedded:$status
        status list-files functions/__fish_pwd.fish absent_status_prefix
        echo listed:$status
        status get-file absent_status_file
        echo missing:$status
    "#,
    );
    let lines: Vec<_> = result.lines().collect();
    assert_eq!(PathBuf::from(lines[0]), fixture.0);
    assert_eq!(lines[1], "/bin/sh");
    assert_eq!(
        PathBuf::from(lines[2]),
        fixture.0.join("custom/__fish_pwd.fish")
    );
    assert_eq!(
        &lines[3..],
        [
            "functions/__fish_pwd.fish",
            "embedded:0",
            "functions/__fish_pwd.fish",
            "listed:0",
            "missing:1"
        ]
    );
    let build = fixture.output("status build-info");
    assert!(build.contains("Build system: Cargo\n"), "{build}");
    assert!(
        build.contains("windows-msvc\n") || build.contains("windows-gnu\n"),
        "{build}"
    );
    assert!(build.contains("Profile: "), "{build}");
}

#[test]
fn function_levels_and_source_paths_are_available_without_fabricated_locations() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.output(
            r#"
        function native_status_inner
            status function
            status --level 2 current-function
            status -L0 function
        end
        function native_status_outer
            native_status_inner
            true
        end
        native_status_outer
        status function
        status current-command
    "#
        ),
        "native_status_inner\nnative_status_outer\nNot a function\nNot a function\nfish\n"
    );
    std::fs::write(fixture.0.join("origin.fish"), "function native_status_origin; status current-filename; status current-basename; status current-dirname; end; native_status_origin\n").unwrap();
    assert_eq!(
        fixture.output("source origin.fish"),
        "origin.fish\norigin.fish\n.\n"
    );
    let result = fixture.output("source \"$PWD/origin.fish\"");
    let lines: Vec<_> = result.lines().collect();
    assert_eq!(PathBuf::from(lines[0]), fixture.0.join("origin.fish"));
    assert_eq!(lines[1], "origin.fish");
    assert_eq!(PathBuf::from(lines[2]), fixture.0);
}

#[test]
fn invalid_and_unavailable_queries_return_statuses_and_leave_the_script_running() {
    let fixture = Fixture::new();
    let result = fixture.run(
        r#"
        status absent_query; echo unknown:$status
        status line-number; echo unavailable:$status
        status --absent; echo option:$status
        status is-interactive extra; echo arguments:$status
        status -ic; echo conflicting:$status
        status -i absent_query; echo unknown_with_flag:$status
        status terminal extra; echo unavailable_arguments:$status
        status is-native-windows; echo native:$status
    "#,
    );
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "unknown:2\nunavailable:1\noption:2\narguments:2\nconflicting:1\nunknown_with_flag:2\nunavailable_arguments:2\nnative:0\n"
    );
    let errors = String::from_utf8(result.stderr).unwrap();
    assert!(errors.contains("unknown subcommand"), "{errors}");
    assert!(errors.contains("not available"), "{errors}");
    assert!(errors.contains("expected 0 arguments"), "{errors}");
}

#[test]
fn only_real_command_substitutions_set_the_substitution_predicate() {
    let fixture = Fixture::new();
    assert_eq!(fixture.output(r#"
        status is-command-substitution; echo direct:$status
        set result (status is-command-substitution; echo substitution:$status)
        echo $result
        begin; status is-command-substitution; echo pipeline:$status; end | string collect; echo
        complete -c native_status_probe -fa '(status is-command-substitution; echo callback:$status)'
        complete -C 'native_status_probe callback:'
        complete -c native_status_plain -n 'not status is-command-substitution' -fa plain
        complete -C 'native_status_plain pl'
    "#), "direct:1\nsubstitution:0\npipeline:1\ncallback:0\nplain\n");
}
