//! Private utility routing must remain parseable when native builtins are absent.
#![cfg(any(unix, all(windows, feature = "native-utilities")))]

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-utility-wrappers-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn sort_and_find_autoload_with_the_available_backend() {
    let root = std::env::temp_dir().join(format!(
        "fish-utility-wrappers-{}-{} space",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    std::fs::write(fixture.0.join("needle.txt"), "").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
    command.env_clear();
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    #[cfg(windows)]
    command.env("PATH", "");
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    let output = command
        .args([
            "-N",
            "-c",
            "printf 'z\\na\\n' | __fish_sort; __fish_find $PWD -name needle.txt | path basename",
        ])
        .current_dir(&fixture.0)
        .env("HOME", &fixture.0)
        .env("USERPROFILE", &fixture.0)
        .env("FISH_NATIVE_CONFIG_HOME", fixture.0.join("config"))
        .env("XDG_CONFIG_HOME", fixture.0.join("config"))
        .env("XDG_DATA_HOME", fixture.0.join("data"))
        .env("XDG_CACHE_HOME", fixture.0.join("cache"))
        .env("XDG_DATA_DIRS", fixture.0.join("vendor"))
        .env("TEMP", &fixture.0)
        .env("TMP", &fixture.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(output.stdout, b"a\nz\nneedle.txt\n");
}
