# Native Windows build milestone

`fish` and `rash` build as native Windows executables with Cargo. MSYS2 remains
a separate POSIX target with the existing full Fish implementation. The native
build is an initial shell implementation, not yet a replacement for that full
interactive environment.

## Choose a target

Cargo's target determines the backend. There is no MSYS2 feature toggle and no
selection based on `MSYSTEM`, `PATH`, Git Bash, or the terminal that started Cargo.
The build script checks the target OS before running POSIX configuration probes.

| Rust target | Backend | Runtime |
| --- | --- | --- |
| `x86_64-pc-windows-msvc` | Native Windows Fish frontend and portable Rash host | Windows |
| `x86_64-pc-cygwin`, using the MSYS2 Rust toolchain | Existing POSIX Fish frontend | MSYS2 |
| Linux, macOS and other supported Unix targets | Existing POSIX Fish frontend | Target OS |

Other native Windows targets select the same backend, but the current build and
execution checks use x64 MSVC. Changing a Cargo feature cannot provide a different
target's ABI or runtime.

Install Rust through `rustup`, the Visual C++ Build Tools and the Windows SDK.
From PowerShell in this checkout:

```powershell
cargo build --locked --release --bins --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\release\fish.exe -N -c 'echo native Fish'
.\target\x86_64-pc-windows-msvc\release\rash.exe -c 'printf "%s\n" "native Rash"'
```

With [Just](https://just.systems/) and PowerShell 7 installed, use `just release`
to rebuild all four native release binaries. Bare `just` lists the available
recipes without starting a build. On Windows the release target defaults to
`x86_64-pc-windows-msvc`; direct PowerShell invocation can override it with
`./build_tools/build-release.ps1 -Target <target-triple>`.

On Windows, Cargo first builds in `.release-build` under its configured target
directory, with any separate Cargo build directory redirected into that cache.
Existing published binaries stay in place throughout compilation. Only after
every binary is built and the complete bundle is prepared does the script rename
the old executables and PDBs to names such as
`fish.previous-<UTC timestamp>-<GUID>.exe` and publish the replacements. Running
shells can keep using the preserved files. Failed builds leave the current bundle
untouched; publication failures attempt to restore it and report any files that
could not be restored. Replacement of multiple files is not atomic.

Published binaries remain in `<target-directory>/<target>/release`. Earlier
backups are kept, and the separate build cache is reused on subsequent runs.
On Unix, the recipe uses the current Cargo toolchain and target configuration
with its ordinary release output paths. `python tests/build_release.py` checks
Windows publication and rollback with isolated fake Cargo artifacts.

With an MSVC host toolchain, `cargo build --locked --release --bins` selects the
native backend automatically and writes optimized executables to `target/release`.
`fish_indent` shares the POSIX formatter and supports native file, stdin,
check/write, and highlighting modes. `fish_key_reader` shares the native editor's
key decoder and prints sourceable bindings. See [native tools](native-tools.md)
for their options and current limitations.

`just package target/dist/fish-native.zip` builds and publishes a portable ZIP
with all four executables, documentation, licenses, and a checksum manifest.
`just install EXTRACTED_BUNDLE DESTINATION` checks and stages an extracted bundle
before replacing a previous managed installation, preserving its files for
rollback. Neither command changes PATH or Fish configuration. CMake installation
still uses the POSIX build; native MSI packaging is not provided.

For the full MSYS2 build, open an **MSYS2 MSYS** shell, update it with `pacman -Syu`,
then use its own Rust toolchain. Git Bash alone does not include that toolchain.

```sh
pacman -S --needed rust gcc git
/usr/bin/rustc -vV  # host: x86_64-pc-cygwin
/usr/bin/cargo build --locked --release --bins --target-dir target/msys2
./target/msys2/release/fish.exe -N -c 'echo (math 1 + 1)'
```

Keep the MSYS2 runtime DLLs and utilities available when launching that executable.
The separate output directories let native and MSYS2 builds coexist. Building
either target does not replace an installed shell or change terminal settings.

Shared Fish startup files can use `status is-native-windows` to select native
configuration. It silently returns status 0 for the native Windows backend and
status 1 for POSIX builds, including MSYS2/Cygwin and WSL. The result is independent
of the terminal, parent shell and Fish/Bash input mode; no subprocess or version
string parsing is needed.

## Native command and script execution

Native `fish` uses the existing Fish AST and tokenizer with a native evaluator.
Ordinary `fish -c`, script files and the prompt evaluate Fish syntax.
`fish -n` checks Fish syntax without executing it. Unsupported execution features
produce diagnostics.

The current Fish subset includes simple commands and standard redirects on
descriptors 0, 1 and 2; list variables, quoting and simple integer indices;
`if`/`else`, `begin`, `for`, `while`, `switch`, functions, `not`, and logical
chains. Initial builtins include `echo`, `set`, `cd`, `pwd`, `count`, `test`, `[`,
`true`, `false`, `source`, `eval`, `exit`, `return`, `break` and `continue`.
`echo` shares POSIX option parsing and escape rendering, including grouped flags,
`-s`, `--` and literal byte escapes. Shared completion helpers therefore handle
cached command names and option-looking arguments consistently.
Native Fish also provides `printf`, `math`, `contains`, `read`, `type`, command
queries, function queries/removal, and common `status` queries. `math` uses the
same arithmetic parser and functions as POSIX Fish. `random` shares POSIX seed,
range, step and choice handling. Seeded state follows the native session into
functions, command substitutions and pipeline workers; final foreground stages
return their updated state to the caller. `count` adds newline-terminated records
from directly piped or redirected stdin to its argument count; an unterminated
final record does not count. `string` supports literal
split, join, replace, trim, case conversion, Unicode length/substrings, repeat,
escaping, and wildcard matching, with arguments or redirected standard input.
Supported string options accept compact forms such as `string sub -s1 -l16`,
grouped flags, and `--option=value`. String operations read stdin only when it
is directly piped or redirected to the builtin; an empty argument list does not
consume input inherited by a function or block. Use `<&0` for an explicit read
inside such a block.
`string pad`, `string shorten`, and `string length --visible` share terminal
escape recognition, Unicode width measurement, and display transforms with
POSIX Fish. They honor session-local `fish_emoji_width` and
`fish_ambiguous_width`, preserve ANSI sequences, and support the same alignment,
ellipsis, multiline, and quiet options. Buffered native display input is limited
to 4,194,304 Unicode characters and 100,000 records; output is limited to 16 MiB.
Left shortening and unfinished escape scanning take linear time. Both builds
clip backspaces after ignoring escape sequences, avoiding negative-width errors
on colored text with excess backspaces.
`string split --fields` selects fields, including repeated fields and forward or
reverse ranges; `--allow-empty` skips missing fields. Native selections are
bounded to 100,000 fields. `string sub --end` accepts positive or negative Unicode
character positions. Both operations share their field/range rules with POSIX
Fish, and `--end` is mutually exclusive with `--length`.
Command substitutions, brace lists, indirect variables, and list index ranges
are supported. Comments inside substitutions can contain quotes and parentheses;
nested quoted substitutions use the same boundaries as POSIX Fish. Unquoted
native command substitutions split Windows CRLF output
into lines; quoted substitutions retain their internal line endings.
`string split` preserves empty fields and embedded newlines inside command
substitutions, including when it runs as the final pipeline stage. Captured
fields use the same separated-output buffer as POSIX Fish; ordinary redirected
output still renders the usual line separators. Native captures are bounded to
16 MiB and 100,000 fields or worker output records. Cancelled or stopped captures
are discarded; resuming a stopped pipeline does not restore its original
substitution result.
`string collect` retains whole input records, with `--allow-empty` and
`--no-trim-newlines`; `string split0` splits NUL-delimited input while preserving
embedded newlines. `string join0` writes ordinary NUL-separated bytes, including
a terminal NUL, for pipelines and files. Substitution slices select fields before
quoted results are joined, so `"$(printf 'a\nb\n')[2]"` expands to `b`.
Native filesystem globbing expands unquoted `*` and recursive
`**` directory components. Quoted stars and stars from variable values stay
literal. Unmatched globs are empty lists for `set`, `count`, and `for`, and produce
status 124 for ordinary command arguments. Command substitutions retain Fish's
newline splitting and variable side effects; quoted substitutions preserve
internal newlines. Variables created with `set -l` inside a substitution disappear
when it finishes; updates to existing caller variables and globals persist.
`switch` expands case arguments and selects the first matching
case; quote wildcard patterns, for example `case '*.txt'`. Blocks support standard
redirections, with a shared file position across their commands: `while read line;
echo $line; end < input.txt > output.txt` reads and writes each line in order.
Nested redirections are restored when a block finishes, including `return` and
`break`. Regular-expression `string match` and `string replace` use the same
UTF-32 PCRE2 implementation as POSIX Fish, including capture groups, character
indices, repeated matches and replacement references. Named match groups set
session variables. String escaping is also shared, including
`string escape --style=regex` for completion helpers.

Native `read` supports grouped options, lists, delimiters, NUL records (`-z`),
one line per variable (`-L`), Unicode character limits (`-n`), and Fish tokenization
(`-t`/`--tokenize-raw`). Tokenization preserves literal variable and command
substitutions without executing them. Field splitting shares its implementation
with POSIX Fish. Records are bounded to 16 MiB; reads preserve subsequent input,
and failures clear remaining destination variables. Bare `read` writes the
record without adding a delimiter. Normal line reads recognize Windows CRLF;
NUL reads preserve embedded newlines and carriage returns.

`set --function` (`-f`) and `read --function` write to the enclosing function
scope, including inside blocks, sourced scripts and command substitutions.
Functions declared with `--no-scope-shadowing` share their caller's function
scope. At top level, `-f` selects the outermost local scope. Scoped queries,
erasure, slices, export flags and name listing use that same destination;
append/prepend still seed their list from the currently visible value.

The full builtin set is not implemented. Edited reads, silent input,
and locale-specific numeric formatting remain unavailable;
unsupported options produce diagnostics.

Native builds enable `native-utilities` by default, packaging 35 commands such
as `awk`, `sed`, `grep`, `find`, `sort`, `seq`, `uname`, and common file/text tools.
They run in isolated child processes and are available at the prompt and in
scripts when ordinary command lookup finds no executable. Functions and aliases
keep their priority; programs selected through `PATH` are never retried as
packaged utilities after an error. Explicit executable paths use only that path.
`command -q` and `type -q` recognize packaged utilities; `command -s` and `type -p`
print real executable paths only. `type -t` reports `utility` for a fallback.
Command completion and highlighting also recognize packaged utilities when PATH
is empty. Bare executable completions follow PATH; files in the current directory
need an explicit `./` unless PATH includes that directory. An empty PATH list
searches no directories, while an empty element explicitly includes the current
directory.
Executable symlinks on PATH, including Rustup's Cargo links, also participate in
command discovery and completion autoloading.
Disable the feature with `--no-default-features` and explicitly select the other
desired Cargo features.

Windows' own `sort.exe` and `find.exe` remain ordinary PATH commands. Bundled
helpers can explicitly request Unix-compatible behavior using `__fish_sort` and
`__fish_find`; Rustup completions use these adapters. Programs launched inside
utilities, such as `xargs seq` or `find -exec`, still require executable files on
PATH. AWK supports the common text-processing forms used by helpers, with known
upstream limitations including `exit` skipping `END` blocks and subprocess forms
requiring `sh`. See [the concrete utility follow-ups](native-utilities-follow-ups.md)
for tested dependencies and remaining integration limits.
Completion helpers prefer `python3` and versioned Python 3 commands, then accept
an installed `python` after verifying Python 3.5 or newer in isolated mode.
This supports Cargo package and feature suggestions with standard Windows
`python.exe` installations; Python remains an external dependency.
`argparse` uses the same option parser as POSIX Fish, including short and long
options, repeated and optional arguments, mutually exclusive options, argument
counts, and validation commands. It writes local `_flag_*`, `argv_opts`, and
remaining `argv` values for the caller.

Native `path` supports basename/dirname, normalization/resolution, extensions,
filtering, modification times and natural sorting. Lexical operations, extension
rules and the natural comparator share their core with POSIX Fish, with Windows
drive and UNC roots handled separately. `path filter`/`path is` support file,
directory, link and character-device checks, Windows read/write/execute access
probes, inversion and `--all`. POSIX ownership and special mode-bit filters report
unsupported options; block devices, FIFOs and Unix sockets have no matching
ordinary Windows file type. `path sort` supports reverse/unique output and
basename/dirname keys, with a 16 MiB or 100,000-path input limit.
Path results retain separate command-substitution fields, including empty
extensions, even alongside ordinary output or across a final pipeline worker.
Redirects and intermediate pipeline stages use ordinary newline-delimited bytes;
`path -Z` emits NUL-delimited bytes and composes with `string split0`.
`builtin realpath -s` makes an absolute path without resolving links in its
argument. Filesystem resolution uses Windows semantics; special device
namespaces remain outside this native subset.

`test` and `[` share the POSIX expression parser, including parentheses, boolean
combinations and precise integer comparisons. File and terminal predicates use
Windows metadata, access rights and stream handles. Unix ownership and special
permission bits remain platform limitations.

`source FILE [ARGS...]`, `source - [ARGS...]`, and piped `source` load Fish code
into the current session. Source-local variables and `argv` disappear afterward;
functions and global changes remain. Input is read and parsed before evaluation.
`status filename` reports the current script, sourced file, or function's
definition file, including after autoloading and pipeline execution. It reports
`-` for sourced stdin and `Standard input` for commands entered without a file.
`status basename` and `status dirname` expose the same source location;
`status function` and `--level` inspect native function frames. `status features`
and `status test-feature` share the feature catalog and report the native
backend's actual behavior. Native wildcard, `%self`, and terminal policies are
fixed; the POSIX-only terminal query feature is reported off.
`status build-info`, `status list-files`, and `status get-file` share build and
embedded-file metadata with POSIX, allowing bundled helpers to discover their
packaged function sources. Native builds do not embed POSIX manpages.
`status is-command-substitution` follows real substitution nesting, including
pipeline workers, independently of prompt and completion output capture.
Unknown queries return status 2; unavailable reader, line-number, stack, terminal,
locale and POSIX job-control queries return status 1 with a diagnostic. They do
not abort the remaining script. Interactive current-command tracking remains
unavailable; noninteractive `status current-command` reports `fish` as on POSIX.

Function declarations use the same option parser as POSIX Fish. Native functions
support `--description`/`-d`, `--wraps`/`-w`, `--argument-names`/`-a`,
`--inherit-variable`/`-V`, and `--no-scope-shadowing`/`-S`. Captured values are
snapshots; named arguments and scope boundaries follow Fish semantics. Printed
definitions preserve these properties.

The bundled `alias` helper creates wrappers in the current session, including
self-named wrappers around builtins or external commands. Native `read --list`
and `builtin --names` provide its list parsing and recursion checks. Saved alias
function files load normally. The bundled `alias --save` and `funcsave` helpers
save definitions through `functions --no-details` into the selected
configuration's functions directory, or `funcsave --directory`. Descriptions,
wrapper completions, argument names and captured values survive a fresh session.
Bare `alias` listing can still load POSIX-only helpers.

`functions --copy OLD NEW` copies a function for wrappers such as a customized
`fish_prompt`. The copy retains its description, named arguments, captured values
and scope behavior. As in POSIX Fish, event handlers and completion wrappers are
not copied, and an existing destination is not overwritten. `status filename`
inside the copy reports the file where it was copied.

Functions can subscribe to `--on-variable` changes or named `--on-event` events.
`emit EVENT ARGS...` invokes named handlers with the supplied arguments. Variable
handlers receive `VARIABLE SET NAME` or `VARIABLE ERASE NAME`; they run after
assignments, including `cd` updates to `PWD`. `set --no-event` suppresses handlers
for that change. Handlers preserve the triggering command's status, execute in
registration order, and retain their registrations in pipeline workers and printed
function definitions. Handler output bypasses redirections on the triggering
command; a handler in a pipeline worker uses that worker's inherited stdout.
The native reader emits `fish_prompt`, `fish_preexec`, `fish_postexec`, and
`fish_cancel`. Pre/post handlers receive the accepted command line, including
internal newlines, once per submission. `fish_exit` runs once at shell shutdown;
script and `-c` execution supply the final status as its argument. Signal and
individual process/job exit subscriptions remain unsupported. An untaken branch
can guard a POSIX-only declaration with `status is-native-windows`.

`set --universal` (`-U`) stores variables across native sessions, including export
and path-variable flags. Local and global variables shadow universal values;
an unscoped assignment updates an existing universal variable when no closer
scope defines it. `set --path` and `--unpath` control whether a variable splits
and exports using Windows' semicolon path separator.

Normal native startup installs the same `fish_user_paths` handler as POSIX Fish,
using Windows' semicolon separator. User paths are prepended to `PATH` without
duplicates, and removing them preserves entries that were already in `PATH`.
The bundled `fish_add_path` works with global or universal user paths, direct
`PATH` updates, append/prepend, move, dry-run and verbose options. Native `_`
prints the original message text; it does not load POSIX localization support.
This portable setup runs before user snippets and is skipped with `--no-config`.

Indexed `set` assignments, queries and erasures use the same index parser as
POSIX Fish, including negative indexes and ranges. For example, `set -e words[1]`
removes the first element and `set -q words[-1]` checks for a last element.
Universal-variable indexes resolve against the latest stored list while locked.
Index expansion is limited to one million positions; native indexed assignments
also limit the highest position to that value.

Native universal variables use `fish/native_fish_variables` under the selected
configuration home. Their encoding is shared with POSIX Fish, but their file and
locking are separate; POSIX `fish_variables` is not imported automatically.
Each change merges with the latest stored values while locked, then replaces the
file atomically. Other sessions' changes become visible at command and prompt
boundaries, with variable events delivered after unlocking. Idle shells do not
receive push notifications. Startup storage errors are diagnosed without blocking
configuration; failed writes leave the file and session values intact. Malformed
files are diagnosed and preserved. Files from an unknown future format can be
read but are not overwritten.

With `-N`/`--no-config`, universal files are neither read nor written. As in POSIX
Fish, `set -U` then creates a global variable; explicit universal queries and
erasures still address only universal scope.

Use `rash`, `fish --rash-shell`, or `fish --script-language=bash` for Bash syntax:

```powershell
.\target\x86_64-pc-windows-msvc\release\rash.exe -c 'x=world; printf "hello %s\n" "$x"'
.\target\x86_64-pc-windows-msvc\release\fish.exe --rash-shell -c 'printf "%s\n" "$PWD"'
.\target\x86_64-pc-windows-msvc\release\fish.exe --script-language=bash .\script.sh argument
```

The standalone `rash` executable uses the portable host on Unix as well as
Windows. It does not launch an installed Bash. Bash startup files are opt-in with
`--bash-startup`; add `--login` to choose login profiles. This differs from the
POSIX Fish `rash` function, which enables Bash startup by default and reuses
Fish's full interactive UI.

Native shells launch external programs using `std::process::Command` and separate
arguments. Paths and process arguments use Unicode: the Rash OS boundary accepts
UTF-8 and rejects unrepresentable values rather than replacing characters.
Windows `PATH` uses semicolons, inherited `Path` is imported as `PATH`, and command
lookup supports the `.exe` suffix. Prefer quoted Windows paths or forward slashes
in shell source. Rash source and internal values remain byte-oriented.
Native redirections recognize `/dev/null` as the Windows `NUL` device, so shared
startup probes such as `status is-native-windows 2>/dev/null` work unchanged.

With a terminal and no command or script, the native Windows frontend offers
interactive line editing and history, described below. The portable process host
supports pipelines and owned jobs as described below.
Signal traps and `exec` replacement remain unavailable. Rash's other language limits
remain documented in [rash-compatibility.md](rash-compatibility.md).

## Interactive editing and startup

Native Windows Fish and Rash use a Unicode line editor that works through ConPTY.
Left/Right, Home/End, Backspace and Delete edit the current input; Up/Down recall
history. Tab completes filenames and command names from builtins, the session's
functions, packaged utilities and its current `PATH`. Completion quotes spaces
and shell punctuation in filenames without evaluating shell source. Input can
span multiple lines;
completed multiline submissions are stored as one history entry. Ctrl-C cancels
the current edit and returns to the prompt. Ctrl-Z at the prompt does not suspend
the shell itself.

Fish path completion expands directory expressions such as `$project/src/`,
`$projects[-1]/` and `~/` using the current session's variables. Lists, indirection
and quoted variables follow the evaluator's expansion rules; inserted completions
retain the typed variable expressions and quote literal filename characters.
This also works in redirection targets and respects completion file policies.
Typed command substitutions and wildcard directory expressions are refused.
Variable expansion in the filename being completed, and Bash variable paths,
remain unavailable. Completion expansion limits protect the editor from large
Cartesian products and repeated slices.

Native Fish suggests the remainder of a matching history entry in gray as you
type. Right or Ctrl-F accepts the suggestion at the end of the input;
Alt-Right or Alt-F accepts one word. Ctrl-E accepts the rest of the suggested
line when the cursor is at the end of the input, stopping at a newline. Otherwise,
it moves to the current line's end. Accepting a suggestion never executes it;
Enter executes only the text you have accepted. Up and Down search history for
the text you typed (including matches inside a command),
skip repeated matches, and restore your draft after the newest match. Ctrl-R
opens incremental history search. In a multiline edit the arrows move between
lines before searching history at the top or bottom.

Unfinished Fish blocks, quotes and pipelines stay in one editable buffer after
Enter, so you can return to earlier lines before submitting the whole command.
Alt-Enter inserts a newline explicitly. Standard editing shortcuts include
Ctrl-A/Ctrl-E, Alt-B/Alt-F, Ctrl-W, Ctrl-U/Ctrl-K, Ctrl-Y and Ctrl-_ for undo.
Continuation lines are indented using the shared Fish grammar, including nested
blocks and substitutions. Quoted contents and backslash continuations receive
only the requested newline, preserving their literal contents. Existing lines
are not automatically reformatted when `end` or `else` is typed. Ctrl-V and
Shift-Insert insert clipboard text as one edit.

Terminals that support bracketed paste, including Rustty/tmuxx, can paste a
multiline command as one editable, undoable insertion. Pasted newlines do not
execute commands; Enter submits the completed buffer. Paste preserves Unicode,
tabs and quoted text, normalizing Windows line endings to newlines. Key bindings
and abbreviation expansion do not run on pasted characters. Bracketed paste is
enabled while reading input and disabled before launching a command. Terminals
that send unbracketed keystrokes still need clipboard insertion for atomic paste.
Escape uses Fish's 30 ms delay to distinguish it from Alt-prefixed keys;
`fish_escape_delay_ms` accepts values from 10 through 4999 (invalid values use
the default).

Use `fish_vi_key_bindings` to select vi editing, or
`fish_default_key_bindings` to return to the default mode. Setting
`fish_key_bindings` to either name in configuration has the same effect. Basic
vi insert/normal-mode editing uses the native line editor; custom Fish keymap
functions and mode-specific bindings are not implemented.

Abbreviations can be defined with `abbr --add`, listed with
`abbr --show`/`--list`, queried with `--query`, and removed with `--erase`:

```fish
abbr --add gs 'git status'
bind ctrl-t complete
```

Space expands a matching bare word while editing. Enter expands and executes
complete input in one press; unfinished expansions continue in the editable
buffer with normal indentation. The shared Fish matcher supports `--regex`,
`--function`, `--position anywhere`, `--command`, and `--set-cursor`, including
bare words inside command substitutions. The default position is still a command
word. Quoted words, redirection targets and noninteractive scripts are not
expanded. Definitions belong to the session; put them in `config.fish` to
recreate them.

Native `bind` maps one key to input actions or Fish script callbacks.
`bind --function-names`
lists actions and `bind --key-names` lists named keys; `bind --query KEY` checks a
binding. Modern names such as `ctrl-t`, `alt-f`, `left` and `f5`, Unicode single
keys and legacy control-character spellings are accepted. `bind --erase KEY`
restores the previous default, and `bind --erase --all` removes all user overrides.
Binding lists can be sourced as Fish code. Multiple callbacks run in order before
queued input actions; callbacks can edit the buffer through `commandline` and
queue input actions through `commandline --function`. Native Fish autoloads and calls
`fish_user_key_bindings` before the first read. Multikey sequences and custom
binding modes remain unsupported. An open completion menu handles its navigation
and cancellation before user bindings. See
[native reader customization](native-reader-customization.md) for supported
editing operations and generated abbreviations.

The native Fish editor colors commands, keywords, quoted strings, variables,
escapes, operators and comments as you type, and marks unknown literal commands
and malformed escapes in red. Matching delimiters are emphasized at the cursor.
Coloring uses the shared Fish grammar without executing input or substitutions.
Customize it with `fish_color_normal`, `fish_color_command`, `fish_color_param`,
`fish_color_option`, `fish_color_keyword`, `fish_color_quote`, `fish_color_escape`,
`fish_color_operator`, `fish_color_redirection`, `fish_color_end`,
`fish_color_comment`, `fish_color_error` and `fish_color_autosuggestion`.
Colors accept names, bright names, RGB hex values and attributes such as `--bold`
and `--underline`; background colors use `--background COLOR`. Invalid settings
fall back to defaults. `set -g fish_autosuggestion_enabled 0` disables suggestions.

The native `set_color` builtin supports those colors and attributes for prompts:

```fish
function fish_prompt
    set_color green
    printf '%s> ' $PWD
    set_color normal
end
```

Completion understands command positions after pipes, logical operators and
command substitutions. Native Fish offers directory-only `cd` completion,
session variable names after `$`, common builtin options, and Git/Cargo
subcommands and options. Matching prefers prefixes, then case-insensitive
prefixes, then substring/subsequence matches. Executable names normally omit
`.exe`; a typed suffix is retained. Descriptions accompany completion choices.
Directories remain open for further completion, while finished arguments gain
a separating space. Completion never executes the text being edited.

Native Fish autoloads the bundled `git.fish` and `cargo.fish` scripts, including
conditions, aliases, repository queries, and project-specific generators. Git
branch and option completion uses the same helpers as POSIX Fish. Cargo package,
feature and target suggestions query the installed Cargo and Python tools.
Saved aliases inherit these completions on their first query. An editor provider
with a smaller static option and Git-reference set remains available when no
script registrations apply; it does not limit the normally autoloaded scripts.

The default-enabled `gix` Cargo feature enables the underlying `git` feature,
which controls repository integration on both Windows and POSIX and the Git
revision/dirty suffix in Cargo build version metadata. Both evaluators expose the
same internal `__fish_git` query builtin, sharing status, history, configuration,
repository routing and regex handling. Completion and prompt scripts on both
backends use this builtin through `__fish_git_query`. The shared shell helper
uses an installed Git executable for queries without a suitable API: installed
help, tool and configuration
catalogs, and queries with nonempty `GIT_COMMON_DIR`, `GIT_CONFIG_PARAMETERS`,
`GIT_OBJECT_DIRECTORY` or `GIT_ALTERNATE_OBJECT_DIRECTORIES` overrides. Errors
from gix and unsupported repository storage do not trigger a retry. Status uses
gix's normalization APIs, including configured filters. Gix may invoke Git to
discover its Windows shell environment when a filter needs one.
Build with `--no-default-features` to disable gix integration, adding
`--features embed-manpages,localize-messages` to retain the other defaults if
desired. Such builds report the package version and use Git for queries through
the shared helper when it is installed. Without either provider, static options
and ordinary file completion remain available, but repository queries cannot
supply suggestions.
CMake builds enable the same gix integration by default; `-DWITH_GIT=OFF`
selects the Git executable for queries through the shared helper.

Native configuration lookup uses the session's exported environment, matching
Windows variable names without case sensitivity. `HOME` (or `USERPROFILE`) selects
user Git configuration; `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` select explicit
files. Native queries do not discover an installed Git distribution to locate its
system configuration. Gix follows `main`, with the tested revision retained in
`Cargo.lock`. Concrete library limitations and upstream follow-ups are recorded
in [gix-follow-ups.md](gix-follow-ups.md).

Register options and choices in `config.fish` or at the prompt:

```fish
complete -c mytool -f -a 'build test'
complete -c mytool -l mode -r -a 'fast thorough' -d 'Select checking mode'
complete -c mytool -n '__fish_seen_subcommand_from build' -l release
```

Native `complete` shares Fish's option parser and argument-list syntax checking.
It supports command names (`-c` or positional), executable path patterns (`-p`),
short/long/old option aliases (`-s`, `-l`, `-o`), arguments and descriptions
(`-a`, `-d`), conditions (`-n`), wrappers (`-w`), erasure (`-e`), parameter
requirements (`-r`, `-x`), file policies (`-f`, `-F`) and source ordering (`-k`).
Repeated aliases, attached arguments, grouped short options and abbreviated long
options work as on POSIX Fish. File policies apply to the matching argument;
`--keep-order` preserves choices within each registration, newest groups first.
Script registrations replace the editor's built-in Git/Cargo suggestions.
`complete` lists registrations as reusable Fish source, optionally filtered by
command and colored with `--color`. `complete -C 'mytool --m'` prints choices with
tab-separated descriptions; `--escape` produces shell-escaped choices.
Registrations belong to the current session and can be recreated by configuration.
Conditions and argument sources are evaluated on demand for both Tab and
`complete -C`. Repeated conditions must all succeed; identical conditions are
cached within one query. Argument sources can expand variables or run registered
command substitutions. Their changes to the live session persist. Merely typing a
command substitution in the input being completed never executes it.

Read-only `commandline` queries use the same command-substitution, job, process
and token boundaries as POSIX Fish. Completion helpers can use `commandline -opc`
or `commandline -pxc`, as well as buffer, cursor, line and column queries.
`--input` supplies explicit text for a query. Reader-editing operations remain
unsupported.

Functions defined with `--wraps` inherit target completion, including
fixed arguments such as `--wraps='git checkout'`. Multiple targets and nested
wrappers are supported with bounded, cycle-safe traversal. Wrapper targets never
execute shell code. A saved wrapper's metadata becomes available when its function
is loaded. The first completion query loads eligible function and completion
files, then follows newly discovered wrappers so saved aliases work immediately.
User `fish/completions` files under the configured native home take precedence
over vendor directories and embedded completions. Generated completions come
after embedded ones. Set `fish_complete_path` to configure the disk search path;
autoloading is also available with `-N`. Unused files are not evaluated.
Vendor paths use `XDG_DATA_HOME` and explicitly configured `XDG_DATA_DIRS`;
generated completions use `XDG_CACHE_HOME`. Native data/cache defaults use
`LOCALAPPDATA`, with `~/.local/share` and `~/.cache` as final fallbacks.
The corresponding `__fish_*_dir` variables are available to shared helpers.

The loader shares file identity checks, caching and recursion protection with
POSIX Fish. Changing completion paths invalidates registrations for loaded
commands; explicit whole-command erasure can suppress automatic loading.
Pipeline workers retain loader state. Script execution uses the same protected
input and status handling as completion generators. Dynamic suggestions still
depend on the evaluator features and programs required by each script.

The isolated corpus audit loads every bundled completion script with an empty
PATH, private configuration/data/cache directories, and packaged utilities:

```powershell
python tests/native_completion_audit.py target/debug/fish.exe target/native-completion-audit.json
```

The current 1,099-script corpus has 1,023 scripts registering completions, 59
loading successfully without registrations, and 17 returning nonzero when their
target programs are absent. Some successful scripts also report missing optional
programs. These counts measure source loading, not every dynamic completion
query. Scripts that ask Docker, Pip, Ruff, or another application to generate
completions still need that application. POSIX service databases, FIFOs and
process/job-exit handlers remain platform or evaluator limits.
Installed-tool checks separately exercise Git branches/options, Cargo
packages/features/targets, Rustup lists, and saved alias wrappers.
The [shared behavior and dynamic completion tests](native-parity-tests.md) run
the same fixtures against native and POSIX Fish, with additional native minimum
Rust and disabled-default-feature CI lanes.

Completion evaluation receives empty standard input by default. During Tab,
Ctrl-C interrupts running generators before returning to the edited line.
Incidental output is buffered and displayed through the editor's redraw path.
Output from background jobs started by a callback may wait until the next
completion or submission.

When multiple completions match, Tab opens a searchable menu under the input.
Type to filter choices; Tab/Shift-Tab, arrows or Ctrl-N/Ctrl-P select, Page Up/Down
move by a page, and Backspace edits the filter. Ctrl-N selects the next match and
Ctrl-P the previous one, wrapping at either end of the filtered list. Outside
the menu, Ctrl-N/Ctrl-P retain their next/previous history behavior.
Enter inserts the selected completion without executing the command.
Escape or Ctrl-G cancels and preserves the
original text and cursor. Selection stays in the menu until accepted. Menu
rows are never command text or autosuggestions, and Unicode widths and terminal
resizing use the same renderer as the editable input.

The native renderer retains unchanged screen rows while editing. Typing updates
the affected input rows; completion navigation updates rows whose selection or
contents changed. Shortening input or dismissing a menu erases leftover text
without first blanking the prompt and the rest of the menu. Resizing, scrolling,
or prompt controls the renderer cannot safely compare can require a full redraw.

Native Fish reads `fish/conf.d/*.fish` followed by `fish/config.fish` under
`FISH_NATIVE_CONFIG_HOME` when it is
an absolute Windows path. This optional override is only used by the native
frontend: a launcher can select its Windows configuration without changing
`XDG_CONFIG_HOME` for MSYS Fish or other child programs. For example, set it to
`C:/Users/name/.config` to share a backend-aware loader with MSYS Fish's normal
home configuration. MSYS must use its own POSIX path spelling for XDG overrides;
its wildcard startup scanning does not support Windows drive paths.

Without the native override, Fish uses `XDG_CONFIG_HOME`, or `%APPDATA%`
when that variable is not set. Without either location it uses
`~/.config/fish/config.fish`. User snippets run in filename order, each with its
own source-local variables. Errors identify the file and do not prevent later
startup files from running. `-N`/`--no-config` skips both snippets and config.
The configuration
runs in the native evaluator and must use its supported Fish syntax and builtins.
Native Fish calls a defined `fish_prompt` function before each new command and
uses its output as the prompt, preserving the previous command's `$status` and
`$pipestatus`. ANSI colors in multiline prompts, including native Starship output,
do not contribute to cursor columns or wrapped rows, so editing preserves output
above the prompt. A user `fish_prompt.fish` can also be autoloaded; without a user
prompt it displays `fish>`. A defined or autoloaded `fish_right_prompt` appears
on the last prompt row when it fits beside the input. With
`set -g fish_transient_prompt 1`, accepting a command renders both prompt
functions once more with `--final-rendering`. Right prompts support single-line
text and SGR colors; unsupported terminal controls are suppressed.

Native sessions provide read-only `version` and `FISH_VERSION` variables.
`CMD_DURATION` starts at zero and records the elapsed milliseconds of the last
submitted interactive command. `COLUMNS` and `LINES` are refreshed from the
console before a fresh prompt. `fish -i -c COMMAND` and `fish -i SCRIPT` run once
with interactive configuration guards enabled, without opening the line editor.

Starship's generated Fish prompt can be initialized with:

```fish
starship init fish | source
```

The default wrapper uses the supported file-backed `psub` implementation.
Files live through the consuming command and are cleaned up afterward, including
for background consumers and owned-job cancellation. See
[native process substitution](native-process-substitution.md) for temporary
directory selection and remaining FIFO and job-event limits. Reader customization
is documented in [native reader customization](native-reader-customization.md).

Function lookup, `functions -q`, and `type` load requested `.fish` files from
`$fish_function_path`, which defaults to the selected configuration's `functions`
directory. Discovery, cached file identities, embedded sources and function-name
enumeration are shared with POSIX Fish. Unused files are not evaluated; explicit
definitions take precedence, erased functions stay erased, and changing the path
invalidates autoloaded definitions. `functions -n` and command completion list
available names without loading their files. Autoloading remains available with
`-N`. Saved alias functions in either `conf.d` or `functions` keep their descriptions,
wrapped commands and argument forwarding. Native builtin implementations take
precedence over bundled POSIX helper shims, while user function files can override
them. Other bundled functions still require the native evaluator to support their
bodies; loading the standard `alias` helper does not provide full builtin parity.

POSIX system initialization, system/vendor snippets, and vendor configuration are
not run by the native frontend. Native Fish reports cwd with OSC 7
and accepted-command lifecycle/status with OSC 133. Rustty/tmuxx can track cwd,
command text and exit status without an MSYS wrapper or injected startup script.
Suggestions and canceled edits are not reported as executed commands. Metadata
is omitted for redirected output and `TERM=dumb`.
Rash displays `rash>` and uses its separate Bash startup files with
`--bash-startup`; `-N` also skips those files.

Native Fish supports the `history` builtin, including search, append, exact
deletion, save, merge, clear and clear-session:

```fish
history
history search --prefix git --max 20
history search --show-time='%Y-%m-%d %H:%M:%S ' --contains build
history delete --exact --case-sensitive 'echo obsolete'
history merge
```

Search is case-insensitive by default; `--case-sensitive` changes that.
`--prefix` and `--contains` accept Fish wildcard patterns, while `--exact`
matches complete commands. Results are newest first, deduplicated per search
term. `--max` limits results before `--reverse` reverses them; `--null` separates
multiline records with NUL. `--color=auto|always|never` controls syntax colors.
The current command stays pending until it finishes, so a search does not show
itself, and clearing history removes that pending command too. Changes appear
in recall and autosuggestions at the next prompt. The native builtin provides
exact case-sensitive deletion; the POSIX wrapper's interactive selection and
pager are not loaded.

Fish history is stored under `fish/native_fish_history.yaml` in `XDG_DATA_HOME`,
falling back to `%LOCALAPPDATA%`, then `~/.local/share`. `fish_history` chooses a
different name (`native_NAME_history.yaml`); an empty value keeps it in memory.
The record format and search algorithms are shared with POSIX Fish, with native
file locking and atomic replacement to preserve concurrent sessions' changes.
`history merge` imports other sessions' saved entries. `clear-session` removes
only the current session's contributions since startup or the last merge,
including explicit appends.
Leading-space submissions and consecutive duplicates are omitted. The store
retains up to 10,000 records; POSIX history files remain separate.

On first use, an existing `native_fish_history` editor file is imported into the
new YAML file. The old file is left intact so older running shells can continue
using it. Later additions made by those old shells are not imported again.
Imported entries have no recorded timestamps and display the Unix epoch when
using `--show-time`. Rash continues to use `fish/native_rash_history` in its
existing format. `--private`/`-P` prevents loading and saving history while
retaining recall and history commands within the current session.

Run the isolated builtin/storage checks with:

```powershell
python tests/native_history_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
```

This is a separate native editor, not the full POSIX Fish reader.
Full Fish keymap compatibility remains separate work. The
standalone Rash executable on Unix retains its simple line-input fallback.
The native dependency carries a small [Windows input patch](../vendor/rustyline/FISH_PATCHES.md)
to Rustyline; its Unix backend and the POSIX Fish reader are unchanged.
Run the isolated Windows editor check with native Windows Python:

```powershell
python tests/native_interactive_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
python tests/native_fish_feel_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
python tests/native_prompt_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
```

It uses a fresh tmuxx owner and private configuration/history directories, leaving
existing terminal sessions and user files unchanged. The check covers Unicode
editing, filename/command/PATH completion, multiline input, Ctrl-C/Ctrl-Z recovery,
history reload and private mode, Fish configuration and `fish_prompt` status,
function completion, and separate Rash history.

The prompt check verifies physical cursor coordinates and preservation of
preceding output during typing, multiline editing and resizing. It inspects raw
ConPTY output to ensure editing and menu navigation preserve unchanged rows,
including transient writes and erasures hidden by the final screen state. It tests
ANSI-colored prompts, right/transient prompts, and the startup binding hook. It
also runs ordinary native Starship initialization when found on `PATH`; pass
`--starship C:/path/to/starship.exe` to select an explicit installation.

## Bash-style interactive input

Native Fish can accept Bash-shaped commands at the prompt while keeping the same
Fish evaluator, variables, functions, working directory and jobs:

```powershell
fish.exe --interactive-language=bash -i
```

The separated spelling `--interactive-language bash` also works. Use
`--interactive-language=fish` for the default Fish input syntax. These options
select only interactive input: `-c`, script files, redirected standard input,
`source`, `eval`, `config.fish` and `fish_prompt` retain Fish syntax. No Bash
process or Bash startup file is involved. Use `rash` or
`--script-language=bash` when Bash evaluation is required; those evaluator
options cannot be combined with `--interactive-language`.

The input dialect supports scalar assignments, ordinary commands and redirects,
pipelines, logical chains, brace groups, `if`/`elif`/`else`/`fi`, `for` and
`while`/`until` loops with `do`/`done`, function definitions, and `$()` command
substitutions:

```bash
name='native Fish'
greet() { printf 'hello %s\n' "$1"; }
for item in "$name" 'another value'; do greet "$item"; done
result=$(printf 'one two')
```

Expansion and scope follow Fish rules. Unquoted variables retain Fish lists;
they do not acquire Bash word splitting or globbing of variable contents.
Assignments update Fish variables, and functions defined in either input mode
can be called from the other. Fish commands such as `set`, `string` and `math`
keep their Fish behavior. Backticks are literal characters; use `$()` for command
substitution. Bash arrays, arithmetic syntax, `[[ ... ]]`, `case`, subshell
isolation, here-documents, here-strings and advanced Bash parameter expansions
are unsupported. The complete submission is translated and checked before any
of its commands run, so an unsupported later construct cannot execute an earlier
command as a side effect.

Switch syntax for the next prompt without starting another shell:

```fish
fish_input_mode bash
```

Run `fish_input_mode fish` to return. The rest of the current submission keeps
the syntax it started with. A mode change requires an explicit `fish` or `bash`
argument and an interactive session; prompts, command substitutions, and
pipeline/background workers cannot change the mode. To choose Bash input from
Fish configuration:

```fish
if status is-interactive
    fish_input_mode bash
end
```

Configuration runs after the command-line selection and can override it.
Unfinished Bash blocks remain in one editable multiline buffer. Suggestions,
history recall, completion and bracketed paste use the native Fish editor.
Syntax coloring and indentation follow the selected input syntax. Literal
completion registrations are shared; Fish abbreviations are disabled in Bash
input so an expansion cannot insert commands written for a different syntax.
History is shared between input modes and saves the original text you entered,
including multiline source, rather than the translated Fish commands. A recalled
command is interpreted using the currently selected mode. Autosuggestions are
checked against that mode's syntax; explicit history search can still recall
entries written in either syntax.

The isolated Windows integration check exercises configuration, unexported state,
functions, mode switching, editable multiline input, paste, history and rejection
before side effects:

```powershell
python tests/native_bash_input_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
```

## Pipelines and Windows job control

Both native shells run concurrent pipelines, including builtins, functions and
compound commands. Native Fish also supports `2>|` to pipe only stderr, leaving
stdout at its existing destination; explicit redirects take precedence over the
pipe. `&` starts a background job; `jobs`, `wait`, `fg`, `bg`,
`disown`, and `kill` operate on jobs created by this shell. Use `%1`, `%+`, `%-`
or an owned process ID to select a job. Fish exposes `$last_pid` and `$pipestatus`;
Rash exposes `$!` and `PIPESTATUS`, including `set -o pipefail`.

```fish
printf '%s\n' hello | string upper
echo value | read saved
echo $saved
# Start an installed native program, then control the job:
long-running-program &
jobs -l
kill -STOP %+
bg %+
fg %+
```

Windows Job Objects keep each pipeline and its descendants together. Ctrl-C
cancels a foreground job, with a bounded forced-termination fallback for programs
that ignore console events. `kill -STOP` suspends owned process threads;
`bg` and `fg` resume them. Ctrl-Break is cancellation too, not a POSIX suspend key.
`kill -TERM` and `kill -KILL` terminate the owned job. Arbitrary system processes
cannot be targeted through these builtins. Closing the shell terminates owned
jobs. `disown` removes this shell's cleanup policy, including for external
descendants that remain after their launcher exits. It cannot escape an enclosing
Windows Job Object or the terminal's own lifetime policy; disowning inside a
pipeline worker does not detach a process from its parent pipeline.

`kill -l`/`--list` lists the supported signals (`INT`, `KILL`, `TERM`, `CONT`,
`STOP`, and `TSTP`) and converts their names, numbers, and signal exit statuses.
Signals can also be selected with `-s`/`--signal`. Bundled `kill` completions offer
these signals and owned job process IDs. Unsupported options return status 2,
respect stderr redirection, and let the calling script continue; GNU signal
queues, signal tables, and arbitrary system process selection remain unavailable.

Windows has no POSIX terminal foreground process groups. Background jobs default
to null input unless redirected; `fg` cannot replace input already inherited by
a background process. Stopped foreground jobs retain their original input.
Shell stages run in isolated interpreter processes. A normally completed Fish
foreground pipeline imports the final stage's variables/functions/cwd, so
`echo value | read saved` works. Earlier stages and background jobs remain
isolated, and state from a stopped-then-resumed pipeline is not imported.
Pipeline workers also receive a snapshot of their parent's active jobs for
queries; nested workers preserve that snapshot. These inherited entries cannot
be controlled with `fg`, `bg`, `kill`, `wait`, or `disown` from the worker.
`jobs -g`/`--group` prints one stable identifier per running or stopped pipeline,
so `jobs -g | count` works in prompt helpers. On Windows the identifier is the
pipeline's first process ID, retained after that process exits; it is not a
Windows Job Object handle. Inherited snapshots do not update while a worker runs.

The isolated `tests/native_job_control_tmuxx.py` check exercises both languages
through a private tmuxx owner: external and interpreter Ctrl-C/status recovery,
background isolation, descendant suspension/resumption/termination, and cleanup.
Run it with native Windows Python, the native Fish executable, and tmuxx paths:

```powershell
python tests/native_job_control_tmuxx.py .\target\x86_64-pc-windows-msvc\release\fish.exe C:/Apps/Rustty/tmuxx.exe
```

Standard descriptors 0, 1 and 2 support ordered redirection and merged stderr
pipes. Additional descriptors, signal traps, `exec` replacement and background
jobs inside command substitutions remain unsupported in the native host.

## Rustty and tmuxx

Launch native Fish directly with an absolute Windows executable path:

```powershell
rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/release/fish.exe -i
rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/release/rash.exe -i
```

For a separate native tmuxx instance, start an owner in one terminal, then create
a pane from another. Choose an unused instance name; all commands below address
only that instance. `-f NUL` skips the user's tmuxx configuration.

```powershell
# First terminal: this owner remains running.
tmuxx.exe -D -L native-fish-trial -f NUL

# Second terminal: explicit argv selects the native executable.
tmuxx.exe -L native-fish-trial new-session -d -s work C:/src/fish-shell/target/x86_64-pc-windows-msvc/release/fish.exe -i
rustty.exe --tmuxx=local:native-fish-trial
```

Use `rash.exe -i` or `fish.exe --rash-shell -i` in the pane command for Bash input.
Windows tmuxx uses ConPTY and requires an explicit owner; its clients do not start
a server automatically. Existing servers retain their selected shell and argv.
Rustty recognizes `fish.exe` as a shell accepting `-c`; use explicit `-e` argv for
`rash.exe`, which older Rustty versions do not recognize for command-string launch.

The x64 MSVC executables have passed the isolated tmuxx ConPTY editor and job
checks described above. Command/script smoke checks also retained exit statuses
for both languages. These checks preserve existing Rustty/tmuxx sessions and user
configuration; full Rustty GUI qualification remains separate work.

To retain the full MSYS2 Fish environment instead, launch it through that MSYS2
installation, for example from PowerShell:

```powershell
rustty.exe -e C:/msys64/usr/bin/bash.exe --login -c 'exec /c/src/fish-shell/target/msys2/release/fish.exe -i'
```

When launching a *native* executable from MSYS2 or Git Bash with `-c` source,
disable automatic argument conversion for that invocation so shell source is
passed intact:

```sh
MSYS2_ARG_CONV_EXCL='*' ./target/x86_64-pc-windows-msvc/release/rash.exe -c 'printf "%s\n" "$PWD"'
```

That variable controls the invoking MSYS shell's argument conversion; it does not
select the build backend. These examples do not change Rustty, Windows Terminal
or running tmuxx settings. A terminal still configured to use
`target/msys2/release/fish.exe` continues to run the MSYS2 build.

## Remaining work

Concrete startup and completion gaps include edited or silent reads, FIFO
helpers, extra file descriptors, signal and per-process/job-exit subscriptions,
custom binding modes, multikey bindings, and advanced `status` introspection.
POSIX ownership, mode bits and terminal process-group
behavior do not have equivalent native implementations. Dynamic completions
that query an application's own commands still require that application;
successful script loading alone does not establish complete query coverage.

1. Complete native Fish expansion, builtins and execution semantics, with shared
   behavior checks against the POSIX implementation.
2. Extend descriptor handling, traps, pipeline state sharing and Windows
   foreground/background input behavior beyond the current owned-job support.
3. Extend native reader customization and dynamic completion compatibility for
   programs and POSIX services not covered by the current checks.
4. Qualify additional Windows targets and terminal environments, and consider
   installer formats beyond the portable ZIP and managed-directory installer.
