# Native Windows tools

`fish_indent.exe` uses the same AST formatter and indentation logic as POSIX
Fish. It formats standard input or named files, including incomplete source,
comments, command substitutions, Unicode filenames, and CRLF input. Formatting
normalizes structural newlines to LF, while quoted content remains intact.
Single-line blocks without a final newline also receive indentation on the
first formatting pass; the shared fix applies on POSIX too.

The native command supports `--write`, `--check`, `--no-indent`, `--only-indent`,
`--only-unindent`, and `--dump-parse-tree`. `--check` returns the number of files
that need formatting and prints their names to stderr. `--write` only rewrites
changed files and reports read/write failures.

`--ansi`, `--html`, and `--pygments` reuse the native editor's syntax highlighter.
Highlighting never executes substitutions or probes command availability.
Its role classifications and default palette can differ from POSIX highlighting;
plain formatting is shared. Exported `fish_color_*` variables configure ANSI
colors. No startup scripts or universal variables are loaded by the formatter.

`fish_key_reader.exe` reads individual console keys and prints sourceable `bind`
commands. It shares the editor's Windows console/VT decoder and canonical binding
names, so arrows, modifiers, function keys, Unicode, and Escape work the same way
as at the prompt. It restores console modes when it exits, including after input
errors. The command requires console input; help and version queries also work
with redirected input.

Use `--continuous` to read more than one key. Type `exit` or `quit`, or press
Ctrl-C or Ctrl-D twice consecutively, to finish. `--verbose` reports the normalized
decoded key and elapsed time; the Windows console transport does not retain the
original input byte sequence. Bracketed paste is reported as data, and unsupported
key events are identified without suggesting an unusable binding.
