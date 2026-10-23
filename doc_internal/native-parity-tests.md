# Native and POSIX behavior checks

The executable tests `fish_behavior_parity` and `fish_dynamic_completions` run
identical Fish source with identical expected output and statuses on native
Windows and POSIX. Each test uses a private home, configuration, data, cache,
vendor, and temporary directory. They do not read or change user configuration.

Run them on either host after selecting the Rust toolchain for that host:

```sh
cargo test --locked -p fish --test fish_behavior_parity --test fish_dynamic_completions
```

The same targets can run with `--no-default-features`; they need neither gix nor
the packaged native utilities. Their PATH is empty. POSIX builds that omit
generated documentation can also use `--no-default-features --features git`.

The behavior checks exercise function scope, transparent helper functions,
sourced scripts, final-stage pipeline reads, explicit substitution records,
argument parsing, regular expressions, display width, and escaping. The reusable
fixture is `tests/support/fish_fixture.rs`.

Function and block redirections also have shared regression checks: an internal
pipeline replaces inherited input/output descriptors while retaining earlier
descriptor duplication and the commands' explicit redirections. This lets
helpers such as `alias --save name command >/dev/null` use their internal
read/source pipelines normally.

The dynamic completion checks actually query the bundled Cargo generators,
including binary, example, test, and target values; they also cover saved alias
autoload, nested subcommand forwarding, completion conditions, and deferred
generator execution. Deterministic Fish functions provide command responses,
including Cargo's stderr output and failure status when an option has no value.
Unexpected command queries fail the test. This isolates the completion scripts
and execution engine from changes in installed applications or toolchains.

`fish_rustup_completions` separately checks installed and installable Rustup
components and toolchains using the same scripts on both hosts. Its POSIX path
uses the system sorting utility; native Windows uses the packaged utility.

These checks complement `tests/native_completion_audit.py`, which only reports
loading and registration for the full bundled corpus. Neither deterministic
command responses nor successful source loading establishes that every dynamic
query works with every application version. Installed-tool smoke tests remain
necessary for those integrations, and terminal harnesses cover actual key input,
redrawing, completion paging, and cancellation.

The Test workflow runs the shared fixtures on Ubuntu and in the regular native
Windows suite. A separate Windows compatibility matrix checks every native
target and runs the shared fixtures, function-scope tests, and file-backed
`psub` and standalone-tool tests on the declared minimum Rust version and on
stable Rust with default features disabled. This keeps core startup behavior
working when gix and the packaged native utilities are omitted. The minimum
version comes from the repository's Rust toolchain action rather than a second
version in the workflow. The regular Windows job also runs the mocked release,
package, and installation rollback checks without changing an installed bundle.
