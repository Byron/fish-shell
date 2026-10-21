# Rash release validation

> Historical measurements for the former shared Rash/Fish interactive session.
> The current Bash-style Fish dialect and isolated Rash shell are validated in
> [bash-style-input-validation.md](bash-style-input-validation.md). Commands below
> describe the measured revision and are not the current invocation contract.

Measured on 2026-10-21. The integrated evaluator is an experimental in-process
Rust implementation. These results establish the behavior of the listed cases;
they do not establish full Bash compatibility or arbitrary `.bashrc` support.
See [compatibility limits](rash-compatibility.md) and the
[invocation, state and Rustty/Tmuxx guide](bash-input-feasibility.md).

## Binaries and environment

Both binaries were built with `cargo build --release --bins --offline --locked`,
default features, release LTO and overflow checks. Sphinx was unavailable for
both builds, so neither embeds generated man pages. Debug latency is not used
for this comparison.

| Item | Value |
| --- | --- |
| Host | macOS 27.0.1, build 26A434, arm64 |
| Rust | rustc 1.99.0 (b940084d7 2026-09-28) |
| Cargo | 1.99.0 (5f94df478 2026-08-27) |
| Git used by completion | 2.54.0 (Apple Git-157) |
| PTY harness | Python 3.14.7, pexpect 4.9.0 |
| Tmuxx | Rustty-bundled `tmuxx 0.1.0` |
| Source oracle | GNU Bash 3.2.57(2)-release, arm-apple-darwin27.0.0 |
| Modern behavior oracle | GNU Bash 5.3.0(1)-release, aarch64-apple-darwin27.0.0 |

The baseline is `target/release/fish-baseline`, built from the exported source
archive of revision `384447640af76e494212f05ca5f18bdb51b6ec68` at
`/private/tmp/fish-release-baseline-0zbllick`. It reports `fish, version 4.9.3`.
Its SHA-256 is
`72de0ba404a26ab36639c02ea8136f5dc2ea940554d66fd91e698a1af636b136`.
Git blob comparisons verified every present runtime/build source against that
revision. The archive omits 25 `export-ignore` paths and normalizes the
`fish.png` symlink by removing a redundant leading `./`.

The measured candidate is `target/release/fish`, built at runtime integration
revision `659fff6add69e90fcea9999943b8180a49ffaa43`. It reports
`fish, version 4.9.3-158-g659fff6ad-dirty`. Its SHA-256 is
`072201731117580f4c9d577d267592d28b2249005490c9ae1b02b0b3d910792d`.
Documentation and evidence changes were uncommitted when it was built; recorded
runtime source hashes did not change during the build or measurement. The
cached version generator was refreshed by touching `build.rs` without changing
its contents. The final incremental release build took 27.41 seconds; this is
not a clean-build benchmark.

Local evidence is retained in
[`target/rash-release-metadata.json`](../target/rash-release-metadata.json),
including source hashes, binary hashes, commands, versions and test results.
Files under `target/` are local artifacts, not distributed as part of the patch
queue. Earlier measurements and the failing logout test are retained separately
with `initial`, `stale-version` or `before-logout-fix` in their artifact names.

## Release latency

Run:

```sh
/tmp/fish-bash-probe-venv/bin/python tests/bash_input_bench.py \
  target/release/fish-baseline target/release/fish \
  > target/rash-release-bench.jsonl
```

All cells show **p50 / p95 milliseconds**. Baseline is ordinary Fish; the other
columns use the same patched release binary. Bash mode uses
`--interactive-language=bash`; Bash + rc additionally uses `--bash-startup`.

| Workload | Baseline Fish | Patched Fish | Bash input | Bash input + small rc |
| --- | ---: | ---: | ---: | ---: |
| Configured startup to prompt | 37.112 / 39.509 | 38.266 / 40.653 | 36.112 / 41.265 | 37.010 / 40.719 |
| `true` to next prompt | 1.216 / 1.533 | 0.962 / 1.106 | 0.986 / 1.271 | 0.937 / 1.344 |
| Tab: `git --ver` | 12.302 / 12.809 | 12.189 / 12.704 | 12.238 / 12.686 | 12.128 / 13.347 |
| Tab: `git -C "$repo" switch topic-` | 27.059 / 28.711 | 27.476 / 29.983 | 27.186 / 27.814 | 27.274 / 31.047 |

Startup has 15 samples per case in interleaved, rotating order; prompt return
has 30; each completion has 20 warm samples after excluding its first request.
The fixture uses isolated HOME/XDG directories, a fixed `PROBE>` prompt, and a
local Git repository with one commit and a branch, in a path containing spaces
and Unicode. The rc file adds one export, alias and function. Raw samples are in
[`target/rash-release-bench.jsonl`](../target/rash-release-bench.jsonl).

Times include PTY transport and the keybinding used to observe the edit buffer.
They are warm-cache observations on one macOS host. Startup order rotates, but
prompt/completion case order and unrelated host activity can still influence
results. No arbitrary user prompt framework, large repository, network
filesystem or Linux workload was measured. These samples show no large latency
increase for the selected workloads; small differences should not be read as
proven speedups or equivalent performance in other workloads.

## Evaluation and terminal checks

The following four suites were run after timing, on the exact candidate hash
above. All passed: **23 printed checkpoints across four suites**. A checkpoint
can contain multiple assertions and scenarios; it is not a language coverage
percentage.

| Suite | Checkpoints | Coverage | Local log |
| --- | ---: | --- | --- |
| `bash_startup.py` | 8 | Default-off startup, Fish config order, rc/profile selection, POSIX ENV, aliases/extglob, Git Tab, prompt hooks, history boundary, logout/status, slow/interrupted startup, startup exit/exec | [startup](../target/rash-release-startup.log) |
| `bash_source_semantics.py` | 7 | Retained variables/functions/aliases/options/cwd, nested source/dot and positional args, redirects, traps/return, parsing, stopped jobs, Fish source language, source exit | [source](../target/rash-release-source.log) |
| `bash_script_semantics.py` | 7 | Script argv/raw bytes/stdio/status, Fish configuration, copied executable without a backend or PATH tools, exec/EXIT, process/group signals, interrupted and partial reads, parent traps | [script](../target/rash-release-script.log) |
| `bash_signal_integration.py` | 1 | Fish listeners with Bash traps, mode suspension/restoration, child ignored dispositions, original job text, Ctrl-C in builtin loops | [signals](../target/rash-release-signal.log) |

The release startup check caught a `.bash_logout` regression: the pending-exit
check prevented the logout file from running. The core correction and a focused
unit regression are included, and all four release suites were rerun afterward.

Reproduction commands (the Python environment must provide pexpect):

```sh
python3 tests/bash_startup.py target/release/fish
python3 tests/bash_source_semantics.py target/release/fish \
  /private/tmp/fish-native-bash-probe/bash-5.3/bash
python3 tests/bash_script_semantics.py target/release/fish \
  /private/tmp/fish-rash-prepared/build/bash
python3 tests/bash_signal_integration.py target/release/fish
```

The focused 20-case shell corpus was also rerun on the exact final release
binary. Bash 5.3 has **20 passes**. Bash 3.2 has **17 passes, one difference and
two explicit version skips**. The difference is assignment evaluation order:
with `a=outer`, a prefix `a=inner b=$a f` exposes `outer` through `b` in Bash 3.2,
and `inner` in Bash 5.3 and Rash. The report keeps this as a failing comparison
against 3.2; it is not silently normalized. Associative arrays and namerefs are
skipped only when the oracle lacks their required version.

```sh
python3 tests/rash_conformance.py \
  --oracle /private/tmp/fish-rash-prepared/build/bash \
  --candidate target/release/fish \
  --candidate-arg=-N --candidate-arg=--script-language=bash \
  --report target/rash-conformance.json
python3 tests/rash_conformance.py \
  --oracle /private/tmp/fish-native-bash-probe/bash-5.3/bash \
  --candidate target/release/fish \
  --candidate-arg=-N --candidate-arg=--script-language=bash \
  --report target/rash-conformance-modern.json
```

Reports retain executable/corpus hashes, exact output bytes, status and filesystem
effects: [Bash 3.2](../target/rash-conformance.json),
[Bash 5.3](../target/rash-conformance-modern.json). The first command exits 1 for
the documented difference. Neither run exercises the entire GNU Bash test suite.

The native Tmuxx startup/OSC/job trial passed on the final release binary:

```sh
python3 tests/bash_input_tmuxx.py target/release/fish \
  /Applications/Rustty.app/Contents/MacOS/tmuxx --bash-startup
```

It exercises the Rustty-bundled native Tmuxx endpoint, retained state, startup,
OSC 133/7, stop/foreground/interrupt and mode switching. It is a headless transport
test; it does not certify Rustty GUI rendering. The two Fish driver checks
`tests/checks/bash-input.fish` and `tests/checks/bash-script.fish` also passed
against the final release build.

## Retained regression coverage

Final Rust checks passed: **45 Rash unit tests**, **192 Fish library tests**, and
seven explicitly enabled oracle tests containing **362 comparisons** (306
expansion/arithmetic/printf/extglob/status, 39 syntax acceptance and 17 function
printing). Rash Clippy passed with warnings denied.

```sh
cargo test -p rash --offline --locked
cargo test --no-default-features --lib --locked --offline
RASH_BASH_ORACLE=/private/tmp/fish-rash-prepared/build/bash \
  cargo test -p rash --test syntax_oracle --test expansion_oracle \
    --locked --offline -- --ignored
cargo clippy -p rash --all-targets --offline --locked -- -D warnings
```

Whole-Fish Clippy with warnings denied still reports preexisting warnings in
earlier queue code. New Rash and newly introduced Fish adapter warnings were
corrected; the whole repository is not claimed Clippy-clean.

The Bash integration, dynamic mode-switch, shared-signal and seven interactive
oracle workloads passed during runtime integration using the debug binary.
They cover Git descriptions/dynamic candidates/insertion, source-preserving
completion, suggestions/history, terminal ownership and state across mode
switches. The final release script/source/startup/signal tests above add release
coverage; no debug latency result is used.

The existing Fish regression selection passed **25/25, zero skipped** after the
signal integration fixes, using the debug binary. These checks required ordinary
PTY and `/bin/ps` access outside the restricted test sandbox. No disk log was
retained for that run or the 192-library-test run; their complete successful
outputs were retained in the task's tool results.

```sh
PATH=/tmp/fish-bash-probe-venv/bin:$PATH \
  /tmp/fish-bash-probe-venv/bin/python tests/test_driver.py \
    --max-concurrency 4 target/debug \
    tests/checks/signal.fish tests/checks/self-signal-usr1.fish \
    tests/checks/trap.fish tests/checks/trap_print.fish tests/checks/exec.fish \
    tests/checks/jobs.fish tests/checks/jobs-are-escaped.fish tests/checks/job-ids.fish \
    tests/checks/job-control-noninteractive.fish tests/checks/job-control-not-a-tty.fish \
    tests/checks/fg.fish tests/checks/bg.fish tests/checks/wait.fish \
    tests/checks/disown.fish tests/checks/disown-parent.fish tests/checks/source.fish \
    tests/checks/cmdsub.fish tests/checks/cmdsub-limit.fish tests/checks/deep-cmdsub.fish \
    tests/checks/psub.fish tests/pexpects/signals.py tests/pexpects/fg.py \
    tests/pexpects/wait.py tests/pexpects/job_summary.py tests/pexpects/postexec.py
```

Signal compatibility remains scoped: idle-prompt Bash traps run at the next Bash
submission; virtual subshells do not provide OS process isolation; their custom
OS signal handlers and job services are rejected. Parent signals can remain
pending until a virtual scope returns. Passing the bounded trap/read cases does
not remove these limitations.
