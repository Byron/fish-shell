# Rash port source and test oracle

The direct Rust port starts from **GNU Bash 3.2.57**: the original Bash 3.2
archive plus all 57 official patches, applied in order. `sources.json` pins
every download by SHA256 and also pins the upstream files used by the first
Rust components. No Bash C runtime is linked into Rash or Fish by these tools.

Prepare the reference source outside the checkout:

```sh
python3 build_tools/rash/fetch.py /tmp/rash-bash32
```

Build the separate C executable used only as a differential test oracle:

```sh
python3 build_tools/rash/fetch.py /tmp/rash-bash32 --build
/tmp/rash-bash32/build/bash --noprofile --norc --version
```

The source is `/tmp/rash-bash32/bash-3.2`. The build is separate and contains
generated headers and `syntax.c`. Downloads are cached and rechecked on every
run. Existing recognized source and build directories are reused; an
unrecognized source directory is refused. Adding source-file hashes or other
provenance metadata preserves those caches and validates the added hashes.
Build and patch diagnostics are in
`prepare.log`, `configure.log`, and `build.log` beside them. The script needs
Python 3, `patch`, and, for `--build`, a C compiler and `make`. It does not
install anything or run as part of the ordinary Fish build. The compiler flags
select the older C dialect and accept diagnostics from this historical source;
they do not apply to the Rust port.

Run the downloader's offline integrity checks with
`python3 build_tools/rash/test_fetch.py`.

## Provenance and license

- Archive: <https://ftp.gnu.org/gnu/bash/bash-3.2.tar.gz>
- Official patches: <https://ftp.gnu.org/gnu/bash/bash-3.2-patches/>,
  `bash32-001` through `bash32-057`.
- GNU public keyring: <https://ftp.gnu.org/gnu/gnu-keyring.gpg>.

On 2026-10-21, the detached `.sig` signatures for the archive and all 57 patches
were verified using GnuPG and GNU's public keyring. All 58 produced `VALIDSIG`
for fingerprint `7C0135FB088AAF6C66C650B9BB5869F064EA74AB`. This is a recorded
verification of the pinned inputs; `fetch.py` verifies their SHA256 hashes,
without requiring GnuPG or importing keys into a developer's keyring. For an
independent signature check, download the corresponding `.sig` files and run
`gpg --no-default-keyring --keyring /path/to/gnu-keyring.gpg --verify FILE.sig FILE`.

The Bash 3.2 source headers grant **GPL version 2 or any later version**;
`COPYING` contains GPL version 2. None of the 57 patches changes `COPYING` or
that license grant. The last patch identifies itself as Bash release 3.2,
patch `bash32-057`, dated 2014-10-04. The GPL version 2 option permits the
source-derived Rust components to remain compatible with Fish's
`GPL-2.0-only` license. Preserve upstream copyright notices in those components
and identify their modifications and source paths.

GNU Bash 5.3 is GPL version 3 or later and is **not** a source donor for this
port. Newer behavior must be implemented independently from documented and
observed behavior, with separately written regression cases. A newer Bash
executable can serve as a black-box oracle. Neither the existence of a Rust
component nor agreement with selected cases establishes complete Bash
compatibility.

`lib/sh/shquote.c` and `syntax.h` are unchanged between 3.2 and 3.2.57.
`jobs.c` includes patches 007, 018, and 035; use the fully patched tree for
process and pipeline status work. Source file hashes in `sources.json` refer
to that tree, before configuration. `syntax.c` is generated from `mksyntax.c`
and `syntax.h`, rather than shipped in the original archive.
