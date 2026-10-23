//! Record-preserving string operations use the native capture transport.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fish-native-records-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
        command.env_clear();
        if let Some(value) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", value);
        }
        command
            .args(["-N", "-c", source])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .output()
            .unwrap()
    }

    fn check(&self, source: &str, status: i32, expected: &[u8]) {
        let result = self.run(source);
        assert_eq!(result.status.code(), Some(status), "{source}: {result:?}");
        assert!(result.stderr.is_empty(), "{source}: {result:?}");
        assert_eq!(result.stdout, expected, "{source}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let (Ok(path), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
            && path.parent() == Some(temp.as_path())
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fish-native-records-")
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[test]
fn collect_preserves_empty_multiline_and_argument_records() {
    Fixture::new().check(
        r#"
        set v (true | string collect); count $v
        set v (true | string collect -a); count $v
        set v (printf '\n' | string collect); count $v
        set v (printf '\n' | string collect -a); count $v
        set v (string collect ''); count $v
        set v (string collect -a ''); count $v
        set v (printf 'a\nb\n' | string collect); count $v; printf '<%s>\n' $v
        set v (printf 'a\nb\n' | string collect -N); count $v; printf '<%s>\n' $v
        set v (string collect a b); count $v; printf '<%s>\n' $v
    "#,
        0,
        b"0\n1\n1\n2\n1\n2\n1\n<a\nb>\n1\n<a\nb\n>\n2\n<a>\n<b>\n",
    );
}

#[test]
fn collect_physical_output_follows_posix_newline_and_status_rules() {
    let fixture = Fixture::new();
    for (source, status, expected) in [
        ("string collect a b", 0, &b"a\nb\n"[..]),
        ("string collect ''", 1, b"\n"),
        ("true | string collect", 1, b""),
        ("true | string collect -a", 1, b"\n"),
        ("printf 'a\\nb\\n\\n' | string collect", 0, b"a\nb"),
        ("printf 'a\\nb\\n' | string collect -N", 0, b"a\nb\n"),
        ("printf '\\n' | string collect", 1, b""),
        ("printf '\\n' | string collect -a", 1, b"\n"),
    ] {
        fixture.check(source, status, expected);
    }
}

#[test]
fn split0_preserves_newlines_and_only_discards_the_final_empty_field() {
    Fixture::new().check(
        r#"
        set v (printf 'a\nb\0\0c\0' | string split0)
        count $v; printf '<%s>\n' $v
        set v (printf '\0' | string split0); count $v; printf '<%s>\n' $v
        set v (string split0 ''); count $v
        set v (true | string split0); count $v
        set v (printf 'a\0b\0' | string split0 -f2,1); printf '<%s>\n' $v
        printf 'a\0b\0' | string split0 -f3; echo status:$status
        printf 'a\0b\0' | string split0 -af3; echo status:$status
    "#,
        0,
        b"3\n<a\nb>\n<>\n<c>\n1\n<>\n0\n0\n<b>\n<a>\nstatus:1\nstatus:0\n",
    );
}

#[test]
fn join0_emits_ordinary_nul_bytes_and_a_terminal_delimiter() {
    let fixture = Fixture::new();
    fixture.check("string join0 a b", 0, b"a\0b\0");
    fixture.check("string join0 a", 1, b"a\0");
    fixture.check("string join0", 1, b"");
    fixture.check("string join0 -n '' a '' b", 0, b"a\0b\0");
    fixture.check("string join0 -nq '' ''", 0, b"");
    fixture.check("string join -nq : '' ''", 0, b"");
    fixture.check("printf 'a\\nb' | string join0", 0, b"a\0b\0");
    fixture.check(
        r#"set v (string join0 'a
b' '' c | string split0); count $v; printf '<%s>\n' $v"#,
        0,
        b"3\n<a\nb>\n<>\n<c>\n",
    );
}

#[test]
fn records_survive_nested_workers_and_serialize_in_middle_pipeline_stages() {
    Fixture::new().check(
        r#"
        set v (echo input | begin
            read ignored
            string collect first
            set inner (printf 'a\nb\n' | string collect)
            string collect -N $inner
            string split0 ''
            string collect last
        end)
        count $v; printf '<%s>\n' $v
        printf 'a\nb\n' | string collect | string length
    "#,
        0,
        b"3\n<first>\n<a\nb>\n<last>\n1\n1\n",
    );
}

#[test]
fn quoted_substitution_slices_select_fields_before_joining() {
    Fixture::new().check(
        r#"
        printf '<%s>\n' "$(printf 'a\nb\nc\n')[2]"
        printf '<%s>\n' "$(printf 'a\nb\nc\n')[3..2]"
        printf '<%s>\n' "$(printf 'a\r\nb\r\n')[2]"
        printf '<%s>\n' "$(printf 'a\nb\n')[9]"
        printf '<%s>\n' "$(string split , 'a,,')[2]"
        printf '<%s>\n' "$(printf 'a\nb\n' | string collect)[1]"
    "#,
        0,
        b"<b>\n<c\nb>\n<b\r>\n<>\n<>\n<a\nb>\n",
    );
}

#[test]
fn record_option_errors_and_field_limits_leave_the_shell_usable() {
    let fixture = Fixture::new();
    for source in [
        "string collect -q value",
        "string split0 --allow-empty value",
        "echo value | string collect extra",
    ] {
        let result = fixture.run(&format!("{source}; echo status:$status"));
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"status:2\n");
        assert!(!result.stderr.is_empty());
    }
    let result = fixture.run("string repeat -n100001 , | string split ,; echo status:$status");
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, b"status:2\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("field limit exceeded"));
    fixture.check(
        "string repeat -n100001 , | string split -n ,; echo status:$status",
        0,
        b"status:1\n",
    );
}

#[test]
fn whole_stream_reads_handle_large_records_and_reject_overflow() {
    let fixture = Fixture::new();
    fixture.check(
        "string repeat -n1048576 x | string collect | string length",
        0,
        b"1048576\n",
    );
    std::fs::write(
        fixture.0.join("too-large"),
        vec![b'x'; 16 * 1024 * 1024 + 1],
    )
    .unwrap();
    let result = fixture.run("string collect < too-large; echo status:$status");
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, b"status:122\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("input record limit exceeded"));
}
