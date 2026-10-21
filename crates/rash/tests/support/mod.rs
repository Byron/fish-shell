use std::io::Write as _;
use std::process::{Command, Output, Stdio};

// Pass source through stdin so byte-oriented oracle fixtures also work when the
// test harness runs on Windows, whose process arguments cannot contain raw bytes.
pub fn run_source(command: &mut Command, source: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run Bash oracle");
    child.stdin.take().unwrap().write_all(source).unwrap();
    child.wait_with_output().expect("wait for Bash oracle")
}
