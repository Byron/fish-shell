//! Packaged command fallbacks retain external lookup and process semantics.
#![cfg(all(windows, feature = "native-utilities"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fish-native-utilities-{}-{} space 文",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .current_dir(&self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("APPDATA", &self.0)
            .env("LOCALAPPDATA", &self.0)
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("PATH", "")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, source: &str) -> Output {
        let mut child = self
            .command()
            .args(["-N", "-c", source])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "native utility fixture timed out: stdout {}, stderr {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn executable(&self, directory: &str, name: &str) -> PathBuf {
        let directory = self.0.join(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        if std::fs::hard_link(env!("CARGO_BIN_EXE_fish"), &path).is_err() {
            std::fs::copy(env!("CARGO_BIN_EXE_fish"), &path).unwrap();
        }
        directory
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(root) = self.0.canonicalize() else {
            return;
        };
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-utilities-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "status {:?}, stdout {}, stderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "\\'")
    )
}

#[test]
fn packaged_filters_stream_through_native_pipelines() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        printf 'c\nb\nb\na\n' | sort -u | head -n 2
        printf 'first\nsecond\nthird\n' | sed '1,2d'
        printf 'red\nblue\nred\n' | sort | uniq -u
        seq 2 2 6 | tail -n 2
    "#
        )),
        "a\nb\nthird\nblue\n4\n6\n"
    );
}

#[test]
fn queries_distinguish_utilities_from_builtins_and_physical_paths() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        command -q seq; echo query:$status
        command -sq seq; echo quiet-search:$status
        command -s seq; echo path:$status
        builtin -q seq; echo builtin:$status
        type -t seq
        type seq
        seq 1 2
        echo still-running
    "#
        )),
        "query:0\nquiet-search:0\npath:127\nbuiltin:1\nutility\nseq is a bundled utility\n1\n2\nstill-running\n"
    );
}

#[test]
fn path_programs_win_and_private_helpers_can_select_the_packaged_utility() {
    let fixture = Fixture::new();
    let directory = fixture.executable("external bin", "sort.exe");
    let source = format!(
        r#"
        set -gx PATH {}
        sort -N -c 'echo external-sort'
        printf 'b\na\n' | __fish_native_utility sort
        type -t sort
    "#,
        quote(&directory)
    );
    assert_eq!(success(fixture.run(&source)), "external-sort\na\nb\nfile\n");
}

#[test]
fn rustup_helpers_use_unix_utilities_with_windows_name_collisions() {
    let fixture = Fixture::new();
    // A real same-name executable would fail on Unix options. The shared
    // helpers must explicitly select the packaged implementation instead.
    let directory = fixture.executable("windows bin", "sort.exe");
    fixture.executable("windows bin", "find.exe");
    let docs = fixture.0.join("docs.v1");
    for module in ["std", "std/collections", "core", "alloc"] {
        std::fs::create_dir_all(docs.join(module)).unwrap();
        std::fs::write(docs.join(module).join("index.html"), b"").unwrap();
    }
    let share = Path::new(env!("CARGO_MANIFEST_DIR")).join("share");
    let source = format!(
        r#"
        set -gx PATH {}
        source {}
        source {}
        source {}
        function rustup
            if test "$argv[1]" = doc
                echo '{}'
            else
                printf 'rustfmt-x86_64-unknown-linux-gnu\nrustfmt-x86_64-unknown-linux-gnu\nclippy-x86_64-unknown-linux-gnu\n'
            end
        end
        __rustup_components
        __fish_rustup_docs_modules | __fish_sort
    "#,
        quote(&directory),
        quote(&share.join("functions/__fish_sort.fish")),
        quote(&share.join("functions/__fish_find.fish")),
        quote(&share.join("completions/rustup.fish")),
        docs.join("index.html")
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\'', "\\'"),
    );
    assert_eq!(
        success(fixture.run(&source)),
        "clippy\nrustfmt\nalloc\tModule\ncore\tModule\nstd\tModule\nstd::collections\tModule\n"
    );
}

#[test]
fn aliases_and_user_autoload_functions_keep_their_precedence() {
    let fixture = Fixture::new();
    let directory = fixture.executable("external bin", "cat.exe");
    let functions = fixture.0.join("functions");
    std::fs::create_dir(&functions).unwrap();
    std::fs::write(
        functions.join("seq.fish"),
        "function seq; echo user-seq; end",
    )
    .unwrap();
    let source = format!(
        r#"
        set -g fish_function_path {}
        seq
        command seq 1 2
        alias cat 'cat -N -c "echo external-alias"'
        set -gx PATH {}
        cat
    "#,
        quote(&functions),
        quote(&directory)
    );
    assert_eq!(
        success(fixture.run(&source)),
        "user-seq\n1\n2\nexternal-alias\n"
    );
}

#[test]
fn local_unexported_and_prefix_paths_select_real_programs() {
    let fixture = Fixture::new();
    let directory = fixture.executable("local bin", "cat.exe");
    let source = format!(
        r#"
        begin
            set -lu PATH {}
            cat -N -c 'echo local-executable'
            __fish_native_utility env | string match -r '^PATH='
            echo exported:$status
        end
        PATH={} cat -N -c 'echo prefix-executable'
        type -t cat
    "#,
        quote(&directory),
        quote(&directory)
    );
    assert_eq!(
        success(fixture.run(&source)),
        "local-executable\nexported:1\nprefix-executable\nutility\n"
    );
}

#[test]
fn empty_path_list_does_not_search_the_current_directory() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("cat.exe"), b"not an executable").unwrap();
    let output = fixture.run(
        r#"
        set -g PATH
        printf 'packaged\n' | command cat
        command -s cat; echo absent:$status
        set -g PATH ''
        command cat; echo empty-component:$status
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "packaged\nabsent:127\nempty-component:126\n"
    );
    assert!(!output.stderr.is_empty());
}

#[test]
fn semicolons_inside_path_elements_keep_their_boundaries() {
    let fixture = Fixture::new();
    let directory = fixture.executable("semi;colon", "cat.exe");
    let source = format!(
        r#"
        set -gu --unpath PATH {}
        command cat -N -c 'echo external'
        command -sq cat; echo query:$status
    "#,
        quote(&directory)
    );
    assert_eq!(success(fixture.run(&source)), "external\nquery:0\n");
}

#[test]
fn type_does_not_search_invalid_paths_after_finding_a_function_or_builtin() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        function cat; echo user-cat; end
        set -g --unpath PATH '"'
        type -t echo cat
        echo query:$status
    "#,
        )),
        "builtin\nfunction\nquery:0\n"
    );
}

#[test]
fn missing_explicit_paths_and_invalid_real_executables_never_retry() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("cat.exe"), b"not an executable").unwrap();
    let output = fixture.run("./seq 1 2; echo explicit:$status; command cat; echo invalid:$status");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "explicit:127\ninvalid:126\n"
    );
    assert!(!output.stderr.is_empty());
}

#[test]
fn private_dispatch_does_not_expand_wildcards_a_second_time() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("one.fish"), b"").unwrap();
    std::fs::write(fixture.0.join("two.fish"), b"").unwrap();
    let output = fixture
        .command()
        .args(["basename", "--native-utility-v1", "*.fish"])
        .output()
        .unwrap();
    assert_eq!(success(output), "*.fish\n");
}

#[test]
fn cat_preserves_binary_input_and_redirections() {
    let fixture = Fixture::new();
    let bytes = b"before\0\xff\r\nafter\n";
    std::fs::write(fixture.0.join("input.bin"), bytes).unwrap();
    assert_eq!(success(fixture.run("cat <input.bin >output.bin")), "");
    assert_eq!(std::fs::read(fixture.0.join("output.bin")).unwrap(), bytes);
}

#[test]
fn awk_supports_bundled_completion_formatting_and_special_assignments() {
    let fixture = Fixture::new();
    assert_eq!(
        success(fixture.run(
            r#"
        printf 'a\tAlpha\nb\tBeta\n' | awk -F '\t' '{ printf "/%s\t%s\n", $1, $2 }'
        printf 'command - description\n' | awk -v FS=' +- +' '{print $1 "|" $2}'
        printf 'a\na\nb\n' | awk '{count[$1]++} END {print count["a"], count["b"]}'
    "#
        )),
        "/a\tAlpha\n/b\tBeta\ncommand|description\n2 1\n"
    );
}

#[test]
fn awk_opens_input_files_only_when_the_interpreter_reaches_them() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("first.txt"), b"first file\n").unwrap();
    let output = fixture.run(
        r#"
        awk 'BEGIN {print "early"; exit}' missing.txt
        echo begin:$status
        awk '{print $0}' first.txt missing.txt
        echo late:$status
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "early\nbegin:0\nfirst file\nlate:2\n"
    );
    assert!(!output.stderr.is_empty());
}

#[test]
fn find_and_grep_work_without_path_tools() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("first.txt"), b"alpha\nbeta\n").unwrap();
    std::fs::write(fixture.0.join("second.bin"), b"unused").unwrap();
    assert_eq!(
        success(fixture.run(
            r#"
        find . -maxdepth 1 -name '*.txt' -printf '%f\n'
        grep -E '^(alpha|gamma)$' first.txt
    "#
        )),
        "first.txt\nalpha\n"
    );
}
