//! Output failures must report a tool error, not panic while printing one.
#![cfg(windows)]

use std::fs::File;
use std::os::windows::io::FromRawHandle as _;
use std::process::{Command, Stdio};

fn closed_pipe() -> Stdio {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // Close the reader before launching the tool, avoiding a race between its
    // first write and the test dropping a piped stdout handle after spawn.
    unsafe {
        assert_ne!(
            windows_sys::Win32::System::Pipes::CreatePipe(
                &mut read,
                &mut write,
                std::ptr::null(),
                0,
            ),
            0
        );
        drop(File::from_raw_handle(read));
        Stdio::from(File::from_raw_handle(write))
    }
}

#[test]
fn closed_stdout_and_stderr_return_errors_without_panicking() {
    for tool in [
        env!("CARGO_BIN_EXE_fish_indent"),
        env!("CARGO_BIN_EXE_fish_key_reader"),
    ] {
        for argument in ["--help", "--unknown"] {
            let status = Command::new(tool)
                .arg(argument)
                .stdin(Stdio::null())
                .stdout(closed_pipe())
                .stderr(closed_pipe())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(1), "{tool} {argument}");
        }
    }
}
