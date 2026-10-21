# Rash compatibility evidence and test plan

The selected implementation direction is a GNU Bash 3.2 source-guided Rust port,
with later Bash features implemented independently. The evaluator is now an
in-process Rust library linked into Fish. It is incomplete: translated leaf
routines and passing quote/status/parser tests do not establish full Bash
execution compatibility.

The shell comparison harness in `tests/rash_conformance.py` is independent of the
runtime. It requires an explicit oracle executable and candidate command prefix;
it never discovers or falls back to a sibling shell executable. The current Fish
runtime likewise has no external evaluator fallback. Its host interface receives
expanded arguments and redirections, not Bash source; Fish supplies jobs and I/O.

See [the validation record](rash-validation.md) for the frozen release binary,
measured latency and integrated results.

## Focused harness

```sh
python3 tests/rash_conformance_test.py
python3 tests/rash_conformance.py \
  --oracle /absolute/path/to/bash \
  --candidate /absolute/path/to/fish \
  --candidate-arg=-N --candidate-arg=--script-language=bash \
  --report target/rash-conformance.json
```

The current corpus has 20 cases. Eighteen are eligible with the GNU Bash 3.2.57
oracle; associative-array and nameref cases require Bash 4.0 and 4.3 respectively,
so a newer oracle is needed to run all 20. Eligibility is not a pass count:
the JSON report records actual pass/fail/skip/error results for each binary.

The initial 14-case corpus retained the seven source strings from
`bash_input_semantics.py` and four from `bash_script_semantics.py` verbatim. Three
original fixtures additionally observe sourced variables/functions/aliases/cwd,
exported state in a child, dynamic function scope, and filesystem effects.
Associative-array and nameref cases have explicit minimum oracle versions and
separate categories. An older oracle skips these cases with a recorded reason;
unsupported syntax failing identically under two old shells is not a modern
feature pass. There is no global requirement for a Bash 5.3 oracle.

Six further cases exercise assignment/conditional evaluation order, redirection
scope and shared input offsets, pipeline expansion isolation, arithmetic array
indices, `errexit` with and/or lists, and expanding heredocs. The parser/formatter
has 13 unit tests and an explicit-oracle suite with 39 syntax-acceptance cases
and 17 exact function-printing cases. Those component cases passed against the
source-built GNU Bash 3.2.57 oracle; they do not substitute for shell execution
or PTY/signal coverage.

The source-built Bash 3.2.57 expansion oracle additionally passed 306 comparisons:
53 word expansions, 31 arithmetic expressions, 28 printf cases, 182 extended-glob
pattern/value pairs and 12 error-status cases. These use the C locale; literal
Unicode bytes are preserved, but locale collation and range semantics remain a
separate compatibility limit.

The current debug binary also passes the isolated-script oracle suite, including
argument/raw-byte handling, stdio, exec/EXIT status, a copied Fish executable with
no companion or PATH tools, 18 PID/group HUP/TERM/INT disposition combinations,
trapped reads with stdin held open, partial-read resumption, and parent traps
through subshell/command substitution. These focused checks do not establish all
subshell signal semantics or arbitrary signal-handler isolation.

Each side runs in a fresh copy of the same workspace path, under isolated HOME
and XDG paths. Results compare exact stdout bytes, stderr bytes, exit or signal
status, and changed filesystem entries, including contents, permissions and
symlink targets. No whitespace, error-message or path normalization is applied.
The harness seeds an identical empty Fish config/cache directory layout on both
sides before taking the initial snapshot. This accommodates Fish's normal first-run
directory creation even under `-N`; every later change to these paths remains part
of the artifact comparison. Scripts explicitly print the state being tested; the harness does not append a
snapshot command that could alter status, traps or evaluation semantics.

Each process group has a timeout and output limit. Stdio uses temporary files,
so inherited pipes cannot make collection wait forever. Cleanup kills remaining
members of that group. It does not contain a program that deliberately creates
another session; job-control suites that exercise that behavior still need their
own PTY cleanup or an isolated test environment.

Reports retain raw byte evidence and executable hashes, report the oracle's
actual version, and count pass/fail/skip/error outcomes per category. An oracle
timeout or anomalous GNU fixture is an oracle failure, never a candidate pass.
The self-tests deliberately supply candidates with wrong output, status and
files, plus hanging/noisy programs and a driver that hides a failing diff behind
`exit 0`. They also check refusal of legacy drivers with fixed temporary paths
and provenance changes when a recursive fixture changes.

Calibration performed on this host:

| Run | Result | Meaning |
| --- | --- | --- |
| Harness self-tests | 7 tests pass | Comparator and failure reporting work on these checks |
| System Bash 3.2.57 against itself | 12 matches, 2 modern cases skipped | Harness plumbing only |
| Native Bash 5.3.0 against itself | 14 matches | Harness plumbing only |
| Intentionally empty candidate against Bash | Fails with exit status 1 | A missing implementation is not accepted |
| External Bash 5.3 `run-ifs` and `run-heredoc`, Bash against itself | Both match | Selected GNU-driver integration only |

The JSON artifacts are `target/rash-harness-bash32.json`,
`target/rash-harness-bash53.json`, `target/rash-harness-negative.json` and
`target/rash-harness-gnu53.json`. These results are **not Rash conformance results**.

## GNU regression tests

The inspected GNU Bash 5.3 source is at
`/private/tmp/fish-native-bash-probe/bash-5.3`. Its test directory has 88 `run-*`
files, 83 `.tests` files, 451 `.sub` files and 87 expected-output `.right` files.
These are file counts, not independent assertions. Bash 3.2 needs a separately
pinned matching suite; the 5.3 inventory must not be presented as its baseline.
The pinned source preparation and license record are in
[`build_tools/rash/README.md`](../build_tools/rash/README.md).

Bash 3.2's individual drivers use a fixed `/tmp/xx`, and `run-all` removes it.
The harness refuses drivers naming fixed `/tmp/` paths before executing either
shell. These legacy fixtures need an isolated OS environment or a separately
reviewed fixture-transport adaptation; no Bash 3.2 GNU-driver pass is claimed.
The driver check is not a general filesystem sandbox. External suites remain
trusted executable code, including recursively invoked fixtures.

`tests/run-all` prints anomalies and unconditionally exits zero. Checking only
`make tests`' exit code can therefore claim success despite emitted diffs. The
new harness runs selected individual drivers and inserts a test-only `diff`
wrapper into PATH that records every actual diff result, including failures
whose status a driver discards. It also detects normal-format diff output.

GNU fixtures stay external; they are copied into temporary test workspaces at
runtime, not vendored into Fish. Reports store changed artifacts, not a copy of
every unchanged source fixture. Reports identify selected drivers and helpers by
SHA256 and hash the full fixture tree, since drivers can read other fixtures.
These hashes identify inputs; they do not automatically establish that the
suite matches the selected oracle version. The Bash 5.3 suite is GPL-3.0-or-later and is not
a source for a GPL-2.0-only-compatible port. The selected Bash 3.2 baseline and
its notices need separate provenance tracking.

```sh
# Build these test utilities in a configured matching Bash build directory.
make recho zecho printenv xcase
python3 tests/rash_conformance.py \
  --oracle /absolute/path/to/bash --candidate /absolute/path/to/candidate \
  --case pipeline-status \
  --gnu-tests /absolute/path/to/bash/tests \
  --gnu-helpers /absolute/path/to/bash-build \
  --gnu-case run-ifs --gnu-case run-heredoc \
  --report target/rash-gnu.json
```

GNU scripts recursively invoke `THIS_SH` with filenames, `-c`, options and stdin.
The launcher forwards argv without translating shell source. Consequently,
`fish --script-language=bash` can exercise file-only fixtures but is not a
complete Bash CLI adapter: ordinary `fish -c` deliberately retains Fish syntax.
A future Rash test driver must implement the invocation paths it claims to test.
Neither a wrapper that rewrites programs nor one that delegates to Bash is proof
of the port.

Expected outputs depend on platform utilities, locale availability, job-control
configuration, signal numbering and selected Bash version. The GNU drivers
already note whitespace differences in `read` tests and platform warnings in
`jobs` tests. Run each fixture against the selected native oracle on the same
host first. Preserve oracle failures separately; do not silently update golden
files or ignore stderr to make a candidate appear compatible.

## Compatibility matrix

Every row needs recorded case results and unresolved differences for the selected
version. None currently has demonstrated complete Rash coverage.

| Area | Required observations | Relevant external GNU families |
| --- | --- | --- |
| Parsing and aliases | Grammar, incomplete input, alias timing, source locations, errors | `parser`, `alias`, `errors`, quote families |
| Expansion | Expansion order, IFS empties, quote removal, globbing, substitution side effects | `ifs`, `dollars`, `comsub`, `exp-tests`, `extglob` |
| Variables and functions | Scope, attributes, readonly/export behavior, call stacks and return status | `varenv`, `attr`, `func`, `array`, `dbg-support` |
| Control flow | Short-circuiting, loops, case, errexit/ERR interactions, arithmetic | `case`, `arith`, `arith-for`, `set-e`, `cond` |
| Redirection and processes | Ordered fd changes, failed redirects, exec/PID identity, pipes and substitutions | `redir`, `vredir`, `execscript`, `procsub` |
| Signals and jobs | Interruptions, process groups, stopped pipelines, wait/disown, trap inheritance | `jobs`, `trap`, plus real controlling-terminal PTYs |
| Startup and sourcing | Invocation/login rules, source argv/return, options and traps surviving source | `invocation`, `builtins`, plus existing Fish startup/source PTYs |
| Builtins | Options, errors, stdout/stderr, state changes, file effects | `builtins`, `printf`, `read`, `getopts`, `test`, `type` |
| Locale and raw bytes | Non-UTF-8 paths/arguments, Unicode, locale-sensitive matching | `intl`, `glob-bracket`, plus byte-preserving fixtures |
| Modern extensions | Version-specific arrays/namerefs, coproc, mapfile, newer options | Separate oracle-version categories; do not count as Bash 3.2 baseline |
| Fish embedding | Fish-code language, hooks, history, completion projection, switching, tty/OSC markers | Existing Fish regressions and Bash integration PTYs |
| No backend delegation | Builtins run in Fish's process; only requested subprocess semantics create children | Process/API inspection and integration tests, not output comparisons alone |

Source size illustrates the work beyond lexical parsing: the inspected Bash 5.3
`parse.y` has 7,530 lines, `execute_cmd.c` 6,409, `subst.c` 13,264,
`variables.c` 6,521, `jobs.c` 5,480 and 43 builtin definition files total 19,106
lines. These figures are not an estimate of required Rust lines or port progress.

## Brush as independent evidence

The inspected Brush checkout is clean at
`fc401d784d93aea0e956fff7d9755199a8bf834b` and declares Rust 1.88. Its actual
`compat` corpus has 121 YAML files and 3,134 declared cases: 744 carry
`known_failure`, 37 carry `skip`, 526 ignore stderr and 32 request a PTY. These
flags overlap and platform/version filters change eligibility; subtracting them
does not produce a tested pass count. The README's “2,500+” and compatibility
page's “~470 known failures” are less current than this source inventory.

The compatibility runner is `brush-shell/tests/compat_tests.rs`, with explicit
`--bash-path` and `--brush-path`. `integration_tests.rs` instead runs Brush-only
expectations. The compatibility runner compares output/status/files, records
known failures, and intentionally treats an unexpected pass as a failure to
update the ledger. Its output is UTF-8-lossy and some cases ignore differences;
keep independent raw-byte and signal tests for the Fish port.

Brush's documented gaps include `select`, `wait -n`, `disown`, `logout`, reliable
`$!`, signal traps and several expansion/alias edges. Its test corpus is useful
independent evidence, not proof that embedding it would be a faithful complete
Bash port. Harness defaults also include Brush-specific command-line flags; they
cannot be passed unchanged to Fish or a new Rash driver.

## Integration regression tests

| File | Coverage and oracle boundary |
| --- | --- |
| `bash_input_semantics.py` | Interactive differential evaluation and HUP/disown checks with an explicit Bash oracle |
| `bash_source_semantics.py` | Source comparison against an explicit oracle; candidate remains the Fish PTY |
| `bash_script_semantics.py` | Explicit oracle path; copied Fish without companion/PATH tools, argv, stdio, exit and PID/group signal checks |
| `bash_input_switch.py` | Mode boundaries, shared jobs, private history and state-conflict coverage |
| `bash_input_integration.py`, `bash_startup.py`, `bash_input_tmuxx.py` | Retained behavioral checks for Fish integration and startup; no backend installation requirement |
| `bash_input_bench.py` | Compare equivalent release builds; earlier native-backend numbers are not Rust-port performance evidence |
| `bash_input_pty_probe.py` | Keep labeled as a historical rejected-worker experiment, not port acceptance coverage |

Leaf-component tests and the shell-level harness should coexist. Promote a
feature only when its actual execution path is wired and observed; list all
unsupported, failing, skipped and untested cases. A successful selected suite is
evidence for those cases, never a certificate of full Bash compatibility.
