//! Native path helpers retain Windows roots and compose in shared Fish scripts.
#![cfg(windows)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-path-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn run(&self, source: &str) -> Output {
        self.run_in("", source, false)
    }

    fn run_in(&self, directory: &str, source: &str, config: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .current_dir(self.0.join(directory))
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        if !config {
            command.arg("-N");
        }
        command.args(["-c", source]).output().unwrap()
    }

    fn path(&self, tail: &str) -> String {
        self.0.join(tail).to_string_lossy().replace('/', "\\")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-path-"))
        {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[test]
fn nested_helpers_find_the_config_home_before_aliases_are_defined() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("config/fish")).unwrap();
    fs::write(
        fixture.0.join("config/fish/config.fish"),
        r#"
        set -g configured_home (path dirname (path dirname (path resolve (status filename))))
        function configured_alias --description 'Loaded after resolving the config home'
            echo $configured_home
        end
    "#,
    )
    .unwrap();
    assert_eq!(
        stdout(fixture.run_in("", "configured_alias", true)),
        format!("{}\n", fixture.path("config"))
    );
}

#[test]
fn transforms_keep_windows_roots_and_use_the_shared_status_rules() {
    let fixture = Fixture::new();
    let output = stdout(fixture.run(r#"
        path normalize 'C:/one/../two' 'C:one/../two' '/one/../../two' '//server/share/one/../../two'
        path dirname 'C:/one/file' 'C:/file' 'C:file' '//server/share/file'
        path basename 'C:/one/file.txt' 'C:file.txt'
        path basename -E archive.tar.gz .hidden . ..
        path normalize already
        echo unchanged:$status
        path dirname folder/file
        echo changed:$status
        path normalize -- -option/file
        path dirname -q folder/file
        echo quiet:$status
        path dirname
        echo empty:$status
    "#));
    assert_eq!(
        output,
        concat!(
            "C:\\two\nC:two\n\\two\n\\\\server\\share\\two\n",
            "C:/one\nC:/\nC:\n//server/share\n",
            "file.txt\nfile.txt\narchive.tar\n.hidden\n.\n..\n",
            "already\nunchanged:1\nfolder\nchanged:0\n.\\-option\\file\nquiet:0\nempty:1\n"
        )
    );
}

#[test]
fn physical_and_lexical_resolution_handle_missing_paths_differently() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("existing")).unwrap();
    fs::write(fixture.0.join("file"), b"contents").unwrap();
    let output = fixture.run(
        r#"
        builtin realpath existing/new
        builtin realpath missing/new
        echo missing-parent:$status
        builtin realpath file/child
        echo file-parent:$status
        builtin realpath -s missing/one/../two
        path resolve missing/one/../two
        path resolve file/../existing
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}\nmissing-parent:1\nfile-parent:1\n{}\n{}\n{}\n",
            fixture.path("existing/new"),
            fixture.path("missing/two"),
            fixture.path("missing/two"),
            fixture.path("existing")
        )
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).lines().count(), 2);
}

#[test]
fn drive_relative_and_current_drive_rooted_paths_use_windows_rules() {
    let fixture = Fixture::new();
    let full = fixture.path("");
    let drive = &full[..2];
    let source = format!(
        "builtin realpath -s {}; builtin realpath -s {}; builtin realpath -s {}; builtin realpath -s {}",
        quote(&format!("{drive}child")),
        quote("\\child"),
        quote("/child"),
        quote("//server/share/../child")
    );
    assert_eq!(
        stdout(fixture.run(&source)),
        format!(
            "{}\n{drive}\\child\n{drive}\\child\n\\\\server\\share\\child\n",
            fixture.path("child")
        )
    );
}

#[test]
fn file_symlinks_and_the_working_directory_have_distinct_resolution_rules() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("elsewhere/target")).unwrap();
    match std::os::windows::fs::symlink_dir(r"elsewhere\target", fixture.0.join("link")) {
        Ok(()) => (),
        Err(error) if error.raw_os_error() == Some(1314) => return,
        Err(error) => panic!("symlink fixture failed: {error}"),
    }
    assert!(
        fixture.0.join("link").is_dir(),
        "directory symlink must resolve"
    );
    assert_eq!(
        stdout(fixture.run(
            r#"
        builtin realpath link
        builtin realpath -s link
        path resolve link/missing
        path resolve link/..
    "#
        )),
        format!(
            "{}\n{}\n{}\n{}\n",
            fixture.path("elsewhere/target"),
            fixture.path("link"),
            fixture.path("elsewhere/target/missing"),
            fixture.path("").trim_end_matches('\\')
        )
    );
    assert_eq!(
        stdout(fixture.run_in("link", "builtin realpath -s child", false)),
        format!("{}\n", fixture.path("elsewhere/target/child"))
    );
}

#[test]
fn redirected_input_accepts_newlines_and_nuls_and_rejects_mixed_operands() {
    let fixture = Fixture::new();
    let output = stdout(fixture.run(
        r#"
        printf '%s\n' folder/one folder/two | path dirname
        printf '%s\0' folder/one folder/two | path dirname -zZ
        printf '%s\0' folder/three folder/four | path basename
        printf '%s\n' inherited/input > paths
        begin; path dirname explicit/input; end < paths
        path dirname argument < paths 2>/dev/null
        echo mixed:$status
    "#,
    ));
    assert_eq!(
        output,
        "folder\nfolder\nfolder\0folder\0three\nfour\nexplicit\nmixed:2\n"
    );
}

#[test]
fn invalid_options_and_namespaces_report_failure_without_stopping_the_caller() {
    let fixture = Fixture::new();
    let mut source = String::from(
        "path dirname --unknown; echo option:$status; builtin realpath; echo arity:$status; builtin realpath ''; echo empty:$status; builtin realpath -s ''; echo empty-s:$status; ",
    );
    for path in [
        r"\\?\C:\Windows",
        r"\\.\NUL",
        r"\??\C:\Windows",
        r"\\server",
    ] {
        source.push_str(&format!(
            "path normalize -- {}; echo rejected:$status; ",
            quote(path)
        ));
    }
    source.push_str("path resolve NUL; echo device:$status; echo continued");
    let output = fixture.run(&source);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "option:2\narity:2\nempty:1\nempty-s:1\nrejected:1\nrejected:1\nrejected:1\nrejected:1\ndevice:1\ncontinued\n"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).lines().count(), 9);
}

#[test]
fn physical_resolution_rejects_targets_that_require_verbatim_paths() {
    let fixture = Fixture::new();
    let target = fixture.0.canonicalize().unwrap().join("verbatim-only.");
    fs::write(&target, b"contents").unwrap();
    match std::os::windows::fs::symlink_file(&target, fixture.0.join("ordinary-link")) {
        Ok(()) => (),
        Err(error) if error.raw_os_error() == Some(1314) => return,
        Err(error) => panic!("symlink fixture failed: {error}"),
    }
    let output = fixture.run("builtin realpath ordinary-link; echo physical:$status; path resolve ordinary-link; echo resolve:$status; builtin realpath -s ordinary-link");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("physical:1\nresolve:1\n{}\n", fixture.path("ordinary-link"))
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).lines().count(), 2);
}
