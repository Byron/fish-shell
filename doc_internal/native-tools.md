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

Both tools return an error status when their output pipe closes. Diagnostic
output is best effort if stderr is also closed; neither tool panics in that case.

## Portable bundles and installation

From the repository, run `just package target/dist/fish-native.zip`. The package
recipe requires an explicit archive destination. It first completes the release
build, prepares a ZIP containing all four executables, license information,
platform notes, an installer, and a SHA-256 manifest, and then preserves any
previous archive before publishing the replacement. `just` without a recipe
still lists recipes; `just release` still builds with the default Windows target.
To select another installed Rust Windows target, invoke
`build_tools/package-windows.ps1 -Destination PATH.zip -Target TARGET` directly.

Extract the ZIP to a temporary directory, then run:

```powershell
just install C:/temporary/fish-bundle C:/your/chosen/Fish
# The extracted bundle also contains a standalone installer:
pwsh -File C:/temporary/fish-bundle/install.ps1 -Bundle C:/temporary/fish-bundle -Destination C:/your/chosen/Fish
```

Choose an empty directory or a previous installation made by this installer.
All files are copied and their checksums verified before installed files are
moved. A failed replacement attempts to restore every previous file; successful
upgrades preserve the old bundle in `.previous-*` inside the installation.
Unrelated directories are refused, and obsolete managed files are retained in
the backup. The commands use locks to prevent simultaneous publication.

Add the chosen directory to PATH yourself when desired. The installer does not
modify PATH, file associations, shell registrations, configuration, or universal
variables. Startup assets, functions, and completions remain embedded in
`fish.exe`, so the portable bundle needs no separate share-directory layout.
This is a portable ZIP/install workflow; MSI packaging and registration as a
Windows login shell are not provided.
