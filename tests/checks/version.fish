#RUN: fish=%fish %fish %s

# Exact tag:
# 1.2.3
# Git version:
# 1.2.3-42-gdeadbeef
# Git version when no tags are present. Distros should probably not package it like this.
# 1.2.3-gdeadbeef
# If the git commit hash is part of the version identifier, it may have a `-dirty` suffix.

$fish -v
# CHECK: fish, version {{\d+\.\d+\.\d+((-\d+)?-g[0-9a-f]{7,}(-dirty)?)?$}}

set -l workspace_root (status dirname)/../..
$workspace_root/build_tools/git_version_gen.sh
# CHECK: {{\d+\.\d+\.\d+((-\d+)?-g[0-9a-f]{7,}(-dirty)?)?$}}

set -l version_script $workspace_root/build_tools/git_version_gen.sh
set -l source_version ($version_script)
set -l package_version (string match --regex --groups-only '^version = "([^"]+)"$' <$workspace_root/Cargo.toml)[1]
set -l build_version (LC_ALL=C $fish --version | string replace 'fish, version ' '')
test "$($version_script $fish)" = "$build_version"; or exit 1
test "$($version_script --stdout)" = "$source_version"; or exit 1
test "$($version_script /nonexistent/fish)" = "$source_version"; or exit 1

set -l tmp (mktemp -d)
set -lx VERSION_GIT_TRACE "$tmp/git-invoked"
printf '%s\n' '#!/bin/sh' ': > "$VERSION_GIT_TRACE"' 'printf "%s\n" "$VERSION_GIT_OUTPUT"' 'exit "$VERSION_GIT_STATUS"' >"$tmp/git"
printf '%s\n' '#!/bin/sh' 'printf "fish, version 1.2.3-gabcdef0\\n"' >"$tmp/fish with spaces"
chmod +x "$tmp/git" "$tmp/fish with spaces"
begin
    set -lx PATH "$tmp" $PATH
    set -lx VERSION_GIT_STATUS 0
    set -lx VERSION_GIT_OUTPUT abcdef0
    test "$($version_script "$tmp/fish with spaces")" = 1.2.3-gabcdef0; or exit 1
    test ! -e "$VERSION_GIT_TRACE"; or exit 1

    test "$($version_script)" = "$package_version-gabcdef0"; or exit 1
    test -e "$VERSION_GIT_TRACE"; or exit 1
    test "$($version_script /nonexistent/fish)" = "$package_version-gabcdef0"; or exit 1
    test "$($version_script --stdout)" = "$package_version-gabcdef0"; or exit 1

    set VERSION_GIT_OUTPUT "$package_version-42-gabcdef0"
    test "$($version_script)" = "$VERSION_GIT_OUTPUT"; or exit 1
    set VERSION_GIT_STATUS 91
    test "$($version_script)" = "$package_version"; or exit 1
end
echo 'version script checks passed'
# CHECK: version script checks passed
