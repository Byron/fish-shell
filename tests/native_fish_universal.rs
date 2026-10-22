//! Persistent native variables, isolated from the developer's Fish home.
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
                "fish-native-universal-{}-{} space 文",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create universal fixture: {error}"),
            }
        }
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
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env("APPDATA", self.0.join("roaming"))
            .env("LOCALAPPDATA", self.0.join("local"))
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("FISH_TEST_EXE", env!("CARGO_BIN_EXE_fish"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null());
        command
    }
    fn file(&self) -> PathBuf {
        self.0.join("config/fish/native_fish_variables")
    }
    fn output(&self, source: &str) -> Output {
        self.command().args(["-c", source]).output().unwrap()
    }
    fn run(&self, source: &str) -> String {
        let output = self.output(source);
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = self.0.canonicalize().unwrap();
        let temp = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(root.parent(), Some(temp.as_path()));
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn universals_persist_lists_flags_and_native_path_separators() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run("echo ready"), "ready\n");
    assert!(
        !fixture.0.join("config").exists(),
        "reading an absent store must create nothing"
    );
    fixture.run(r#"set -U empty_list; set -U empty_value ''; set -U words 'one two' 'line\nbreak' 文; set -Ux --path custom 'C:\one;D:\two'; set -U --unpath ordinaryPATH 'C:\one;D:\two'"#);
    assert!(fixture.file().is_file());
    assert_eq!(fixture.run(r#"count $empty_list; count $empty_value; printf '<%s>\n' $words; count $custom; echo "$custom"; count $ordinaryPATH; echo "$ordinaryPATH"; set -qU --path -x custom; echo flags=$status; set -qU --unpath ordinaryPATH; echo unpath=$status; set -nU"#),
        "0\n1\n<one two>\n<line\\nbreak>\n<文>\n2\nC:\\one;D:\\two\n1\nC:\\one;D:\\two\nflags=0\nunpath=0\ncustom\nempty_list\nempty_value\nordinaryPATH\nwords\n");
}

#[test]
fn scopes_and_exported_inheritance_keep_universal_identity() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(r#"
        set -Ux native_value original
        set -gu native_value hidden
        $FISH_TEST_EXE -N -c 'set -q native_value; echo hidden=$status'
        set -eg native_value
        $FISH_TEST_EXE -c 'set -qg native_value; echo global=$status; set native_value child'
        echo updated=$native_value
        native_value=override $FISH_TEST_EXE -c 'set -qg native_value; echo override=$status; set native_value private'
        echo retained=$native_value
        begin
            set -l native_value local
            set native_value changed_local
            echo $native_value
        end
        echo $native_value
        set -eU native_value
        set -q native_value
        echo erased=$status
        set -eU native_value
        echo missing=$status
    "#), "hidden=1\nglobal=1\nupdated=child\noverride=0\nretained=child\nchanged_local\nchild\nerased=1\nmissing=4\n");
}

#[test]
fn scope_shadows_inherit_path_metadata_but_keep_their_own_export_flag() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
        set -Ux --path selected one two
        set -g selected global
        set -qxg selected
        echo new_global=$status
        set -qg --path selected
        echo path=$status
        set -eg selected
        begin
            set -l selected local
            set -qxl selected
            echo new_local=$status
            set -lx selected exported
            set -l selected changed
            set -qxl selected
            echo existing_local=$status
        end
        set -gx selected base
        begin
            set -lu --unpath selected local1 local2
            set -ga selected more
        end
        set -qxg selected
        echo existing_global=$status
        set -qg --unpath selected
        echo unpath=$status
        echo "$selected"
        set -Ua selected tail
        set -eg selected
        echo "$selected"
    "#
        ),
        "new_global=1\npath=0\nnew_local=1\nexisting_local=0\nexisting_global=0\nunpath=0\nlocal1 local2 more\nlocal1;local2;more;tail\n"
    );
}

#[test]
fn universal_events_cover_same_values_remote_changes_and_reentrant_setters() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
        function watch --on-variable tracked
            printf 'event:%s:%s\n' "$argv" "$tracked"
            set -U handler_ran yes
        end
        false
        set -U tracked first
        echo status=$status
        set -U tracked first
        set -U --no-event tracked quiet
        $FISH_TEST_EXE -c 'set -U tracked remote'
        echo observed=$tracked
        set -eU tracked
        set -qU handler_ran
        echo handler=$status
    "#
        ),
        "event:VARIABLE SET tracked:first\nstatus=1\nevent:VARIABLE SET tracked:first\nevent:VARIABLE SET tracked:remote\nobserved=remote\nevent:VARIABLE ERASE tracked:\nhandler=0\n"
    );
}

#[test]
fn pipeline_workers_merge_disk_before_replacing_the_parent_snapshot() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
        set -U old stale
        begin; set -eU old; set -U first_stage saved; end | count
        echo first=$first_stage
        set -qU old
        echo erased=$status
        echo input | set -U final_stage saved
        echo final=$final_stage
        set -U another saved
        echo (begin; set -U substitution value; end | count) $substitution
    "#
        ),
        "0\nfirst=saved\nerased=1\nfinal=saved\n0 value\n"
    );
    assert_eq!(
        fixture.run("set -qU old; echo $status; echo $first_stage $final_stage $another"),
        "1\nsaved saved saved\n"
    );
}

#[test]
fn no_config_uses_globals_without_reading_or_mutating_the_store() {
    let fixture = Fixture::new();
    fixture.run("set -U existing disk");
    let before = std::fs::read(fixture.file()).unwrap();
    let output = fixture.command().args(["-N", "-c", "set -q existing; echo read=$status; set -U created memory; set -qg created; echo global=$status; set -qU created; echo universal=$status; set -eU created; echo erase=$status; echo $created"]).output().unwrap();
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "read=1\nglobal=0\nuniversal=1\nerase=4\nmemory\n"
    );
    assert_eq!(std::fs::read(fixture.file()).unwrap(), before);
    assert_eq!(
        fixture.run("set -q created; echo $status; echo $existing"),
        "1\ndisk\n"
    );
}

#[test]
fn future_file_and_failed_writes_leave_alias_startup_and_shell_usable() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("config/fish/functions")).unwrap();
    std::fs::write(
        fixture.0.join("config/fish/functions/saved_alias.fish"),
        "function saved_alias --description 'saved alias'; echo alias-loaded; end\n",
    )
    .unwrap();
    let future = b"# VERSION: 99.0\nSETUVAR existing:readable\nFUTURE something\n";
    std::fs::write(fixture.file(), future).unwrap();
    let output = fixture.output("saved_alias; echo $existing; set -U existing changed; echo failure=$status; echo $existing");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "alias-loaded\nreadable\nfailure=1\nreadable\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to overwrite"));
    assert_eq!(std::fs::read(fixture.file()).unwrap(), future);
    std::fs::write(fixture.file(), b"# VERSION: 3.0\nSETUVAR broken\n").unwrap();
    let output = fixture
        .output("saved_alias; echo still-running; set -U attempted value; echo failure=$status");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "alias-loaded\nstill-running\nfailure=1\n"
    );
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(errors.matches("fish: universal variables:").count(), 1);
    assert!(errors.contains("malformed universal-variable records"));
}

#[test]
fn path_metadata_survives_prefixes_arguments_captures_and_loop_bindings() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
        set -U --path custom 'C:\one;D:\two'
        set -g PATH 'C:\one;D:\two'
        PATH=$PATH $FISH_TEST_EXE -N -c 'count $PATH; echo "$PATH"'
        function named --argument-names custom
            count $custom
            echo "$custom"
        end
        named 'E:\three;F:\four'
        function captured --inherit-variable custom
            echo "$custom"
        end
        captured
        set -U --unpath custom plain
        captured
        for custom in 'E:\three;F:\four'
            count $custom
        end
    "#
        ),
        "2\nC:\\one;D:\\two\n2\nE:\\three;F:\\four\nC:\\one;D:\\two\nC:\\one D:\\two\n1\n"
    );
}

#[test]
fn universal_indexes_use_the_target_scope_and_preserve_failed_transactions() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
        set -Ux --path shared one two three
        set -gu --unpath shared shadow
        set -U shared[-1] changed
        set -eU shared[1]
        set -eg shared
        echo "$shared"
        set -qxU --path shared
        echo flags=$status
    "#
        ),
        "two;changed\nflags=0\n"
    );
    let before = std::fs::read(fixture.file()).unwrap();
    let output = fixture.output("set -U shared[0] wrong; echo failure=$status; echo \"$shared\"");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "failure=2\ntwo;changed\n"
    );
    assert_eq!(std::fs::read(fixture.file()).unwrap(), before);
}

#[test]
fn concurrent_universal_slice_erasure_resolves_negative_indexes_under_the_lock() {
    let fixture = Fixture::new();
    fixture.run(&format!(
        "set -U shared {}",
        (0..40)
            .map(|index| format!("v{index}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    let mut children: Vec<_> = (0..4)
        .map(|_| {
            fixture
                .command()
                .args([
                    "-c",
                    "for number in 1 2 3 4 5 6 7 8 9 10; set -eU shared[-1]; end",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
    }
    assert_eq!(
        fixture.run("count $shared; set -qU shared; echo exists=$status"),
        "0\nexists=0\n"
    );
}
