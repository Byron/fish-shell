//! Execution feasibility probe: a persistent Bash with a private control socket.
//! Not connected to the interactive reader until terminal and signal semantics are proven.

use std::collections::BTreeMap;
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DRIVER: &str = r#"
set +H
__fish_probe_status=0
while IFS= read -r -d '' -u 9 __fish_probe_input; do
    (exit "$__fish_probe_status")
    builtin eval -- "$__fish_probe_input"
    __fish_probe_status=$?
    builtin printf '%s\0%s\0' "$__fish_probe_status" "$PWD" >&9
    for __fish_probe_name in $(builtin compgen -v); do
        case $__fish_probe_name in
            __fish_probe_*|BASH*|EUID|UID|PPID|SHELLOPTS|SHELL|IFS|RANDOM|SECONDS|LINENO|_) continue;;
        esac
        __fish_probe_decl=$(builtin declare -p "$__fish_probe_name" 2>/dev/null)
        case $__fish_probe_decl in 'declare -a'*|'declare -A'*|'declare -n'*) continue;; esac
        builtin printf '%s\0%s\0' "$__fish_probe_name" "${!__fish_probe_name}" >&9
    done
    builtin printf '\0' >&9
done
"#;

pub struct Session {
    child: Child,
    socket: UnixStream,
}

#[derive(Debug)]
pub struct State {
    pub status: i32,
    pub cwd: String,
    pub scalars: BTreeMap<String, String>,
}

impl Session {
    pub fn new() -> io::Result<Self> {
        let (socket, peer) = UnixStream::pair()?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let fd = peer.as_raw_fd();
        let mut cmd = Command::new("/bin/bash");
        cmd.args(["--noprofile", "--norc", "-i", "-c", DRIVER])
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Only async-signal-safe operations run between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                for signal in [
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTSTP,
                    libc::SIGTTIN,
                    libc::SIGTTOU,
                    libc::SIGCHLD,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                if libc::dup2(fd, 9) < 0 || libc::fcntl(9, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        Ok(Self { child, socket })
    }

    fn field(&mut self, deadline: Instant) -> io::Result<String> {
        let mut bytes = vec![];
        loop {
            let mut byte = [0];
            match self.socket.read(&mut byte) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Bash exited")),
                Ok(_) if byte[0] == 0 => return String::from_utf8(bytes).map_err(io::Error::other),
                Ok(_) => bytes.push(byte[0]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    if Instant::now() >= deadline || self.child.try_wait()?.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Bash did not finish its control response",
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
            if bytes.len() > 1024 * 1024 {
                return Err(io::Error::other("oversized probe field"));
            }
        }
    }

    pub fn eval(&mut self, command: &str) -> io::Result<State> {
        if command.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in command",
            ));
        }
        self.socket.write_all(command.as_bytes())?;
        self.socket.write_all(&[0])?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = self.field(deadline)?.parse().map_err(io::Error::other)?;
        let cwd = self.field(deadline)?;
        let mut scalars = BTreeMap::new();
        loop {
            let name = self.field(deadline)?;
            if name.is_empty() {
                break;
            }
            if scalars.len() >= 4096 {
                return Err(io::Error::other("too many probe fields"));
            }
            scalars.insert(name, self.field(deadline)?);
        }
        Ok(State {
            status,
            cwd,
            scalars,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if self.child.kill().is_ok() {
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[crate::tests::prelude::serial]
    fn persistent_bash_probe() {
        let mut s = Session::new().unwrap();
        let first = s.eval("repo='a b'; greeting() { printf 'hello %s' \"$1\"; }; alias hi='greeting world'; false").unwrap();
        assert_eq!(first.status, 1);
        assert_eq!(first.scalars["repo"], "a b");
        let next = s.eval("previous=$?; result=$(hi); cd /; export exported='ü🐟'; count=0; for i in 1 2 3; do ((count+=i)); done").unwrap();
        assert_eq!(next.scalars["previous"], "1");
        assert_eq!(next.scalars["result"], "hello world");
        assert_eq!(next.scalars["count"], "6");
        assert_eq!(next.scalars["exported"], "ü🐟");
        assert_eq!(next.cwd, "/");
        let end = s
            .eval("unset repo; false | true; pipeline=${PIPESTATUS[*]}")
            .unwrap();
        assert!(!end.scalars.contains_key("repo"));
        assert_eq!(end.scalars["pipeline"], "1 0");
    }

    #[test]
    #[crate::tests::prelude::serial]
    fn probe_exposes_transport_commands_to_debug_traps() {
        let mut s = Session::new().unwrap();
        s.eval("observed=0; trap '((observed+=1))' DEBUG").unwrap();
        let state = s.eval("user_observed=$observed; trap - DEBUG").unwrap();
        let observed: usize = state.scalars["user_observed"].parse().unwrap();
        assert!(
            observed > 2,
            "transport unexpectedly became invisible: {observed}"
        );
        eprintln!("execution probe limitation: DEBUG trap observed {observed} internal commands");
    }
}
