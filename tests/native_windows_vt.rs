#![cfg(windows)]

// Exercise the exact decoder compiled into the Windows editor without pulling
// the vendored upstream crate's unrelated development dependencies into Fish.
mod keys {
    pub use rustyline::{KeyCode, KeyEvent, Modifiers};
}

#[path = "../vendor/rustyline/src/tty/windows_vt.rs"]
mod windows_vt;
