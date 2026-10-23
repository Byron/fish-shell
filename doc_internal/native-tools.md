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
