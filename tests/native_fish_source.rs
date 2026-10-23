//! Source input, scope, and filename context through the native executable.
#![cfg(windows)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-native-source-{}-{} space 文",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(root.join("config/fish/functions")).unwrap();
        Self(root)
    }

    fn write(&self, name: &str, source: impl AsRef<[u8]>) {
        std::fs::write(self.0.join(name), source).unwrap();
    }

    fn command(&self) -> Command {
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
            .env("TMP", &self.0);
        command
    }

    fn run(&self, source: &str, input: &[u8]) -> Output {
        let mut child = self
            .command()
            .args(["-N", "-c", source])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
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
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-native-source-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn text(output: Output) -> String {
    assert!(
        output.status.success(),
        "status {:?}, stderr {}",
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
fn stdin_source_populates_the_live_session_and_preserves_source_local_scope() {
    let fixture = Fixture::new();
    let program = b"set -l source_local hidden\nset -g loaded yes\nfunction installed -d 'From generated input'; status filename; echo installed; end\ncomplete -c source_probe -f -a ready\nstring upper generated\n";
    assert_eq!(text(fixture.run("set argv outer; source; echo loaded:$loaded; installed; complete -C 'source_probe '; set -q source_local; echo local:$status; echo argv:$argv; status filename", program)), "GENERATED\nloaded:yes\n-\ninstalled\nready\nlocal:1\nargv:outer\nStandard input\n");
    assert_eq!(text(fixture.run("printf '%s\\n' 'set -g from_pipeline yes' 'string upper inherited' | source; echo $from_pipeline", b"")), "INHERITED\nyes\n");
}

#[test]
fn explicit_stdin_dot_alias_and_source_arguments_use_fish_option_ordering() {
    let fixture = Fixture::new();
    let source = "status -f\r\nprintf '<%s>\\n' $argv\r\necho 文";
    assert_eq!(
        text(fixture.run(". - one 'two words' --help", source.as_bytes())),
        "-\n<one>\n<two words>\n<--help>\n文\n"
    );
    fixture.write("-named.fish", "printf '<%s>\\n' $argv");
    assert_eq!(
        text(fixture.run("source -- -named.fish --help", b"")),
        "<--help>\n"
    );
}

#[test]
fn source_preserves_empty_status_and_consumes_only_explicit_source_exit() {
    let fixture = Fixture::new();
    fixture.write("exit.fish", "status filename; exit 37; echo wrong");
    fixture.write("return.fish", "set -l private local; return 23; echo wrong");
    fixture.write("empty.fish", "# nothing to execute\n");
    assert_eq!(text(fixture.run("false; source empty.fish; echo empty:$status; false; source; echo stdin:$status; source exit.fish; echo exit:$status; source return.fish; echo return:$status; set -q private; echo private:$status; status filename", b"")), "empty:1\nstdin:1\nexit.fish\nexit:37\nreturn:23\nprivate:1\nStandard input\n");
    assert_eq!(
        text(fixture.run("source -; echo survived:$status", b"exit 41\necho wrong\n")),
        "survived:41\n"
    );
}

#[test]
fn source_reads_all_input_before_parsing_or_running_commands() {
    let fixture = Fixture::new();
    let invalid = fixture.run(
        "source; echo source:$status; set -q changed; echo changed:$status",
        b"set -g changed yes\nif\n",
    );
    assert!(invalid.status.success());
    assert_eq!(
        String::from_utf8(invalid.stdout).unwrap(),
        "source:1\nchanged:1\n"
    );
    assert!(!invalid.stderr.is_empty());
    assert_eq!(
        text(fixture.run(
            "source -; echo source:$status",
            b"read value\necho read:$status\necho done\n"
        )),
        "read:1\ndone\nsource:0\n"
    );
}

#[test]
fn failed_open_read_and_closed_stdin_set_status_and_allow_caller_continuation() {
    let fixture = Fixture::new();
    fixture.write("invalid.fish", [0xff, b'\n']);
    let output = fixture.run("source missing.fish; echo missing:$status; source invalid.fish; echo utf8:$status; source <&-; echo closed:$status; source - <&-; echo explicit-closed:$status; true; source <NUL; echo null:$status; source --unknown; echo option:$status; status filename", b"");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "missing:1\nutf8:1\nclosed:1\nexplicit-closed:1\nnull:0\noption:2\nStandard input\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("source:"));
}

#[test]
fn file_and_function_origins_survive_nested_sources_and_pipeline_workers() {
    let fixture = Fixture::new();
    fixture.write("inner space 文.fish", "status --filename; printf '<%s>\\n' $argv; function inner_defined; status current-filename; end; return 7");
    fixture.write("outer.fish", "status filename; interactive_defined; source 'inner space 文.fish' arg; echo inner-status:$status; status --current-filename; function outer_defined; status filename; function nested_defined; status filename; end; end");
    assert_eq!(text(fixture.run("function interactive_defined; status filename; end; source outer.fish; inner_defined; outer_defined; outer_defined | string upper; nested_defined; status filename", b"")), "outer.fish\nStandard input\ninner space 文.fish\n<arg>\ninner-status:7\nouter.fish\ninner space 文.fish\nouter.fish\nOUTER.FISH\nouter.fish\nStandard input\n");
}

#[test]
fn top_level_scripts_and_startup_sources_keep_distinct_origins_and_exit_policy() {
    let fixture = Fixture::new();
    fixture.write("script space 文.fish", "status filename; printf '<%s>\\n' $argv; function defined_in_script; status filename; end; defined_in_script | string upper");
    let output = fixture
        .command()
        .args(["-N", "script space 文.fish", "argument"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        text(output),
        "script space 文.fish\n<argument>\nSCRIPT SPACE 文.FISH\n"
    );
    fixture.write(
        "config/fish/config.fish",
        "status filename; exit 29; echo wrong",
    );
    let output = fixture
        .command()
        .args(["-c", "echo wrong"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(29));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("config.fish\n")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn autoloaded_function_origin_is_retained_after_the_loader_returns() {
    let fixture = Fixture::new();
    fixture.write(
        "config/fish/functions/from_disk.fish",
        "function from_disk; status filename; end",
    );
    let output = text(fixture.run("functions -q from_disk; status filename; from_disk; from_disk | string upper; status filename", b""));
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines[0], "Standard input");
    assert!(lines[1].ends_with("from_disk.fish"), "{output}");
    assert_eq!(lines[2], lines[1].to_uppercase());
    assert_eq!(lines[3], "Standard input");
}
