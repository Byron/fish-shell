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
cargo build --locked --bins --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\debug\fish.exe -N -c 'echo native Fish'
.\target\x86_64-pc-windows-msvc\debug\rash.exe -c 'printf "%s\n" "native Rash"'
```

With an MSVC host toolchain, `cargo build --locked --bins` selects the native
backend automatically and writes to `target/debug`. Use `--release` for optimized
builds. This milestone supports native Cargo builds; CMake installation and
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

## Native command and script execution

Native `fish` uses the existing Fish AST and tokenizer with an initial native
evaluator. Ordinary `fish -c`, script files and the prompt evaluate Fish syntax.
`fish -n` checks Fish syntax without executing it. Unsupported execution features
produce diagnostics.

The current Fish subset includes simple commands and standard redirects on
descriptors 0, 1 and 2; list variables, quoting and simple integer indices;
`if`/`else`, `begin`, `for`, `while`, functions without options, `not`, and logical
chains. Initial builtins include `echo`, `set`, `cd`, `pwd`, `count`, `test`, `[`,
`true`, `false`, `source`, `eval`, `exit`, `return`, `break` and `continue`.
Native Fish also provides `printf`, `math`, `contains`, `read`, `type`, command
queries, function queries/removal, and common `status` queries. `math` uses the
same arithmetic parser and functions as POSIX Fish. `string` supports literal
split, join, replace, trim, case conversion, Unicode length/substrings, repeat,
escaping, and wildcard matching, with arguments or redirected standard input.
Command substitutions, globbing, brace expansion, `switch`, block redirections,
function options and the full builtin set are not implemented. Regular-expression
string operations, advanced read options and locale-specific numeric formatting
remain unavailable; unsupported options produce diagnostics.

Use `rash`, `fish --rash-shell`, or `fish --script-language=bash` for Bash syntax:

```powershell
.\target\x86_64-pc-windows-msvc\debug\rash.exe -c 'x=world; printf "hello %s\n" "$x"'
.\target\x86_64-pc-windows-msvc\debug\fish.exe --rash-shell -c 'printf "%s\n" "$PWD"'
.\target\x86_64-pc-windows-msvc\debug\fish.exe --script-language=bash .\script.sh argument
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

With a terminal and no command or script, the native frontend offers a basic line
prompt. It has no Fish editor, completion, persistent history, automatic Fish
configuration, or Fish prompt integration yet. `-N` is accepted for invocation
compatibility; native Fish does not load configuration even without it. The
portable process host does not yet implement pipelines, background jobs,
job control, signal traps, or `exec` replacement. Rash's other language limits
remain documented in [rash-compatibility.md](rash-compatibility.md).

## Rustty and tmuxx

Launch native Fish directly with an absolute Windows executable path:

```powershell
rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/debug/fish.exe -i
rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/debug/rash.exe -i
```

For a separate native tmuxx instance, start an owner in one terminal, then create
a pane from another. Choose an unused instance name; all commands below address
only that instance. `-f NUL` skips the user's tmuxx configuration.

```powershell
# First terminal: this owner remains running.
tmuxx.exe -D -L native-fish-trial -f NUL

# Second terminal: explicit argv selects the native executable.
tmuxx.exe -L native-fish-trial new-session -d -s work C:/src/fish-shell/target/x86_64-pc-windows-msvc/debug/fish.exe -i
rustty.exe --tmuxx=local:native-fish-trial
```

Use `rash.exe -i` or `fish.exe --rash-shell -i` in the pane command for Bash input.
Windows tmuxx uses ConPTY and requires an explicit owner; its clients do not start
a server automatically. Existing servers retain their selected shell and argv.
Rustty recognizes `fish.exe` as a shell accepting `-c`; use explicit `-e` argv for
`rash.exe`, which older Rustty versions do not recognize for command-string launch.

The x64 MSVC executable has been checked through an isolated tmuxx ConPTY owner:
a Fish list/loop command printed its values and retained exit status 7, a Rash
command printed its marker and retained exit status 9, and a no-argument Fish
session displayed its prompt and executed text sent through `send-keys`.
That check left the existing Rustty/tmuxx session and user configuration unchanged;
it does not certify the full GUI or editor behavior.

To retain the full MSYS2 Fish environment instead, launch it through that MSYS2
installation, for example from PowerShell:

```powershell
rustty.exe -e C:/msys64/usr/bin/bash.exe --login -c 'exec /c/src/fish-shell/target/msys2/release/fish.exe -i'
```

When launching a *native* executable from MSYS2 or Git Bash with `-c` source,
disable automatic argument conversion for that invocation so shell source is
passed intact:

```sh
MSYS2_ARG_CONV_EXCL='*' ./target/x86_64-pc-windows-msvc/debug/rash.exe -c 'printf "%s\n" "$PWD"'
```

That variable controls the invoking MSYS shell's argument conversion; it does not
select the build backend. These examples do not change Rustty, Windows Terminal
or running tmuxx settings. A terminal still configured to use
`target/msys2/release/fish.exe` continues to run the MSYS2 build.

## Remaining work

1. Complete native Fish expansion, builtins and execution semantics, with shared
   behavior checks against the POSIX implementation.
2. Add native pipelines, descriptor handling, process lifetime management and
   terminal cancellation; map job behavior to Windows facilities.
3. Port the interactive editor, completion, configuration, history and terminal
   integration, including cwd and prompt reporting to Rustty/tmuxx.
4. Add native installation and packaging, and qualify additional Windows targets
   and terminal environments.
