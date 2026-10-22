set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

# List recipes; builds must be requested by name.
[default]
default:
    @just --list

# Build first, then publish the native Windows release.
[windows]
release:
    @& ./build_tools/build-release.ps1

# Build release binaries with the current Unix toolchain.
[unix]
release:
    cargo build --locked --release --package fish --bins
