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

# Build and package a native bundle at an explicit ZIP destination.
[windows]
package $destination:
    @& ./build_tools/package-windows.ps1 -Destination $env:destination

# Install an extracted native bundle without changing PATH or configuration.
[windows]
install $bundle $destination:
    @& ./build_tools/install-windows.ps1 -Bundle $env:bundle -Destination $env:destination
