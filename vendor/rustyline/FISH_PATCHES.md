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
- `src/tty/windows.rs` measures prompt positions with the same ANSI-aware widths
  used when drawing. Colored multiline prompts, including Starship output, keep
  the cursor aligned without erasing preceding output during input redraws.
- `src/tty/windows.rs` and `src/tty/windows_redraw.rs` retain the previous
  rendered rows. Editing and completion navigation overwrite changed rows and
  erase only leftover text, preserving unchanged prompt, input and menu rows.
  Resizes, scrolling and unsupported terminal controls use a full redraw.
- `src/tty/windows.rs` queues external output without blocking the reader's
  completion callback. The reader drains queued messages without losing wakeups
  or blocking on stale notifications, then uses its normal redraw path.
- `src/tty/mod.rs` uses the reader's associated buffer type in `unbuffer`, so the
  Windows implementation also compiles in the crate's test configuration.
- `src/edit.rs` keeps the cursor after pasted text in Windows vi insert mode.
  Vi normal-mode paste retains the upstream cursor behavior.
- `src/keymap.rs` and `src/command.rs` provide a Windows-only command to replace
  an abbreviation and submit the corrected input with validation in one key
  press. Fish's native editor computes expansions and continuation indentation.
- `src/completion.rs`, `src/history.rs` and `src/sqlite_history.rs` spell out
  inferred return lifetimes so current Rust compilers build without lifetime
  syntax warnings. The borrowing behavior is unchanged.

The pure decoder and redraw tests run through `tests/native_windows_vt.rs` and
`tests/native_windows_redraw.rs` in Fish. Live
ConPTY coverage uses the native release Fish executable and a private tmuxx
instance; see `tests/native_fish_feel_tmuxx.py` and
`tests/native_interactive_tmuxx.py`. `tests/native_prompt_tmuxx.py` checks cursor
coordinates and preservation of preceding output with ANSI and Starship prompts.
It also inspects raw ConPTY output for unnecessary erasure or repainting while
typing and navigating completions.
`tests/native_completion_tmuxx.py` checks live completion callbacks, output,
empty callback input, cancellation, and retention of the shell's existing jobs.

When updating this dependency, compare these files with the new upstream version
and re-run the native unit and ConPTY checks. The remaining upstream files are
kept intact to make rebasing the Windows changes straightforward.
