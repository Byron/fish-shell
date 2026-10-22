The shared integration follows gitoxide's `main` branch. `Cargo.lock` keeps builds
reproducible; use `cargo update -p gix` to advance it, then run the Git query and
native completion checks before committing the new lockfile. Both the default
`gix` feature and its underlying `git` feature select this same dependency; the
feature name does not update the locked revision automatically.

The current lockfile uses gix 0.89.0 at
[`b8ef9ea94ba69302981c56e16d52b696a00d4c98`](https://github.com/GitoxideLabs/gitoxide/commit/b8ef9ea94ba69302981c56e16d52b696a00d4c98).
It requires Rust 1.88, reflected in the workspace minimum.
These upstream changes would simplify the integration and improve correctness.
The POSIX integration uses gix wherever its APIs cover the query. Errors returned
by gix, including unsupported repository storage, never trigger a Git CLI retry.
The native Windows integration remains gix-only.

With the feature enabled, remaining CLI uses are common/object-directory and
serialized config environment overrides (missing an explicit session-snapshot
opening API), Git's help/config-key/tool catalogs, and packaging before a runnable
fish binary is available. Disabling the feature intentionally uses the CLI for all
POSIX queries; this is a compatibility mode rather than an upstream gap.
Non-UTF-8 query arguments are currently rejected by fish's private string protocol.

1. **Preserve repository isolation when checking submodules (bug).**
   Non-parallel [`BuiltinSubmoduleStatus::status`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/status/index_worktree.rs#L229)
   reopens with `crate::open`, losing the caller's permissions and in-memory configuration.
   Preserve those across reopening and test equivalent behavior with and without parallel support.
   Fish currently supplies its own isolated submodule status provider.

2. **Bind status rename settings to the status section (bug).**
   [`Status::RENAMES` and `Status::RENAME_LIMIT`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/config/tree/sections/status.rs#L14)
   point at `Tree::MERGE`, so configured status reads the wrong keys.
   Fix the constants and test `status.renames=true` with `merge.renames=false`.
   Fish reads `status.*` explicitly with gix's typed converters, then falls back to `diff.*`.

3. **Let per-submodule ignore settings override the global default (bug).**
   [`BuiltinSubmoduleStatus::status`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/status/index_worktree.rs#L243)
   gives `diff.ignoreSubmodules` precedence over `submodule.<name>.ignore`.
   With a modified submodule, `diff.ignoreSubmodules=all`, and
   `submodule.sub.ignore=none`, Git reports the modification. Fish initially copied
   gix's precedence and incorrectly reported nothing in the same fixture.
   Its status provider now uses CLI override, then explicit per-submodule setting,
   then the global default, with regression coverage for each level.

4. **Propagate status iterator errors from `Repository::is_dirty` (bug candidate).**
   Source inspection of [`is_dirty`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/status/mod.rs#L170)
   shows `take_while(Result::is_ok).next().is_some()`: if the first status item is
   an error, the method returns `Ok(false)`. Return that error instead of classifying
   the repository as clean. This finding has not been reproduced with a failing
   repository fixture. Fish consumes status results directly and propagates errors.

5. **Allow long reflog records in the convenience iterator (bug).**
   [`Platform::rev`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix-ref/src/store/file/log/iter.rs#L51)
   always resizes its buffer to 4096 bytes. A valid 5000-byte message fails there while
   `Platform::all` reads it successfully. Grow the buffer or expose its size as an option.
   Fish uses the lower-level reverse iterator with a bounded 1 MiB buffer and tests a 5 KiB message.

6. **Peel nested annotated tags when describing commits (bug).**
   The default annotated-tag path in [`Commit::describe`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/commit/mod.rs#L151)
   peels only one tag object and assumes a direct reference. For `outer -> inner tag -> commit`,
   Git describes the commit as `outer`, while gix's default returns no description.
   Follow symbolic references and peel recursively while retaining the outer tag's date and name.
   Fish uses this API directly and returns its result without retrying via Git.

7. **Accept an explicit cwd and environment snapshot when opening repositories (API).**
   [`open/repository.rs`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/open/repository.rs#L109)
   obtains cwd and repository environment overrides from process-global state.
   A shell needs its current exported variables and repeated `-C` arguments without changing
   process state. An explicit context would replace fish's configuration loader and individual
   environment mappings, and avoid Git CLI fallback for currently unsupported
   object-store/common-directory overrides.

8. **Provide a policy that forbids external processes throughout a query (API).**
   Isolated opening still permits repository-local configuration, including executable
   [clean/process filters](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/filter.rs#L225).
   A process policy would help callers distinguish required normalization helpers
   from optional executable drivers. Fish uses gix's normalization filter APIs
   directly and reports their errors without retrying via Git.

9. **Expose rename similarity in index changes (API).**
   [`ChangeRef::Rewrite`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix-diff/src/index/mod.rs#L72)
   lacks similarity, although the [conversion has diff statistics](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix-diff/src/index/function.rs#L163).
   Retaining them would allow exact porcelain-v2 scores without another diff.
   Fish's private completion protocol currently uses 100 for identical blobs and 50 otherwise;
   its consumers only inspect status and paths.

10. **Add containing-revision names and cooperative cancellation (API).**
   A `name-rev`/`describe --contains` API would replace fish's descendant naming traversal.
   Cancellation hooks in [revision description](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix-revision/src/describe.rs#L197)
   and [hidden-frontier traversal](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix-traverse/src/commit/simple.rs#L483)
   would let interactive callers enforce a budget during library work. Fish can currently check
   its 500 ms history/reference budget only between synchronous library calls.

11. **Invalidate cached submodule configuration after repository config changes (bug).**
    [`Repository::modules`](https://github.com/GitoxideLabs/gitoxide/blob/b8ef9ea94ba69302981c56e16d52b696a00d4c98/gix/src/repository/submodule.rs#L43)
    caches by `.gitmodules` modification time, but the cached value already includes
    repository config overrides. Repository clones share that cache. Reading modules,
    setting `submodule.sub.ignore=none` through `config_snapshot_mut()`, then reading
    again can return the old ignore setting. Track the config generation or overlay
    repository overrides on each access. Fish reads worktree module settings afresh;
    normal queries configure a newly opened repository before reading modules.
