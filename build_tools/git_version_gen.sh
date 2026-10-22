#!/bin/sh
# Prefer a fish binary's build version, then Git, then the source package version.

set -e

# Find the fish directory as two levels up from script directory.
FISH_BASE_DIR="$( cd "$( dirname "$( dirname "$0" )" )" && pwd )"

# Older packaging callers pass --stdout; output always goes to stdout.
if [ "${1-}" = --stdout ]; then
    shift
fi

if [ "$#" -gt 0 ] && version=$(LC_ALL=C "$1" --version 2>/dev/null); then
    case "$version" in
        'fish, version '?*)
            printf '%s\n' "${version#fish, version }"
            exit
            ;;
    esac
fi

# A target binary may be unavailable or unrunnable while cross-compiling.
version=$(awk <"$FISH_BASE_DIR/Cargo.toml" -F'"' '$1 == "version = " { print $2; exit }')
if git_version=$(
    GIT_CEILING_DIRECTORIES=$FISH_BASE_DIR/.. \
        git -C "$FISH_BASE_DIR" describe --always --dirty 2>/dev/null); then
    if [ "$git_version" = "${git_version#"$version"}" ]; then
        version=$version-g$git_version
    else
        version=$git_version
    fi
fi
printf '%s\n' "$version"
