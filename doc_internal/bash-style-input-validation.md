# Bash-style Fish input: validation and release measurements

Measured 2026-10-21. The main dialect uses Fish evaluation; the dedicated Rash
shell uses Bash evaluation in a freshly launched process. Passing dialect tests
is not evidence of Bash compatibility. Invocation, supported syntax, state,
history and Rustty/Tmuxx procedures are in
[bash-input-feasibility.md](bash-input-feasibility.md).

## Built artifacts and environment

The release binary is `target/release/fish`, built with
`cargo build --release --bins --offline --locked`, default features, release LTO
and overflow checks. No native Bash dependency or companion binary was built.
The final incremental build took 30.36 seconds; this is not a clean-build metric.

- Runtime revision: `9f61557f5ceaca79b47a7e5069782041f134f540`.
- Version: `fish, version 4.9.3-161-g9f61557f5-dirty`.
- SHA-256: `3f401386959e38d6d74e4bbf41bc968fbd424a57abaed26cf765342d242e3fe4`.
- Pending documentation/benchmark changes account for the dirty suffix. The
  following documentation patch also clarifies two source comments; runtime
  behavior is unchanged.
- Baseline: `target/release/fish-baseline`, revision
  `384447640af76e494212f05ca5f18bdb51b6ec68`, version `4.9.3`, SHA-256
  `72de0ba404a26ab36639c02ea8136f5dc2ea940554d66fd91e698a1af636b136`.
  Its matching release build and archived-source provenance are recorded in
  [the prior report](rash-validation.md#binaries-and-environment).

Host: macOS 27.0.1 (26A434), arm64. Rustc 1.99.0 (b940084d7), Cargo 1.99.0
(5f94df478), Python 3.14.7, pexpect 4.9.0, Git 2.54.0 (Apple Git-157), Tmuxx 0.1.0.
Sphinx is unavailable, so neither binary embeds generated man pages; command
reference sources are present. No installation, login-shell configuration change
or push was performed.

## Release latency

Each cell is **p50 / p95 milliseconds**. The last four columns use the same
patched binary; the first is the existing ordinary-Fish release baseline.

| Workload | Baseline Fish | Patched Fish | Bash-style Fish | Rash | Rash + small rc |
| --- | ---: | ---: | ---: | ---: | ---: |
| Configured startup | 31.904 / 35.581 | 31.385 / 34.559 | 32.626 / 36.065 | 32.538 / 37.955 | 33.857 / 35.283 |
| true to next prompt | 1.230 / 1.888 | 0.953 / 1.409 | 1.051 / 1.270 | 0.983 / 1.206 | 0.964 / 1.143 |
| Tab: git --ver | 12.346 / 12.898 | 12.197 / 12.654 | 12.411 / 13.345 | 12.505 / 13.572 | 12.902 / 14.379 |
| Tab: git -C "$repo" switch topic- | 27.454 / 29.661 | 27.432 / 28.295 | 27.985 / 31.328 | 27.646 / 32.408 | 27.057 / 30.516 |

Startup has 15 interleaved/rotating samples per case; prompt return has 30; Tab
completion has 20 warm samples after excluding the first request. The fixture
uses isolated HOME/XDG directories, a fixed `PROBE>` prompt and a small local Git
repository in a path containing spaces and Unicode. The rc adds one export,
alias and function. These are warm-cache wall-clock observations including PTY
transport and the buffer-observer keybinding. Tests and builds were not running
concurrently with this final measurement.

The dialect adds about a millisecond to startup relative to patched ordinary
Fish in this workload. Prompt and completion times remain close. Small numerical
differences do not establish speedups or equivalent performance. Arbitrary
prompt frameworks, large function directories/repos, network filesystems and
other platforms were not benchmarked. Debug timings are not used.

Reproduce:

```sh
/tmp/fish-bash-probe-venv/bin/python tests/bash_input_bench.py \
  target/release/fish-baseline target/release/fish \
  > target/bash-style-release-bench.jsonl
```

Raw samples, versions, source/binary hashes and build details are retained in
`target/bash-style-release-bench.jsonl` and
`target/bash-style-release-metadata.json`. These are local artifacts; the report
is versioned. The earlier measurement before the final breakpoint-reader guard
is kept as `target/bash-style-release-bench-before-final.jsonl`.

## Validation

- **199 Fish library tests passed** on the final runtime tree, including the five
  translator tests, projected lexical escapes and nested-reader history selection.
  The sandbox initially blocked the unchanged macOS notifyd registration test;
  the final entire library suite passed with normal local OS access.
- **45 Rash unit tests passed**. Optional external-oracle leaf tests remain in
  the suite but were not rerun for this architecture change.
- **27 selected Fish checks/PTys passed**: default invocation/script/source,
  signals/traps, jobs, foreground/background control, wait/disown, command
  substitutions, and postexec behavior. Initially five PTY tests failed before
  running because their subprocess Python lacked pexpect; they passed with the
  test virtualenv on PATH. Logs separate the 22 script checks and five reruns.
- **Eight suites / 42 checkpoints passed on the final release binary**:
  `bash_style_input.py`, `bash_input_switch.py`, `bash_input_integration.py`,
  `bash_input_semantics.py`, `bash_source_semantics.py`, `bash_startup.py`,
  `bash_signal_integration.py`, and `bash_script_semantics.py`.
- **Two native Tmuxx trials passed**: Bash-style Fish and Rash with startup,
  checking direct argv launch, state, cwd, Ctrl-Z/C, foreground jobs, resize,
  pane snapshot and raw OSC 133/7 markers.
- An additional Fish-function breakpoint probe passed: native Fish syntax and
  history inside the debugger, then preserved state on return to the dialect.
  A breakpoint before the outer reader was not exercised: `-C` and prompt
  execution treat that builtin as noninteractive/no-op in the test setup.
- Formatting/syntax checks and `git diff --check` passed.

The dialect suite exercises Fish aliases/functions and `..`, plain abbreviations,
shared private variables, Fish list/scalar expansion, immediate directory/status
visibility, translated control flow/functions, Fish sourcing, atomic rejection,
unchanged Git and wrapped-alias completion, live variable-dependent Git context,
options/descriptions/ordering, quoting/Unicode/literal metacharacter filenames,
ASCII and Unicode backslash escapes, cursors/trailing whitespace, provider edits,
suggestions/recall, separate old histories, concurrent merges and private mode.

Nested-shell checks verify a distinct PID and the same executable, parent/child
state and job isolation, source/startup behavior, exit status and terminal
ownership. Bash evaluation differential suites use GNU Bash 5.3.0(1)-release at
`/private/tmp/fish-native-bash-probe/bash-5.3/bash` for interactive/source behavior,
and GNU Bash 3.2.57(2)-release at `/private/tmp/fish-rash-prepared/build/bash` for
scripts. These native binaries are test oracles only.

Reproduce the main suites (Python needs pexpect):

```sh
python3 tests/bash_style_input.py target/release/fish
python3 tests/bash_input_switch.py target/release/fish
python3 tests/bash_input_integration.py target/release/fish
python3 tests/bash_input_semantics.py target/release/fish /path/to/bash-5.3
python3 tests/bash_source_semantics.py target/release/fish /path/to/bash-5.3
python3 tests/bash_startup.py target/release/fish
python3 tests/bash_signal_integration.py target/release/fish
python3 tests/bash_script_semantics.py target/release/fish /path/to/bash-3.2.57
cargo test --lib --offline --locked
cargo test -p rash --offline --locked
```

Logs are `target/bash-style-release-*.log`, `target/bash-style-lib-tests-final.log`,
`target/bash-style-rash-tests.log`, `target/bash-style-existing-checks.log` and
`target/bash-style-existing-pty-checks.log`.

## Limits and follow-up boundary

The dialect deliberately rejects unsupported syntax, and completion still has a
narrower supported context than evaluation. Provider-driven buffer mutations
and syntax-bearing Fish abbreviations remain unexpanded/rejected with input
preserved. This change does not promise transparent translation of every Bash
program or every Fish editor customization.

Rash remains an incomplete implementation: stateful/function pipeline stages,
full subshell OS isolation, some syntax/builtins and arbitrary rc frameworks are
not certified. Its compatibility ledger remains
[rash-compatibility.md](rash-compatibility.md). No automatic fallback exists.

Rustty GUI rendering/navigation/detach-reattach, startup breakpoint entry, Linux
and the full GNU Bash corpus were not tested. Further dialect syntax/completion
coverage and Rash compatibility work can proceed independently; no additional
speculative bridge or prompt engine was introduced.

## Follow-up: namespace cleanup and Rash launcher defaults

Validated 2026-10-21 after removing the obsolete public `bash_session` command,
generated `$bash_jobs` variable and Rash's rejection-only `fish_input_mode`
pseudo-builtin. The internal Rash host remains. The `rash` function now reads
Bash startup files by default, inherits Fish login/private-history status, and
accepts `--login`, `--no-login` and `--no-startup` overrides. Direct
`fish --rash-shell` retains its explicit `--bash-startup` opt-in.

The tested release binary was built with
`cargo build --release --bins --offline --locked` from the final implementation
and reports `fish, version 4.9.3-162-g41b7daaff-dirty`. Its SHA-256 is
`0b532504630570a91c494d27533163623dee87147bc8fac20bc108d491900831`.
The dirty suffix reflects these then-uncommitted changes. The incremental build
took 29.55 seconds and again lacked Sphinx-generated man pages. The earlier
latency measurements above remain historical; no latency benchmark was rerun
for this follow-up.

- 16 focused Fish library tests passed (`cargo test --lib --offline --locked bash_`).
- 45 Rash unit tests passed (`cargo test -p rash --offline --locked`). Optional
  leaf-oracle tests remain ignored; the source PTY suite uses GNU Bash 5.3.
- Both CLI/script checks passed: `tests/checks/bash-input.fish` and
  `tests/checks/bash-script.fish`.
- Six PTY suites passed on the final release binary, with 32 grouped
  checkpoints: `rash_launch.py`, `bash_input_switch.py`, `bash_startup.py`,
  `bash_input_integration.py`, `bash_source_semantics.py` and
  `bash_signal_integration.py`.

The new launcher suite exercises 14 child-launch scenarios plus help and `--`:
login inheritance/overrides, unreadable-profile fallback, all three profile
precedence/fallback cases, no
implicit login `.bashrc`, startup/logout suppression, independent Fish `-N`,
`-C` values that resemble flags, and caller variables that resemble wrapper
locals. Private-mode coverage verifies startup still runs but saved Rash history
cannot be recalled or changed. Child exit status 23 and parent terminal ownership
are checked. The other suites retain source/state, jobs/signals, completion,
history, OSC 133/7 and low-level startup regression coverage; user-owned
`bash_jobs` is preserved and Rash no longer advertises `fish_input_mode`.

Two startup regressions failed on the previous release before correction:
unreadable `.bash_profile` now falls through to `.bash_login`, and interactive
Ctrl-D now runs `.bash_logout`. Both explicit exit and Ctrl-D run logout before
the `EXIT` trap, preserving or explicitly overriding exit status. Bash 3.2 and
5.3 oracles agree on that order and on skipping logout at noninteractive EOF.
Startup suppression and non-login EOF remain covered. Rash formatting, Fish
script formatting/syntax and diff whitespace checks passed; whole-workspace
`cargo fmt --all --check` reports an existing module-order difference in
`src/lib.rs`, outside this follow-up.

Reproduce the new suite with `python3 tests/rash_launch.py target/release/fish`
(Python needs pexpect). Logs are `target/rash-launch-fish-tests.log`,
`target/rash-launch-unit-tests.log` and `target/rash-launch-release-*.log`.
Personal startup files/frameworks and the Rustty GUI were not tested. The Rash
compatibility limits above still apply. Nothing was installed or pushed.

## Follow-up: shared Fish history, home prefixes and literal backticks

Validated 2026-10-21. Ordinary Fish and Bash-style Fish now share the existing
Fish history store, including custom session names, private memory and concurrent
merging. Rash stays separate. Older `bash-style-history/` files remain untouched
and are not automatically imported. Entries retain their text and suggestions
use the active syntax; reduced dialect suggestion matching is unchanged.

Completion projection now accepts only leading unquoted `~/` among the previously
unsupported tilde forms. Tests cover unchanged Git providers with a `~/` argument,
files with Unicode/metacharacters, directory suffixes, middle-of-buffer edits,
autosuggestions and preservation of expansion spelling. Backticks are literal
in dialect parsing, highlighting, movement and completion, including unmatched
and quoted backticks. `$(...)`, `$?` and `$$` retain their behavior. Rash's Bash
parser is unchanged. The history and backtick PTY regressions failed on the
previous release before the fixes.

The final release build used `cargo build --release --bins --offline --locked`.
SHA-256: `3f8f2b6a20704a6e50d8f858f660ccfab4547bb11b3741d8b757251e6bc1a49a`.
Version output retains the cached pre-squash identifier
`fish, version 4.9.3-162-g41b7daaff-dirty`; the executable contains this follow-up.
The incremental build took 29.50 seconds. Sphinx remains unavailable.

- 20 focused `bash_` library tests and the shared-history ID test passed.
- Eight standard Fish checks passed: basic behavior, history, commandline,
  completion, directory completion, source, Bash input and Bash scripts.
- Five final-release PTY suites passed with 27 grouped checkpoints:
  `bash_style_input.py`, `bash_input_switch.py`, `bash_input_integration.py`,
  `rash_launch.py` and `bash_source_semantics.py` (GNU Bash 5.3 oracle).
- Changed Rust files pass rustfmt; `git diff --check` passes.

Logs are `target/dialect-polish-*.log`; failing-before evidence is
`target/shared-fish-history-before.log` and `target/dialect-backtick-before.log`.
No latency benchmark, Rustty GUI or Windows trial was repeated. No installation,
push, history-file migration or change to personal configuration was performed.
