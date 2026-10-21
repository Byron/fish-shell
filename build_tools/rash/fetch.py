#!/usr/bin/env python3
"""Prepare the pinned GNU Bash 3.2.57 source and optional external test oracle."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request


def verify(path, expected):
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise ValueError(f"SHA256 mismatch: {path}")


def fetch(cache, name, url, sha256):
    path = cache / name
    if not path.exists():
        with urllib.request.urlopen(url, timeout=30) as response:
            data = response.read()
        if hashlib.sha256(data).hexdigest() != sha256:
            raise ValueError(f"SHA256 mismatch: {url}")
        path.write_bytes(data)
    verify(path, sha256)
    return path


def prepare(directory):
    manifest_path = Path(__file__).with_name("sources.json")
    manifest = json.loads(manifest_path.read_text())
    source_inputs = [(manifest["archive"]["url"], manifest["archive"]["sha256"])]
    source_inputs += [
        (manifest["patch_base_url"] + name, digest)
        for name, digest in sorted(manifest["patches"].items())
    ]
    stamp = hashlib.sha256(json.dumps(source_inputs).encode()).hexdigest()
    cache = directory / "downloads"
    cache.mkdir(parents=True, exist_ok=True)
    archive = fetch(cache, **manifest["archive"])
    patches = [
        fetch(cache, name, manifest["patch_base_url"] + name, digest)
        for name, digest in sorted(manifest["patches"].items())
    ]
    source = directory / "bash-3.2"
    if source.exists():
        marker = source / ".rash-source"
        if not marker.is_file() or marker.read_text() != stamp:
            raise ValueError(f"Unrecognized source directory; choose a fresh directory: {source}")
    else:
        with tempfile.TemporaryDirectory(prefix=".rash-prepare-", dir=directory) as temporary:
            # Only the hash-verified, pinned GNU archive reaches extraction.
            with tarfile.open(archive) as tar:
                tar.extractall(temporary)
            unpacked = Path(temporary) / "bash-3.2"
            with (directory / "prepare.log").open("w") as log:
                for patch in patches:
                    subprocess.run(
                        ["patch", "--batch", "-p0", "-i", str(patch)],
                        cwd=unpacked, stdout=log, stderr=subprocess.STDOUT, check=True,
                    )
            (unpacked / ".rash-source").write_text(stamp)
            unpacked.rename(source)
    for name, digest in manifest["ported_source_sha256"].items():
        verify(source / name, digest)
    return source


def build(source, directory):
    build_dir = directory / "build"
    build_dir.mkdir(exist_ok=True)
    # Bash 3.2 predates modern C defaults; these flags are for this test oracle only.
    cflags = (
        "-O2 -fwrapv -std=gnu89 -Wno-implicit-function-declaration "
        "-Wno-int-conversion -Wno-incompatible-pointer-types -Wno-return-mismatch"
    )
    configure = [
        str(source / "configure"), "--without-bash-malloc", "--disable-nls",
        "CFLAGS=" + cflags, "CFLAGS_FOR_BUILD=" + cflags,
    ]
    marker = build_dir / ".rash-configure"
    options = json.dumps(configure)
    if not (build_dir / "Makefile").exists() or not marker.exists() or marker.read_text() != options:
        with (directory / "configure.log").open("w") as log:
            subprocess.run(configure, cwd=build_dir, stdout=log, stderr=subprocess.STDOUT, check=True)
        marker.write_text(options)
    with (directory / "build.log").open("w") as log:
        subprocess.run(["make", "-j4", "bash"], cwd=build_dir, stdout=log, stderr=subprocess.STDOUT, check=True)
    oracle = build_dir / "bash"
    subprocess.run([str(oracle), "--noprofile", "--norc", "--version"], check=True)
    return oracle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="external source, download, and build directory")
    parser.add_argument("--build", action="store_true", help="build the C reference executable for tests")
    args = parser.parse_args()
    directory = args.directory.resolve()
    try:
        source = prepare(directory)
        print(f"source: {source}", flush=True)
        if args.build:
            print(f"oracle: {build(source, directory)}")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\nBuild/patch diagnostics: {directory}/*.log\n")


if __name__ == "__main__":
    main()
