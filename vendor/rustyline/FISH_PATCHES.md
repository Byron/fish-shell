# Native Windows input changes

This directory contains the published `rustyline` 14.0.0 crate from crates.io,
under its original MIT license. Fish selects this pinned copy only through its
`cfg(windows)` dependency. The directory is excluded from the Fish workspace;
the POSIX Fish reader and Rustyline's Unix backend are unchanged.
The upstream crate checksum is
`7803e8936da37efd9b6d4478277f4b2b9bb5cdb37a113e8d63222e58da647e63`.

Local changes:

- `src/tty/windows.rs` and `src/tty/windows_vt.rs` support VT keyboard input and
  bracketed paste while retaining Windows console events, clipboard insertion,
  resize handling and restoration of console modes.
- `src/edit.rs` keeps the cursor after pasted text in Windows vi insert mode.
  Vi normal-mode paste retains the upstream cursor behavior.

The pure decoder tests run through `tests/native_windows_vt.rs` in Fish. Live
ConPTY coverage uses the native release Fish executable and a private tmuxx
instance; see `tests/native_fish_feel_tmuxx.py` and
`tests/native_interactive_tmuxx.py`.

When updating this dependency, compare these files with the new upstream version
and re-run the native unit and ConPTY checks. The remaining upstream files are
kept intact to make rebasing the Windows changes straightforward.
