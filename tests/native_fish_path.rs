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

#[test]
fn extension_queries_share_dotfile_and_unicode_rules() {
    let fixture = Fixture::new();
    assert_eq!(
        stdout(fixture.run(
            r#"
        path extension .profile .cache/name file.tar.gz file. . .. 'C:\folder\文.txt'
        path extension .profile; echo absent:$status
        path change-extension log .profile file.tar.gz 'C:\folder\文.txt'
        path change-extension '' .profile file.tar.gz
        path extension -q no-extension file.txt; echo any:$status
    "#
        )),
        "\n\n.gz\n.\n\n\n.txt\n\nabsent:1\n.profile.log\nfile.tar.log\nC:\\folder\\文.log\n.profile\nfile.tar\nany:0\n"
    );
}

#[test]
fn path_filters_combine_types_permissions_inversion_and_all_queries() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"data").unwrap();
    fs::write(fixture.0.join("-dash"), b"data").unwrap();
    fs::create_dir(fixture.0.join("directory")).unwrap();
    assert_eq!(
        stdout(fixture.run(
            r#"
        path filter -rf file directory missing
        path filter -df file directory missing
        path filter -v file missing
        path filter --type=file --perm=read -- -dash
        path is --all -f file directory; echo all-files:$status
        path is --all file directory; echo all-present:$status
        path is file missing; echo any-present:$status
        path is --all; echo empty:$status
        path is --all file ''; echo empty-path:$status
    "#
        )),
        "file\nfile\ndirectory\nmissing\n./-dash\nall-files:1\nall-present:0\nany-present:0\nempty:1\nempty-path:1\n"
    );
}

#[test]
fn path_permissions_use_native_access_without_mutating_files() {
    let fixture = Fixture::new();
    let path = fixture.0.join("readonly");
    fs::write(&path, b"untouched").unwrap();
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let result = fixture
        .run("path is -r readonly; echo read:$status; path is -w readonly; echo write:$status");
    fs::set_permissions(&path, original).unwrap();
    assert_eq!(stdout(result), "read:0\nwrite:1\n");
    assert_eq!(fs::read(path).unwrap(), b"untouched");
}

#[test]
fn mtime_supports_absolute_and_relative_seconds_and_missing_paths() {
    let fixture = Fixture::new();
    let file = fs::File::create(fixture.0.join("dated")).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567_890))
        .unwrap();
    drop(file);
    assert_eq!(stdout(fixture.run("path mtime missing dated; path mtime missing; echo missing:$status; path mtime -q dated; echo present:$status")), "1234567890\nmissing:1\npresent:0\n");
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let relative: u64 = stdout(fixture.run("path mtime --relative dated"))
        .trim()
        .parse()
        .unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((before - 1_234_567_890..=after - 1_234_567_890).contains(&relative));
}

#[test]
fn path_sort_uses_shared_natural_order_and_stable_unique_keys() {
    let fixture = Fixture::new();
    assert_eq!(
        stdout(fixture.run(
            r#"
        path sort item10 item2 item1 item2
        path sort -ru item10 item2 item1 item2
        path sort --key=basename -u z/name2 a/name10 b/name2
        path sort --key=dirname -u b/one a/two b/three
        path sort; echo empty:$status
    "#
        )),
        "item1\nitem2\nitem2\nitem10\nitem10\nitem2\nitem1\nz/name2\na/name10\na/two\nb/one\nempty:0\n"
    );
}

#[test]
fn path_query_streams_preserve_null_records_and_direct_input_rules() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"data").unwrap();
    assert_eq!(
        stdout(fixture.run(
            r#"
        printf '%s\0' folder/item10 folder/item2 | path sort -zZ --key=basename
        printf '%s\0' file missing | path filter -z
        printf '%s\n' one.txt two.tar | path change-extension bin
        printf '%s\n' file > input
        begin; path is missing; echo inherited:$status; end < input
        path filter file < input 2>/dev/null; echo mixed:$status
    "#
        )),
        "folder/item2\0folder/item10\0file\none.bin\ntwo.bin\ninherited:1\nmixed:2\n"
    );
}

#[test]
fn path_query_option_errors_are_explicit_and_leave_the_caller_running() {
    let fixture = Fixture::new();
    let result = fixture.run(
        r#"
        path filter --type=unknown; echo type:$status
        path filter --perm=unknown; echo permission:$status
        path filter --perm=user; echo ownership:$status
        path sort --key=unknown; echo key:$status
        path extension --unique; echo option:$status
        path change-extension; echo replacement:$status
        echo survived
    "#,
    );
    assert!(result.status.success());
    assert_eq!(
        result.stdout,
        b"type:2\npermission:2\nownership:2\nkey:2\noption:2\nreplacement:2\nsurvived\n"
    );
    assert_eq!(String::from_utf8_lossy(&result.stderr).lines().count(), 6);
}

#[test]
fn extension_queries_keep_dotted_unc_roots_intact() {
    let fixture = Fixture::new();
    assert_eq!(
        stdout(fixture.run(
            r#"
        path extension C:/ //server.example/share //server/share.name/
        echo root-extension:$status
        path change-extension txt //server.example/share //server/share.name/
        path extension //server.example/share/file.tar
    "#
        )),
        "\n\n\nroot-extension:1\n//server.example/share.txt\n//server/share.name/.txt\n.tar\n"
    );
}

#[test]
fn empty_filter_lists_remain_distinct_from_absent_filters() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"data").unwrap();
    assert_eq!(
        stdout(fixture.run(
            r#"
        path filter --type '' file; echo empty-type:$status
        path filter --type=, file; echo comma-type:$status
        path filter --type '' --type file file
        path filter --perm '' missing; echo empty-perm:$status
        path filter --perm=, missing; echo comma-perm:$status
        path filter --type '' --perm '' file; echo both-empty:$status
        path filter missing; echo absent:$status
    "#
        )),
        "empty-type:1\ncomma-type:1\nfile\nmissing\nempty-perm:0\nmissing\ncomma-perm:0\nboth-empty:1\nabsent:1\n"
    );
}

#[test]
fn path_character_devices_share_the_native_test_predicate() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"data").unwrap();
    assert_eq!(
        stdout(fixture.run(
            r#"
        test -c NUL; echo test-char:$status
        path is --type char NUL; echo path-char:$status
        path filter --type=char NUL file
        path filter --type=char --perm=read NUL
        path is -f NUL; echo not-file:$status
    "#
        )),
        "test-char:0\npath-char:0\nNUL\nNUL\nnot-file:1\n"
    );
}

#[test]
fn path_sort_rejects_excess_aggregate_input_before_emitting_output() {
    let fixture = Fixture::new();
    // Every record is small enough to pass the existing individual-input cap.
    let records = format!("{}\n", "x".repeat(4096)).repeat(4097);
    fs::write(fixture.0.join("bytes"), records).unwrap();
    fs::write(fixture.0.join("count"), vec![b'\n'; 100_001]).unwrap();
    for source in ["path sort < bytes", "path sort < count"] {
        let output = fixture.run(source);
        assert_eq!(output.status.code(), Some(1), "{source}");
        assert!(output.stdout.is_empty(), "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("path sort input limit exceeded"),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn path_substitutions_keep_explicit_fields_separate_from_adjacent_output() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("-dash"), b"data").unwrap();
    assert_eq!(
        stdout(fixture.run(
            r#"
        set v (printf prefix; path basename C:/a.txt C:/b.txt; printf suffix)
        count $v; printf '<%s>\n' $v
        set v (printf prefix; path extension plain file.txt plain; printf suffix)
        count $v; printf '<%s>\n' $v
        set v (printf prefix; path filter -- -dash; path sort z a; printf suffix)
        count $v; printf '<%s>\n' $v
    "#
        )),
        concat!(
            "4\n<prefix>\n<a.txt>\n<b.txt>\n<suffix>\n",
            "5\n<prefix>\n<>\n<.txt>\n<>\n<suffix>\n",
            "5\n<prefix>\n<./-dash>\n<a>\n<z>\n<suffix>\n"
        )
    );
}

#[test]
fn path_fields_cross_final_workers_and_serialize_for_byte_stream_consumers() {
    let fixture = Fixture::new();
    assert_eq!(
        stdout(fixture.run(
            r#"
        set v (printf prefix; printf '%s\n' plain file.txt | path extension; printf suffix)
        count $v; printf '<%s>\n' $v
        set v (printf prefix; path basename C:/a.txt C:/b.txt | string collect)
        count $v; printf '<%s>\n' $v
        path basename C:/a.txt C:/b.txt > values
        set v (printf prefix; string collect < values)
        count $v; printf '<%s>\n' $v
        set v (path extension -Z plain file.txt plain | string split0)
        count $v; printf '<%s>\n' $v
        set v (printf prefix; builtin realpath .; printf suffix); count $v
        set v (printf prefix; path resolve .; printf suffix); count $v
        path basename -Z C:/a.txt C:/b.txt
    "#
        )),
        concat!(
            "4\n<prefix>\n<>\n<.txt>\n<suffix>\n",
            "2\n<prefix>\n<a.txt\nb.txt>\n",
            "2\n<prefix>\n<a.txt\nb.txt>\n",
            "3\n<>\n<.txt>\n<>\n",
            "2\n3\n",
            "a.txt\0b.txt\0"
        )
    );
}
