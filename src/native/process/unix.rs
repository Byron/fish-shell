use super::{Controls, Event, Signal};
use std::fs::File;
use std::io;
use std::os::fd::FromRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command};

pub fn install_controls(_: bool) -> io::Result<()> {
    Ok(())
}
pub fn controls() -> Controls {
    Controls::default()
}

pub fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let files = unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    };
    for fd in descriptors {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(files)
}

pub fn poll(child: &mut Child) -> io::Result<Option<Event>> {
    let mut status = 0;
    let pid = unsafe {
        libc::waitpid(
            child.id() as i32,
            &mut status,
            libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
        )
    };
    if pid == 0 {
        return Ok(None);
    }
    if pid < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error);
    }
    if libc::WIFEXITED(status) {
        Ok(Some(Event::Exited(libc::WEXITSTATUS(status))))
    } else if libc::WIFSIGNALED(status) {
        Ok(Some(Event::Exited(128 + libc::WTERMSIG(status))))
    } else if libc::WIFSTOPPED(status) {
        Ok(Some(Event::Stopped(libc::WSTOPSIG(status))))
    } else {
        Ok(Some(Event::Continued))
    }
}

pub struct TerminalGuard {
    previous: Option<i32>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            unsafe {
                libc::tcsetpgrp(0, previous);
            }
        }
    }
}

pub struct Group {
    pid: Option<u32>,
}

impl Group {
    pub fn new(_: bool) -> io::Result<Self> {
        Ok(Self { pid: None })
    }
    pub fn configure(&self, command: &mut Command) {
        command.process_group(self.pid.unwrap_or(0) as i32);
    }
    pub fn add(&mut self, child: &Child) -> io::Result<()> {
        self.pid.get_or_insert(child.id());
        Ok(())
    }
    pub fn start(&mut self, _: &Child) -> io::Result<()> {
        Ok(())
    }
    pub fn signal(&mut self, signal: Signal) -> io::Result<()> {
        let signal = match signal {
            Signal::Interrupt => libc::SIGINT,
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
            Signal::Stop => libc::SIGSTOP,
            Signal::Continue => libc::SIGCONT,
            Signal::Check => 0,
        };
        if let Some(pid) = self.pid {
            if unsafe { libc::kill(-(pid as i32), signal) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    pub fn terminate(&mut self, _: i32) -> io::Result<()> {
        self.signal(Signal::Kill)
    }
    pub fn foreground(&self) -> io::Result<TerminalGuard> {
        let previous = unsafe { libc::tcgetpgrp(0) };
        if previous < 0 {
            return Ok(TerminalGuard { previous: None });
        }
        if let Some(pid) = self.pid {
            // The parent must be able to reclaim its terminal while technically
            // in a background process group after this handoff.
            unsafe {
                libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            }
            if unsafe { libc::tcsetpgrp(0, pid as i32) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(TerminalGuard {
            previous: Some(previous),
        })
    }
    pub fn disown(&mut self) -> io::Result<()> {
        Ok(())
    }
}
