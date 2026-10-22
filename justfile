set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

# Build native release binaries, preserving existing Windows executables.
[windows]
release:
    @& ./build_tools/build-release.ps1

# Build release binaries with the current Unix toolchain.
[unix]
release:
    cargo build --locked --release --package fish --bins
