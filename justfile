set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

# Build first, then publish the release for an explicit Rust target.
[windows]
release $target:
    @& ./build_tools/build-release.ps1 -Target $env:target

# Build release binaries for an explicit Rust target.
[unix]
release $target:
    cargo build --locked --release --package fish --bins --target "$target"
