# Native utility integration follow-ups

The default-enabled `native-utilities` feature packages existing Rust utility
libraries in native Windows Fish. POSIX Fish keeps using installed commands.
Native Fish first resolves the session's `PATH` and `PATHEXT`, including local
unexported values. A selected executable's error is returned without retrying a
packaged utility. Functions, aliases and explicit paths retain their precedence.
An empty PATH list searches no directories; an empty element explicitly includes
the current directory.
Utilities run in separate children of Fish so their global state, standard
streams and process exit cannot affect the evaluator.

The tested dependency set is uutils coreutils/uucore 0.10.0, sed 0.2.0,
uu_grep 0.2.0, findutils 0.10.0 and awk-rs 0.2.0. The lockfile retains the exact
dependency graph. The Windows build was checked with Rust 1.99.1; compilation
with the workspace's Rust 1.88 minimum remains unverified.

## Concrete upstream issues and integration constraints

1. **AWK special-variable initialization (API behavior).**
   In [awk-rs 0.2.0](https://github.com/quinnjr/rawk/tree/v0.2.0),
   `Interpreter::set_variable()` writes the ordinary variable map without
   updating the interpreter's `FS`, `OFS`, or `ORS` state. A CLI adapter using it
   for `-v FS=' +- +'` does not split `command - description` correctly.
   Fish uses the public `assign_to_lvalue(Expr::Var(...), Value::from_string(...))`
   API instead and tests the resulting fields. A public assignment API with the
   same semantics as an AWK assignment would make embedding less error-prone.

2. **AWK `exit` skips `END` blocks (reproduced bug).**
   `awk 'BEGIN { print "begin"; exit 7 } END { print "end" }'` prints only
   `begin` with awk-rs 0.2.0. It should also run `END` before exiting with status
   7. `Interpreter::run()` returns immediately when `should_exit` is set during
   `BEGIN` or input processing. This remains an upstream limitation; Fish does
   not replace the interpreter or pretend to implement full GNU AWK behavior.
   An installed AWK selected through `PATH` can supply that behavior.

3. **AWK subprocesses require `sh` (platform/API limitation).**
   The interpreter's `system()` and command-pipe implementations explicitly run
   `sh -c`. No shell is bundled by this feature, so these forms require an
   installed `sh`. A caller-supplied process launcher would allow portable host
   integration. Fish's adapter also rejects assignment operands between input
   files, such as `awk PROGRAM first FS=: second`, because the library's run API
   does not expose that sequencing; use `-v name=value` for initial assignments.
   Input files open lazily as the interpreter reaches them, so a `BEGIN` exit
   does not require those files to exist. AWK input and arguments are text, not
   a byte-preserving Unix stream API.

4. **Findutils uses a different uucore version (dependency/API follow-up).**
   [findutils 0.10.0](https://github.com/uutils/findutils/tree/0.10.0)
   requires uucore 0.9.0 while the other packaged utilities use 0.10.0. Both must
   be initialized with their own multicall setting. Aligning the dependency
   would remove duplicate runtime code. Its public `find_main`/`xargs_main`
   interfaces accept UTF-8 `&str` arguments, so Fish reports invalid Unicode
   instead of converting it lossily.

5. **Find output uses native Windows separators (compatibility constraint).**
   `find C:/fixture/std -name index.html` can print
   `C:/fixture/std\index.html`. This broke the shared Rustup completion's
   slash-based module matching even after selecting the packaged `find`.
   The Rustup helper now normalizes native separators before matching; the
   general utility adapter preserves output, including `-printf` and `-exec`
   output. This is a platform spelling difference, not an assumption that all
   `find` output is safe to rewrite.

6. **Uucore embedding uses process-global invocation state (API constraint).**
   [uucore 0.10.0](https://github.com/uutils/coreutils/tree/0.10.0/src/uucore)
   derives the utility name from process arguments even when `uumain` receives
   a custom iterator. Fish invokes its private child as
   `fish.exe NAME --native-utility-v1 ARGS...` and sets the second-argument
   utility mode. It passes `std::env::args_os()` to the utility, because
   uucore's Windows argument iterator expands wildcards and would expand quoted
   Fish arguments a second time. A per-invocation context could make embedding
   simpler; child isolation remains necessary for process-exiting utilities.

7. **MSVC debug linkage reports duplicate PCRE2 object names (build follow-up).**
   The native library test link succeeds but emits `LNK4255` for objects in
   `libpcre2_sys`: the linker treats those objects as if they have no debug
   information. The focused tests pass. The cause in the dependency/build graph
   has not yet been isolated; retain this observation when updating utility or
   regex dependencies.

## Shell integration limits

Windows `sort.exe` and `find.exe` remain valid ordinary PATH commands; their
options differ from Unix utilities. Shared helpers that require Unix behavior
can call `__fish_sort` or `__fish_find`. Those adapters explicitly select the
packaged command on a native build with utilities enabled, and use `command`
lookup on POSIX or feature-disabled builds. Rustup component and documentation
completions exercise these adapters. Other bundled scripts should be adapted
when a failing invocation is demonstrated, rather than replacing every command.
The adapters resolve the native builtin name through a variable: POSIX Fish
validates literal builtin names while parsing a function, even in an untaken
branch, so a literal native-only builtin would prevent the shared helper loading.

Rustup's shared component and toolchain helpers compare Fish lists directly,
without `comm`, FIFOs, or process-substitution cleanup. Component names strip
only a known final target triple, retaining platform-independent names such as
`rust-src`. Explicit toolchain selection is forwarded as separate arguments;
completion list queries disable automatic installation of missing toolchains.

Utilities that launch commands themselves, including `env`, `xargs`, `find
-exec`, and AWK subprocesses, use their own process lookup. They cannot discover
Fish functions or Fish-only packaged fallbacks in an empty PATH. The utility
feature does not supply POSIX services, `/etc` databases, process substitution,
or tools such as `stty`, `id`, `groups`, `chmod` and `chown`. Scripts depending on
those still need adaptation or installed tools.

`tests/native_utilities.rs` covers isolated fallback execution, PATH and
function/alias precedence, local PATH, explicit and invalid executable errors,
streaming pipelines, binary redirection, quoted wildcards, AWK field assignment,
find/grep queries and Rustup's Windows command-name collisions. Invalid Windows
executable images originally displayed a blocking loader dialog; native process
spawning now suppresses those dialogs on the calling thread and returns the
Windows error to the shell, restoring the prior thread setting afterward.
