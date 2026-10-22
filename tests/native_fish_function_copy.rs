//! Function copies keep properties while remaining independent definitions.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-function-copy-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(root.join("config/fish/functions")).unwrap();
        Self(root)
    }

    fn write(&self, name: &str, source: &str) {
        std::fs::write(self.0.join(name), source).unwrap();
    }

    fn run(&self, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("PATH", "")
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .args(["-N", "-c", source])
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("fish-native-function-copy-")
            })
        {
            let _ = std::fs::remove_dir_all(root);
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

#[test]
fn copies_keep_descriptions_arguments_snapshots_and_shared_scope() {
    let output = stdout(Fixture::new().run(
        r#"
        set -g captured original 'two words'
        function original -d 'Copied description' -a argument -V captured -S
            printf '<%s>\n' $argument $captured
            echo caller:$label
            set label changed
        end
        functions --copy original 'copied-文!'
        functions -e original
        set -g captured changed
        function caller
            set -l label local
            copied-文! argument
            echo after:$label
        end
        caller
        functions copied-文!
    "#,
    ));
    assert!(
        output.starts_with("<argument>\n<original>\n<two words>\ncaller:local\nafter:changed\n"),
        "{output}"
    );
    assert!(output.contains("function copied-文! --description 'Copied description' --no-scope-shadowing --argument-names argument"), "{output}");
}

#[test]
fn copy_errors_set_status_without_overwriting_functions_or_stopping_the_caller() {
    let output = Fixture::new().run(
        r#"
        function original; echo original; end
        function existing; echo existing; end
        functions -c original
        echo arity:$status
        functions --copy missing destination
        echo missing:$status
        functions -c original 'bad/name'
        echo invalid:$status
        functions -c original if
        echo reserved:$status
        functions -c original original
        echo self:$status
        functions -c original existing
        echo overwrite:$status
        existing
        functions -cq original other
        echo conflict:$status
        functions --copy=invalid original other
        echo option:$status
        functions --copy original -- --bad
        echo dash:$status
        functions original --copy permuted
        echo permuted:$status
        permuted
        functions -q permuted
        echo query:$status
        functions -e permuted
        functions -q permuted
        echo erased:$status
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "arity:2\nmissing:1\ninvalid:2\nreserved:2\nself:1\noverwrite:1\nexisting\nconflict:2\noption:2\ndash:2\npermuted:0\noriginal\nquery:0\nerased:1\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("functions:"));
}

#[test]
fn copies_do_not_duplicate_event_handlers_or_completion_wraps() {
    let output = stdout(Fixture::new().run(
        r#"
        set -g observed
        function original --on-event copy_probe --on-variable tracked --wraps source_command
            set -ga observed fired
        end
        complete -c source_command -f -a wrapped_choice
        complete -c copied -f
        functions -c original copied
        emit copy_probe
        set -g tracked changed
        echo events:(count $observed)
        complete -C 'original '
        complete -C 'copied '
        functions copied
    "#,
    ));
    assert!(
        output.starts_with("events:2\nwrapped_choice\nfunction copied\n"),
        "{output}"
    );
    assert!(
        !output.contains("--on-event")
            && !output.contains("--on-variable")
            && !output.contains("--wraps"),
        "{output}"
    );
}

#[test]
fn source_and_destination_autoload_are_respected_and_copies_survive_path_changes() {
    let fixture = Fixture::new();
    fixture.write(
        "config/fish/functions/from_disk.fish",
        "set -g source_loaded yes; function from_disk; status filename; end",
    );
    fixture.write(
        "config/fish/functions/occupied.fish",
        "set -g destination_loaded yes; function occupied; echo occupied; end",
    );
    fixture.write(
        "config/fish/functions/tombstoned.fish",
        "echo wrong; function tombstoned; echo wrong; end",
    );
    fixture.write(
        "copy_place.fish",
        "functions -c from_disk disk_copy; disk_copy; status filename",
    );
    let output = fixture.run(
        r#"
        source copy_place.fish
        echo source:$source_loaded
        functions -c from_disk occupied
        echo occupied-status:$status
        echo destination:$destination_loaded
        occupied
        functions -e tombstoned
        functions -c from_disk tombstoned
        functions -e from_disk
        set fish_function_path
        disk_copy | string upper
        tombstoned
        status filename
    "#,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "copy_place.fish\ncopy_place.fish\nsource:yes\noccupied-status:1\ndestination:yes\noccupied\nCOPY_PLACE.FISH\nStandard input\nStandard input\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
}

#[test]
fn destination_autoload_can_erase_the_source_without_creating_a_stale_copy() {
    let fixture = Fixture::new();
    fixture.write(
        "config/fish/functions/destination.fish",
        "functions -e original",
    );
    let output = fixture.run("function original; echo stale; end; functions -c original destination; echo copy:$status; functions -q destination; echo exists:$status");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "copy:1\nexists:1\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not exist"));
}

#[test]
fn pipeline_workers_keep_copy_properties_and_only_merge_the_last_stage() {
    let output = stdout(Fixture::new().run(
        r#"
        set captured snapshot
        function original -d Description -a argument -V captured
            echo $argument $captured
            status filename
        end
        true | functions -c original copied
        copied value | string upper
        functions -c original isolated | string upper
        functions -q isolated
        echo isolated:$status
        copied final
    "#,
    ));
    assert_eq!(
        output,
        "VALUE SNAPSHOT\nSTANDARD INPUT\nisolated:1\nfinal snapshot\nStandard input\n"
    );
}
