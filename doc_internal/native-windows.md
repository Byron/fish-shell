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

With an MSVC host toolchain, `cargo build --locked --release --bins` selects the
native backend automatically and writes optimized executables to `target/release`.
This milestone supports native Cargo builds; CMake installation and
packaging still use the POSIX build. `fish_indent` and `fish_key_reader` currently
build as native executables that report their functionality is unavailable.

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

Native `fish` uses the existing Fish AST and tokenizer with an initial native
evaluator. Ordinary `fish -c`, script files and the prompt evaluate Fish syntax.
`fish -n` checks Fish syntax without executing it. Unsupported execution features
produce diagnostics.

The current Fish subset includes simple commands and standard redirects on
descriptors 0, 1 and 2; list variables, quoting and simple integer indices;
`if`/`else`, `begin`, `for`, `while`, `switch`, functions without options, `not`, and logical
chains. Initial builtins include `echo`, `set`, `cd`, `pwd`, `count`, `test`, `[`,
`true`, `false`, `source`, `eval`, `exit`, `return`, `break` and `continue`.
Native Fish also provides `printf`, `math`, `contains`, `read`, `type`, command
queries, function queries/removal, and common `status` queries. `math` uses the
same arithmetic parser and functions as POSIX Fish. `string` supports literal
split, join, replace, trim, case conversion, Unicode length/substrings, repeat,
escaping, and wildcard matching, with arguments or redirected standard input.
Command substitutions, brace lists, indirect variables, and list index ranges
are supported. Native filesystem globbing expands unquoted `*` and recursive
`**` directory components. Quoted stars and stars from variable values stay
literal. Unmatched globs are empty lists for `set`, `count`, and `for`, and produce
status 124 for ordinary command arguments. Command substitutions retain Fish's
newline splitting and variable side effects; quoted substitutions preserve
internal newlines. `switch` expands case arguments and selects the first matching
case; quote wildcard patterns, for example `case '*.txt'`. Blocks support standard
redirections, with a shared file position across their commands: `while read line;
echo $line; end < input.txt > output.txt` reads and writes each line in order.
Nested redirections are restored when a block finishes, including `return` and
`break`. Function options and the full builtin set are not implemented. Regular-expression
string operations, advanced read options and locale-specific numeric formatting
remain unavailable; unsupported options produce diagnostics.

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

With a terminal and no command or script, the native Windows frontend offers
interactive line editing and history, described below. The portable process host
supports pipelines and owned jobs as described below.
Signal traps and `exec` replacement remain unavailable. Rash's other language limits
remain documented in [rash-compatibility.md](rash-compatibility.md).

## Interactive editing and startup

Native Windows Fish and Rash use a Unicode line editor that works through ConPTY.
Left/Right, Home/End, Backspace and Delete edit the current input; Up/Down recall
history. Tab completes filenames and command names from builtins, the session's
functions and its current `PATH`. Completion quotes spaces and shell punctuation
in filenames without evaluating shell source. Input can span multiple lines;
completed multiline submissions are stored as one history entry. Ctrl-C cancels
the current edit and returns to the prompt. Ctrl-Z at the prompt does not suspend
the shell itself.

Native Fish suggests the remainder of a matching history entry in gray as you
type. Right or Ctrl-F accepts the suggestion at the end of the input; Alt-Right
or Alt-F accepts one word. Enter executes only the text you have accepted. Up and
Down search history for the text you typed (including matches inside a command),
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

Literal command abbreviations can be defined with `abbr --add`, listed with
`abbr --show`/`--list`, queried with `--query`, and removed with `--erase`:

```fish
abbr --add gs 'git status'
bind ctrl-t complete
```

Space expands a matching bare command word while editing. Enter expands and
executes complete input in one press; unfinished expansions continue in the
editable buffer with normal indentation. Quoted words, ordinary arguments
and noninteractive scripts are not expanded. Abbreviations inside command
substitutions, regex/function generators and cursor markers are not supported.
Definitions belong to the session; put them in `config.fish` to recreate them.

Native `bind` maps one key to one supported input action. `bind --function-names`
lists actions and `bind --key-names` lists named keys; `bind --query KEY` checks a
binding. Modern names such as `ctrl-t`, `alt-f`, `left` and `f5`, Unicode single
keys and legacy control-character spellings are accepted. `bind --erase KEY`
restores the previous default, and `bind --erase --all` removes all user overrides.
Binding lists can be sourced as Fish code. Shell-script actions, multiple-key
sequences, multiple actions and binding modes are not supported. An open
completion menu handles its navigation and cancellation before user bindings.

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

Git completion queries local branches, remote branches, tags and `HEAD` for
revision arguments to `reset`, `checkout`, `switch`, `merge`, `rebase`, `diff`,
`log`, `show`, and `restore --source`. For example, `git reset fea<Tab>` offers
matching branches as well as paths; `git reset --hard fea<Tab>` offers refs only,
and `git reset -- <Tab>` offers paths only. New branch names and message/strategy
arguments do not receive revision suggestions. Literal `-C`, `--git-dir`,
`--work-tree`, `--namespace` and `--bare` routing is supported, including packed
refs and linked worktrees. Fixed read-only Git queries use the session's exported
environment and `git.exe` from its `PATH`, with a short cache, a 500 ms deadline
and bounded output. Missing Git or an unavailable repository yields no refs.
Typed configuration overrides and routing requiring shell expansion are not
evaluated. This native provider does not load the full POSIX `git.fish` script;
Git aliases, all option combinations, revision expressions and tracked-file
filtering remain outside its current scope.

Register literal options and choices in `config.fish` or at the prompt:

```fish
complete -c mytool -f -a 'build test'
complete -c mytool -l mode -r -a 'fast thorough' -d 'Select checking mode'
```

Native `complete` supports `-c`, `-s`, `-l`, `-o`, `-a`, `-d`, `-f`, `-r`, `-x`,
and `-e` (erase), with their long forms. `complete` lists registrations as Fish
source; `complete -C 'mytool --m'` prints the editor's annotated choices.
Registrations belong to the current session and can be recreated by configuration.
Conditions, wrapper definitions, command-substitution generators, automatic
completion-script loading, and path completion through `$VARIABLE/` are not yet
supported. Dynamic registrations report a diagnostic instead of silently
running as static choices.

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

Native Fish reads `fish/config.fish` under `XDG_CONFIG_HOME`, or under `%APPDATA%`
when that variable is not set. Without either location it uses
`~/.config/fish/config.fish`. `-N`/`--no-config` skips this file. The configuration
runs in the native evaluator and must use its supported Fish syntax and builtins.
Native Fish calls a defined `fish_prompt` function before each new command and
uses its output as the prompt, preserving the previous command's `$status` and
`$pipestatus`. ANSI colors in multiline prompts, including native Starship output,
do not contribute to cursor columns or wrapped rows, so editing preserves output
above the prompt. Without that function it displays `fish>`. Function autoloading
and vendor configuration are not provided. Native Fish reports cwd with OSC 7
and accepted-command lifecycle/status with OSC 133. Rustty/tmuxx can track cwd,
command text and exit status without an MSYS wrapper or injected startup script.
Suggestions and canceled edits are not reported as executed commands. Metadata
is omitted for redirected output and `TERM=dumb`.
Rash displays `rash>` and uses its separate Bash startup files with
`--bash-startup`; `-N` also skips those files.

History is stored under `fish/native_fish_history` or `fish/native_rash_history`
in `XDG_DATA_HOME`, falling back to `%LOCALAPPDATA%`, then `~/.local/share`.
The editor appends completed submissions with file locking so concurrent sessions
preserve each other's entries. Leading-space commands and consecutive duplicates
are omitted. These files use a separate format and do not overwrite or import
the existing POSIX Fish histories. `--private`/`-P` prevents loading and saving
history while retaining recall within the current session.

This is a separate native editor, not the full POSIX Fish reader.
Dynamic completion scripts and full Fish keymap compatibility remain separate work. The
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
ANSI-colored prompts and also runs native Starship when found on `PATH`; pass
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
compound commands. `&` starts a background job; `jobs`, `wait`, `fg`, `bg`,
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

Windows has no POSIX terminal foreground process groups. Background jobs default
to null input unless redirected; `fg` cannot replace input already inherited by
a background process. Stopped foreground jobs retain their original input.
Shell stages run in isolated interpreter processes. A normally completed Fish
foreground pipeline imports the final stage's variables/functions/cwd, so
`echo value | read saved` works. Earlier stages and background jobs remain
isolated, and state from a stopped-then-resumed pipeline is not imported.
Job builtins inside a pipeline worker see only that worker's own jobs.

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

1. Complete native Fish expansion, builtins and execution semantics, with shared
   behavior checks against the POSIX implementation.
2. Extend descriptor handling, traps, pipeline state sharing and Windows
   foreground/background input behavior beyond the current owned-job support.
3. Extend the native editor and completion toward full Fish behavior, including
   programmable scripts and reader customization.
4. Add native installation and packaging, and qualify additional Windows targets
   and terminal environments.
