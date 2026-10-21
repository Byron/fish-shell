//! Owned process groups and anonymous pipes for the native execution host.

use std::fs::File;
use std::io;
use std::process::{Child, Command};

#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
pub use windows::request_interrupt_for_test;
#[cfg(windows)]
pub use windows::{Group, controls, install_controls, pipe, poll};
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Group, controls, install_controls, pipe, poll};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
pub enum Event {
    Exited(i32),
    Stopped(i32),
    Continued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
    Stop,
    Continue,
    Check,
}

#[derive(Clone, Copy, Default)]
pub struct Controls {
    pub interrupt: usize,
    pub stop: usize,
}

// Assert the common surface stays available on both backends.
const _: fn() -> io::Result<(File, File)> = pipe;
const _: fn(&mut Child) -> io::Result<Option<Event>> = poll;
const _: fn(&Group, &mut Command) = Group::configure;
