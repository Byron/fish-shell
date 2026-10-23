# Bash-style Fish input and the isolated Rash shell

The interactive dialect now uses **Fish evaluation and state**. Rash evaluates
only explicit Bash scripts and explicit Rash shell sessions. There is no native
Bash runtime dependency; a standalone `rash` executable is also built. Earlier reports
retain the history of the former dual-language session and its measurements.

Unless a section says otherwise, this guide describes the POSIX Fish frontend,
including the MSYS2 build. The initial native Windows frontend uses the shared
Fish parser, a smaller Fish evaluator and a portable Rash host. Its basic prompt
does not yet implement the editor, mode switching or job-control integration
described here. See [native-windows.md](native-windows.md).

## Build, invoke and switch

```sh
cargo build --release --bins --offline --locked
./target/release/fish --interactive-language=bash
```

In an existing Fish session, use `fish_input_mode bash`; return to normal spelling
with `fish_input_mode fish`. The switch applies after the entire current
submission. Variables, functions, aliases, cwd and running/stopped jobs remain in
the same Fish session. Neither switch creates an evaluator process.

For a login shell already configured as Fish, add this Fish code to
`~/.config/fish/config.fish`:

```fish
if status is-interactive
    fish_input_mode bash
end
```

The installed Fish must contain these patches. Building this checkout does not
replace the system/login executable. Fish initialization, functions, hooks,
completion scripts, keybindings, `-c`, `-C` and ordinary script files keep their
Fish language. Normal Fish remains the default. Mode requests in dedicated
Rash-shell Fish initialization are ignored so the same config can serve both.

## Input syntax and execution

`src/bash_to_fish.rs` parses the entire submission without executing it, lowers
supported structural syntax to Fish, and validates the resulting Fish code
before execution. It does not execute the completion projection and does not
fall back to Rash when a line is unsupported.

Supported forms include simple commands, pipelines/background commands, logical
chains, command-scoped assignments, standalone scalar assignments, ordinary
redirections, brace command groups, `if`/`elif`/`else`, `for`, `while`/`until`,
function definitions and recursively translated `$(...)` substitutions.
Bash lexical quotes/escapes are converted to equivalent Fish literals, except
that backticks are ordinary characters as in Fish, even when unmatched or inside
strings. They never introduce command substitution; `$(...)` and special
variables such as `$?` remain supported. Variable
expansion, lists/slices, globbing, scope, builtin options, command resolution,
pipelines and command-substitution behavior follow Fish rules. This is an input
dialect, not Bash compatibility.

For example, `value='a b'; printf '<%s>\n' $value` prints one argument. A Fish
list expands to its elements. `${name}` is spelling for a Fish variable;
`$?` maps to `$status`, `$$` to `$fish_pid`, `$!` to `$last_pid`, and positional
parameters to `$argv`/slices. Fish functions defined by startup code, including
Fish aliases and helpers such as `..`, run in Fish without a cross-engine bridge.
`..; echo "$PWD"` sees the new directory immediately.

Unsupported forms currently include Bash arrays/append assignments, parameter
operators such as `${x:-default}`, arithmetic expansion/commands, `[[ ... ]]`,
`case`, process substitution, heredocs/here-strings and subshell
isolation. Unsupported syntax is diagnosed with the input preserved. An invalid
construct later in a submission prevents earlier commands from running too.
Function bodies defined through Bash spelling become Fish code and obey Fish
function scope. Bash-spelled syntax is not recognized inside existing Fish
function bodies, scripts, `source`, or `eval` strings.

## Explicit Rash shell and scripts

**Rash — always with you.** Bash wherever this Fish instance is: its experimental
evaluator ships in the Fish executable, with no separate Bash installation.

```fish
rash
rash --no-login       # explicitly choose .bashrc
rash --login          # explicitly choose login profiles
rash --no-startup     # skip Bash startup/logout files
```

`rash` is a Fish function launching `(status fish-path)` as an ordinary foreground
job with `--rash-shell`. The underlying direct invocation is:

```sh
./target/release/fish --rash-shell
```

This is a fresh invocation of the same executable, not Rust interpreter code
running directly after `fork` in multithreaded Fish. The child inherits exported
environment, cwd and standard descriptors, and starts its own Bash state and job
table. Parent private variables, functions, aliases and jobs are not imported.
Fish config runs afresh for the child's editor/prompt/completions; those Fish
functions do not enter Rash command resolution. Rash owns Bash evaluation,
while the child reuses Fish's reader, prompt rendering and process services.

`exit` or EOF returns to the parent; its cwd/variables/functions remain unchanged
and it receives the child's exit status. The parent controls the child as a job;
each shell manages its own jobs and terminal handoff. `fish_input_mode` cannot
switch a submitted Rash session into the parent Fish session. Use `exit`.

The `rash` function reads Bash startup files by default and inherits
`status is-login` from the parent Fish. `--login` and `--no-login` override that
choice; `--no-startup` skips Bash startup and logout files. Private-history mode
also carries into the child. Fish UI configuration
is still controlled independently with `-N`/`--no-config`. The function preserves
forwarded Fish option arguments, including `-C` values that look like options.
Direct `fish --rash-shell` and explicit Bash scripts still require
`--bash-startup`; their low-level defaults are unchanged. A non-login interactive Rash shell reads `.bashrc`; a login shell reads
`/etc/profile` then the first readable `.bash_profile`, `.bash_login` or `.profile`.
`PROMPT_COMMAND` runs in Rash, while Fish renders the prompt. Arbitrary rc
framework compatibility is not claimed.

Sourcing is deliberately language-specific:

- `source config.fish` at the Fish-evaluated prompt changes the current Fish state.
- `source script.sh` inside Rash changes that child's retained Bash state.
- `fish --script-language=bash script.sh ARG...` runs an isolated noninteractive
  Rash session. It cannot change the calling shell's variables or directory.

The public Fish commands added by this work are `fish_input_mode` and `rash`.
The obsolete `bash_session` job proxy and generated `$bash_jobs` variable have
been removed. Use ordinary `jobs`, `fg`, `bg`, `wait` and `disown` in each shell.
Rash no longer advertises a rejection-only `fish_input_mode` builtin. The internal
`src/bash_session.rs` module remains the POSIX Fish Rash host; it is not a command.
Existing Fish/Rash history directories retain their names and data.

The script entry point remains explicit and keeps normal Fish script defaults.
Bash startup-file selection, script argument/return scoping and sourcing use the
existing Rash implementation and its documented compatibility limits. The Rash
shell requires a terminal and rejects script filenames and `-c`; use the explicit
script entry for noninteractive execution. Bash binaries are test oracles only.

## Completion, editing and history

The original input remains authoritative. Tree-sitter Bash supplies lexical
highlighting, movement and indentation; the structural translator determines
whether submitted dialect input is complete and supported. Existing Fish
completion definitions receive a transient Fish-readable view with cursor and
source mappings. Candidate descriptions, matching/ordering, pager behavior and
space/directory suffixes are retained. Returned edits preserve original variable
references and untouched source. The dialect queries live Fish functions,
builtins and variable lists; it never creates a Rash state snapshot.

Git, Cargo, SSH and Make providers remain unchanged. Focused PTY tests cover Git
with live variable-dependent directories, Fish functions, quoted/escaped and
Unicode filenames, options, trailing whitespace, middle-of-buffer edits,
descriptions/ordering, suggestions and history. Original provider feasibility
tests remain. File completion also covers `<`, `>`, `>>`, `>|`, `&>` and `&>>`,
descriptor prefixes and leading redirections, without opening their targets.
In the Fish input dialect, earlier wildcard arguments also allow filename
completion (for example `cp *.txt dest`), and brackets in filenames are literal.
Wildcard spelling is preserved; completion does not expand a current glob into
multiple words. Rash glob contexts remain outside this projection.
Completion projection still refuses compound/assignment contexts, here-documents,
here-strings, read/write redirects and substitutions it cannot map reliably. It never evaluates
unfinished substitutions; intentional provider commands retain existing behavior.
Leading unquoted `~/` paths support Tab completion and completion-based
suggestions, including when used as an earlier argument such as `git -C ~/repo`.
The POSIX Rash editor also supports `~/` using its scalar `HOME` value.
The original `~/` prefix is retained; other tilde forms and unsupported expansion
contexts remain outside this projection. Provider-driven `commandline` mutations remain explicitly rejected with input
preserved. Plain command abbreviations expand; replacements containing syntax,
quotes or escapes remain unexpanded until their Fish source can be mapped safely.

History stores beneath `$XDG_DATA_HOME/fish/`:

| Input/evaluator | History location |
| --- | --- |
| Ordinary Fish and Bash-style Fish input | shared existing `<fish_history>_history` |
| Rash shell | existing `bash-history/<fish_history>_history` |

Both Fish input modes use the same history and Fish's concurrent merge and
private-history machinery. Switching saves pending entries and clears undo.
Entries retain their original text; suggestions must pass the active syntax's
validation, so recalled commands using different syntax may need editing.
The dialect's reduced suggestion matching remains unchanged. Rash history stays
isolated. Existing `bash-style-history/` files are preserved but no longer used
or automatically imported; no history files are deleted.

## Rustty/Tmuxx trial

Use the absolute path to this Fish build. Rustty's `-e` forwards the executable
and its arguments directly:

```sh
rustty -e /path/to/fish --interactive-language=bash
rustty -e /path/to/fish --rash-shell --bash-startup
```

For a native Tmuxx session displayed in Rustty:

```sh
tmuxx -L fish-rash -f /dev/null new-session -d -s work \
  /path/to/fish --interactive-language=bash
rustty --tmuxx=local:fish-rash
```

Replace the Fish mode flag with `--rash-shell --bash-startup` to start that pane
in Rash. Add `--login` to Fish's arguments for login profiles; a direct launch
cannot inherit a parent Fish login flag.

For ordinary Rustty windows, locate the active settings with
`rustty --config-info` and set:

```ini
command = direct:/path/to/fish --interactive-language=bash
```

The `direct:` setting splits on spaces; use an executable path without spaces,
or use the quoted `-e` CLI form. This setting does not choose the shell for
later Tmuxx panes. For servers started by Rustty, also set:

```ini
tmuxx-config-file = ~/tmuxx.conf
```

Then use these defaults in `~/tmuxx.conf`:

```tmux
set -g default-shell /path/to/fish
set -g default-command 'exec /path/to/fish --interactive-language=bash'
```

Use `--rash-shell --bash-startup` instead in `default-command` for Rash panes.
Newly started servers load this file; existing servers retain their settings
until explicitly reloaded with `source-file`. These examples assume executable
paths without spaces. Selecting the dialect in Fish config, as shown above,
is also supported.

Fish retains OSC 133 prompt/command markers and OSC 7 cwd reporting. Try `..`,
Git completion, a background job, Ctrl-C/Ctrl-Z and `fg`, then `rash`, a child
`cd /`, and `exit`; the original Fish cwd and job table should still be present.

The headless native endpoint harness is:

```sh
python3 tests/bash_input_tmuxx.py target/release/fish /path/to/tmuxx
python3 tests/bash_input_tmuxx.py target/release/fish /path/to/tmuxx --rash-shell --bash-startup
```

Headless transport checks do not certify Rustty GUI navigation, directory display
or detach/reattach rendering. Current validation and release timings are recorded
in [bash-style-input-validation.md](bash-style-input-validation.md). The previous
[rash-validation.md](rash-validation.md) is historical evidence for the former
shared-session architecture. [rash-compatibility.md](rash-compatibility.md) remains
the execution-compatibility ledger for explicit Rash sessions/scripts.

## Native Windows, MSYS2 and WSL

Both executables now build natively with Cargo. The target selects the backend;
MSYS2 environment variables and feature toggles do not select it.

```powershell
cargo build --locked --bins --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\debug\fish.exe -N -c 'echo native Fish'
.\target\x86_64-pc-windows-msvc\debug\rash.exe -c 'printf "%s\n" "native Rash"'
rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/debug/fish.exe --rash-shell
```

The native frontend accepts Bash input through `rash`, `fish --rash-shell` and
`fish --script-language=bash`. Unlike the POSIX Fish Rash-shell entry point,
native `fish --rash-shell` also accepts `-c` and script files. Startup files remain
opt-in with `--bash-startup`. The native basic prompt does not yet include Fish's
editor, completion, history, configuration or terminal integration. Pipelines,
background jobs, signal traps and job control still require the POSIX host.

Native Windows tmuxx uses ConPTY. Start a separate owner explicitly with
`tmuxx.exe -D -L native-fish-trial -f NUL`, then use the same instance name in
`new-session` and Rustty's `--tmuxx=local:native-fish-trial` argument. Keep the
native Fish path in the pane's explicit argv. Native Fish and Rash command
execution, exit status and the no-argument Fish prompt have been checked through
an isolated tmuxx owner without changing the user's active server or settings.
Full commands and current limits are in [native-windows.md](native-windows.md).

For the full POSIX Fish frontend on Windows, build with MSYS2's
`x86_64-pc-cygwin` Rust toolchain or inside WSL. Existing terminals configured to
launch `target/msys2/release/fish.exe` continue to use MSYS2. Building the native
target does not update those paths. The Rustty and Windows Terminal user
configuration is unchanged.

If an MSYS2 or Git Bash shell launches a native executable with `-c` source, use
`MSYS2_ARG_CONV_EXCL='*'` for that invocation to preserve the command text. This
controls argument conversion, not the build target. Rash does not bundle Git or
external Unix tools, and its incomplete Bash semantics do not yet replace all
Git Bash workloads.
