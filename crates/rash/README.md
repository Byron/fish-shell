# Rash: Bash in Rust, embedded in Fish

*always with you*

Bash wherever this Fish instance is. Rash ships inside the Fish executable;
type `rash` to launch its experimental Bash evaluator without finding or
installing a separate Bash interpreter.

Fish calls Rash as an **in-process Rust library** for explicit Bash scripts and
`fish --rash-shell`. Ordinary Bash-style interactive input lowers to Fish instead. There is no native Bash
runtime, evaluator worker, companion executable, or fallback. Bash is used only
by explicit test oracles. This is an experimental, incomplete source-guided
port, not a claim of complete GNU Bash compatibility.

Rash owns parsing, expansion, variables, functions, aliases, source/startup
execution and control flow. `Session` retains this state across submissions.
Its `Host` interface exchanges expanded argv, environment, ordered redirections,
I/O, process results and signal registrations. Fish supplies process launching,
the invoked process's job table, signal handling and terminal ownership. Existing Fish
configuration, functions, prompts, hooks, completion scripts and `fish -c` stay
Fish code. See the [integration guide](../../doc_internal/bash-input-feasibility.md)
for invocations, the isolated child-shell boundary, history, completion,
Rustty/Tmuxx setup and the Windows/WSL example. The current core and Fish host
use Unix APIs; a native Windows port remains future work. Rash does not bundle
Git or the external tools a script may invoke.

The source baseline is GNU Bash **3.2.57**, GPL-2.0-or-later, compatible with Fish's
GPL-2.0-only license. Source notices and [COPYING](COPYING) are retained. Modern
associative arrays and namerefs are independent implementations; Bash 5.3 is a
behavioral oracle only. [Source preparation](../../build_tools/rash/README.md)
records the signed archive, all 57 patches and donor hashes.

The crate forbids unsafe Rust. It depends on the existing `fish-printf` Rust
crate for numeric formatting and has no build script. Shell values and source
are byte vectors; invalid UTF-8 is preserved. OS-dependent process, descriptor
and signal operations belong to the host. No arbitrary interpreter executes
after `fork` in the multithreaded Fish process.

## Components

| Rust component | Bash 3.2 source guidance | Implemented scope |
| --- | --- | --- |
| `quote`, `ansi_c` | `lib/sh/shquote.c`, `lib/sh/strtrans.c`, `syntax.h` | Byte-preserving quoting and ANSI escape helpers |
| `status` | `jobs.c`, `execute_cmd.c` | Process outcomes, pipeline reduction and inversion |
| `syntax`, `syntax_print` | `parse.y`, `command.h`, `print_cmd.c` | Lazy input-unit parsing, aliases, raw words/spans, grammar and function display |
| `arithmetic`, `expand` | `expr.c`, `subst.c` | Arithmetic, parameters, substitutions, splitting, globbing and patterns |
| `builtins_printf` | `builtins/printf.def` | Bash argument conversion, byte strings, numeric formats, `%b`, `%q`, `%n`, `-v` |
| `session`, `host` | `eval.c`, `execute_cmd.c`, `variables.c`, builtin behavior | Retained state, evaluator, builtins and embedding services |

Parsing and completion projection never call execution-time expansion. Parsing
one outer input unit at a time preserves aliases defined on earlier lines.
Read-only syntax checks do not execute earlier commands to discover aliases.

## Checks

```sh
cargo test -p rash --offline
cargo clippy -p rash --all-targets --offline -- -D warnings
RASH_BASH_ORACLE=/absolute/path/to/bash-3.2.57 \
  cargo test -p rash --test expansion_oracle --test syntax_oracle -- --ignored
python3 tests/rash_conformance.py --oracle /absolute/path/to/bash \
  --candidate ./target/release/fish \
  --candidate-arg=-N --candidate-arg=--script-language=bash \
  --report target/rash-conformance.json
```

The original quote, ANSI-C and status oracle tests remain available. They require
explicit `RASH_QUOTE_ORACLE`, `RASH_ANSI_C_ORACLE` and `BASH_ORACLE` paths and are
ignored in normal Cargo runs. Leaf comparisons do not establish shell semantics;
[the compatibility report](../../doc_internal/rash-compatibility.md) records
shell and PTY evidence separately. A Bash 3.2 oracle skips modern fixtures
explicitly; rejecting a modern feature in both programs is never counted as a pass.

## Known execution boundaries

- External pipelines/background commands use Fish jobs. Stateful builtins,
  functions and compound commands in these positions are explicitly rejected.
  Selected stateless pipeline builtins currently use system utilities; their
  option/format behavior can differ from Bash builtins.
- Subshells and command substitutions clone language state and restore cwd and
  trap dispositions. They do not yet provide full OS process isolation. PID,
  descriptor, signal and job side effects need additional compatibility work.
  `exec` in these scopes is rejected. Parent pipeline variables are isolated
  before expansion.
- Arithmetic `for`, `select`, `coproc`, process substitution and some newer
  operators are unsupported. Regex conditionals are parsed but not evaluated.
  Builtin coverage, attributes, array edge cases, traps and `errexit` are partial.
- Locale collation/ranges, arbitrary startup frameworks, Bash programmable
  completion and Readline behavior are not certified. Fish owns interactive
  editing, history and prompts. Bash `PS1` is not evaluated as the Fish prompt.

Unsupported syntax produces a diagnostic, never Bash-to-Fish evaluation or a
native fallback. Continue the port with small source-component patches and
independent differential cases. Genuine process isolation, full builtin/error
semantics and broader GNU regression coverage remain substantial work.
