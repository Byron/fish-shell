#![cfg(windows)]

// Exercise the helper used by the native console renderer without importing
// the vendored crate's unrelated upstream development dependencies.
#[path = "../vendor/rustyline/src/tty/windows_redraw.rs"]
mod windows_redraw;
