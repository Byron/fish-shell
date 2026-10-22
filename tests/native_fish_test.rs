#![cfg(windows)]
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

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

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fish-native-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn quote(&self, name: &str) -> String {
        format!(
            "'{}'",
            self.0
                .join(name)
                .to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "\\'")
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        let Ok(root) = self.0.canonicalize() else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-test-"))
        {
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[test]
fn standard_config_predicates_allow_later_aliases_to_load() {
    let directory = Fixture::new();
    fs::create_dir_all(directory.0.join("fish/functions")).unwrap();
    fs::write(
        directory.0.join("fish/config.fish"),
        r#"
        if test -x /opt/homebrew/bin/brew
            echo unexpected-brew
        end
        if test -t 0
            echo unexpected-terminal
        end
        function configured_alias --description 'Loaded after portable tests'
            echo alias-loaded
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .env_clear()
        .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
        .env("HOME", &directory.0)
        .env("USERPROFILE", &directory.0)
        .env("FISH_NATIVE_CONFIG_HOME", &directory.0)
        .args(["-c", "configured_alias"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"alias-loaded\n");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_and_bracket_use_posix_arity_precedence_and_parentheses() {
    assert_eq!(
        run(r#"
        test 0 = 0 -o 0 = 1 -a 0 = 2; echo or=$status
        test '(' 0 = 0 -o 0 = 1 ')' -a 0 = 2; echo paren=$status
        test ! ! ! A; echo bang=$status
        test -S = -S; echo operator-string=$status
        test foo -a bar; echo strings=$status
        [ -n x -a '(' -z '' -o 5 -gt 10 ')' ]; echo brackets=$status
        test; echo empty=$status
        test ''; echo empty-string=$status
        test --; echo literal-option=$status
        test --help; echo literal-help=$status
        [ ]; echo empty-brackets=$status
    "#),
        "or=0\nparen=1\nbang=1\noperator-string=0\nstrings=0\nbrackets=0\nempty=2\nempty-string=1\nliteral-option=0\nliteral-help=0\nempty-brackets=2\n"
    );
}

#[test]
fn numeric_comparisons_preserve_large_integers_and_fractional_values() {
    assert_eq!(
        run(r#"
        test 4611686018427387904 -eq 4611686018427387905; echo exact=$status
        test 9223372036854775807 -gt 9223372036854775806; echo max=$status
        test -9223372036854775808 -lt -9223372036854775807; echo min=$status
        test ' 2 ' -eq 2; echo whitespace=$status
        test -1.25 -lt -1.2; echo fraction=$status
        test 1e2 -eq 100; echo exponent=$status
        test 0x1p2 -eq 4; echo hexadecimal=$status
        test 0 -eq 0 -o bad -eq 1; echo short-circuit=$status
    "#),
        "exact=1\nmax=0\nmin=0\nwhitespace=0\nfraction=0\nexponent=0\nhexadecimal=0\nshort-circuit=0\n"
    );
}

#[test]
fn malformed_tests_report_status_and_allow_the_script_to_continue() {
    let output = fish(
        r#"
        test inf -gt 0; echo infinite=$status
        test 5 -eq nan; echo nan=$status
        test 999999999999999999999 -eq 1; echo overflow=$status
        test 2x -eq 2; echo suffix=$status
        test 1 =; echo missing=$status
        [ value; echo bracket=$status
        echo continued
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"infinite=2\nnan=2\noverflow=2\nsuffix=2\nmissing=1\nbracket=2\ncontinued\n"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("Number is infinite\n"), "{error}");
    assert!(error.contains("Not a number\n"), "{error}");
    assert!(
        error.contains("Result too large: 999999999999999999999\n"),
        "{error}"
    );
    assert!(
        error.contains("Integer 2 in '2x' followed by non-digit\n"),
        "{error}"
    );
    assert!(
        error.contains("test: Missing argument at index 3\n1 =\n    ^\n"),
        "{error}"
    );
    assert!(
        error.contains("[: the last argument must be ']'\n"),
        "{error}"
    );
}

#[test]
fn native_file_access_checks_do_not_change_contents_or_timestamps() {
    let directory = Fixture::new();
    let file = directory.0.join("value.txt");
    fs::write(&file, b"preserved contents").unwrap();
    let modified = fs::metadata(&file).unwrap().modified().unwrap();
    let path = directory.quote("value.txt");
    let dir = directory.quote("");
    let missing = directory.quote("missing");
    assert_eq!(
        run(&format!(
            r#"
        test -e {path}; echo exists=$status
        test -f {path}; echo file=$status
        test -d {dir}; echo directory=$status
        test -s {path}; echo size=$status
        test -r {path}; echo read=$status
        test -w {path}; echo write=$status
        test -x {path}; echo execute=$status
        test -x {dir}; echo search=$status
        test -x {missing}; echo missing=$status
        test -L {path}; echo link=$status
        test -b {path}; echo block=$status
        test -c NUL; echo device=$status
    "#
        )),
        "exists=0\nfile=0\ndirectory=0\nsize=0\nread=0\nwrite=0\nexecute=0\nsearch=0\nmissing=1\nlink=1\nblock=1\ndevice=0\n"
    );
    assert_eq!(fs::read(&file).unwrap(), b"preserved contents");
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions.clone()).unwrap();
    let output = run(&format!("test -w {path}; echo readonly=$status"));
    permissions.set_readonly(false);
    fs::set_permissions(&file, permissions).unwrap();
    assert_eq!(output, "readonly=1\n");
}

#[test]
fn file_identity_and_timestamp_queries_follow_native_filesystem_metadata() {
    let directory = Fixture::new();
    fs::write(directory.0.join("original.txt"), b"first").unwrap();
    fs::hard_link(
        directory.0.join("original.txt"),
        directory.0.join("hardlink.txt"),
    )
    .unwrap();
    fs::write(directory.0.join("newer.txt"), b"second").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(directory.0.join("original.txt"))
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)),
        )
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(directory.0.join("newer.txt"))
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000)),
        )
        .unwrap();
    let original = directory.quote("original.txt");
    let hardlink = directory.quote("hardlink.txt");
    let newer = directory.quote("newer.txt");
    let missing = directory.quote("missing");
    let dir = directory.quote("");
    assert_eq!(
        run(&format!(
            r#"
        test {original} -ef {hardlink}; echo hardlink=$status
        test {original} -ef {newer}; echo different=$status
        test {dir} -ef {dir}; echo directory=$status
        test {missing} -ef {missing}; echo missing-same=$status
        test {newer} -nt {original}; echo newer=$status
        test {original} -ot {newer}; echo older=$status
        test {original} -nt {missing}; echo present-newer=$status
        test {missing} -ot {original}; echo absent-older=$status
        test {missing} -nt {missing}; echo missing-newer=$status
    "#
        )),
        "hardlink=0\ndifferent=1\ndirectory=0\nmissing-same=1\nnewer=0\nolder=0\npresent-newer=0\nabsent-older=0\nmissing-newer=1\n"
    );
    // Symlink creation may require Developer Mode or a privilege. When available,
    // verify -L checks the link itself while -ef follows its target.
    match std::os::windows::fs::symlink_file(
        directory.0.join("original.txt"),
        directory.0.join("symbolic.txt"),
    ) {
        Ok(()) => {
            eprintln!("native test symlink coverage: exercised");
            let symbolic = directory.quote("symbolic.txt");
            assert_eq!(
                run(&format!(
                    "test -L {symbolic}; echo link=$status; test {symbolic} -ef {original}; echo target=$status"
                )),
                "link=0\ntarget=0\n"
            );
        }
        Err(error) => eprintln!("native test symlink coverage: skipped ({error})"),
    }
}

#[test]
fn ownership_predicates_are_explicit_without_fabricating_unix_metadata() {
    let directory = Fixture::new();
    let existing = directory.quote("");
    let missing = directory.quote("missing");
    let output = fish(&format!(
        r#"
        test -O {existing}; echo owner=$status
        test -G {existing}; echo group=$status
        test -u {existing}; echo setuid=$status
        test -g {existing}; echo setgid=$status
        test -k {existing}; echo sticky=$status
        test -O {missing}; echo missing=$status
    "#
    ));
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"owner=2\ngroup=2\nsetuid=2\nsetgid=2\nsticky=2\nmissing=1\n"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert_eq!(error.lines().count(), 5);
    assert!(
        error
            .lines()
            .all(|line| line.contains("requires POSIX ownership or mode bits")),
        "{error}"
    );
}

#[test]
fn tty_queries_respect_capture_redirects_and_unavailable_descriptors() {
    assert_eq!(
        run(r#"
        test -t 0; echo stdin=$status
        test -t 1; echo stdout=$status
        test -t 2; echo stderr=$status
        test -t 0 <&-; echo closed=$status
        test -t 99; echo unavailable=$status
        test -t 0.5; echo fractional=$status
        test -t 2147483648; echo overflow-fd=$status
        set captured (test -t 1; echo $status); echo capture=$captured
        set captured (test -t 2 2>&1; echo $status); echo duplicated-capture=$captured
        printf 'line\n' | test -t 0; echo pipe=$status
    "#),
        "stdin=1\nstdout=1\nstderr=1\nclosed=1\nunavailable=1\nfractional=1\noverflow-fd=1\ncapture=1\nduplicated-capture=1\npipe=1\n"
    );
}
