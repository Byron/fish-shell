//! The same Rustup helpers work without FIFO process substitution on either host.
#![cfg(any(unix, all(windows, feature = "native-utilities")))]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-rustup-completions-{}-{} space",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn run(&self, source: &str) -> String {
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
            .args(["-N", "-c", &format!("{QUERIES}\n{source}")])
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_DIRS", self.0.join("vendor"))
            .env("TEMP", &self.0)
            .env("TMP", &self.0)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(
            !self.0.join("unexpected-query").exists(),
            "{}",
            std::fs::read_to_string(self.0.join("unexpected-query")).unwrap_or_default()
        );
        String::from_utf8(output.stdout).unwrap()
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
                    .starts_with("fish-rustup-completions-")
            })
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

const QUERIES: &str = r#"
function rustup
    set -l operation "$argv[1] $argv[2]"
    switch $operation
        case 'component list' 'target list'
            argparse 'toolchain=' installed -- $argv[3..]
            or begin
                echo malformed-arguments >> unexpected-query
                return 91
            end
            if test "$_flag_toolchain" != "$expected_toolchain"; or test "$RUSTUP_AUTO_INSTALL" != 0
                echo unexpected-toolchain-or-auto-install >> unexpected-query
                return 91
            end
            if test "$operation" = 'target list'
                printf '%s\n' $targets
            else if set -q _flag_installed
                if set -q installed_components[1]
                    printf '%s\n' $installed_components
                end
            else
                printf '%s\n' $components
            end
        case 'toolchain list'
            if set -q installed_toolchains[1]
                printf '%s\n' $installed_toolchains
            end
        case '*'
            printf 'unexpected-rustup-query %s\n' $argv >> unexpected-query
            return 91
    end
end
function psub
    echo unexpected-psub >> unexpected-query
    return 91
end
function comm
    echo unexpected-comm >> unexpected-query
    return 91
end
"#;

#[test]
fn component_names_strip_only_known_final_targets_and_exclude_installed_items() {
    for host in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        let fixture = Fixture::new();
        let output = fixture.run(&format!(
            r#"
            set -g targets {host} thumbv8m.main-none-eabi
            set -g components 'cargo-{host} (installed)' 'rustfmt-{host} (installed)' \
                rust-analyzer-{host} rust-src rust-std-thumbv8m.main-none-eabi \
                'rust-std-{host} (installed)' diagnostic-{host}-suffix special-thumbv8mXmain-none-eabi
            set -g installed_components 'cargo-{host} (installed)' 'rustfmt-{host} (installed)' 'rust-std-{host} (installed)'
            complete -C 'rustup component add rust'
            echo -- --installed
            complete -C 'rustup component remove rust' | __fish_sort
            echo -- --suffixes
            complete -C 'rustup component add diagnostic'
            complete -C 'rustup component add special'
            "#
        ));
        assert_eq!(
            output,
            format!(
                "rust-analyzer\nrust-src\n--installed\nrust-std\nrustfmt\n--suffixes\ndiagnostic-{host}-suffix\nspecial-thumbv8mXmain-none-eabi\n"
            )
        );
    }
}

#[test]
fn selected_toolchain_is_forwarded_as_two_arguments_and_empty_installed_lists_work() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g expected_toolchain nightly
            set -g targets aarch64-pc-windows-msvc
            set -g components rust-analyzer-aarch64-pc-windows-msvc rust-src
            set -g installed_components
            complete -C 'rustup --toolchain nightly component add rust'
            "#
        ),
        "rust-analyzer\nrust-src\n"
    );
}

#[test]
fn installable_toolchains_use_exact_membership_without_fifo_or_sort_requirements() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run(
            r#"
            set -g installed_toolchains nightly-test-host
            complete -C 'rustup toolchain install not-a-prefix' >/dev/null
            function __rustup_all_toolchains
                printf '%s\n' stable-test-host nightly-test-host beta-test-host
            end
            __rustup_installable_toolchains
            set -g installed_toolchains
            echo -- --empty
            __rustup_installable_toolchains
            "#
        ),
        "stable-test-host\nbeta-test-host\n--empty\nstable-test-host\nnightly-test-host\nbeta-test-host\n"
    );
}
