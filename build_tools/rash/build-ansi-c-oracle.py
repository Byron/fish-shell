#!/usr/bin/env python3
"""Build a test-only oracle from unchanged GNU Bash 3.2 strtrans.c.

Usage: build-ansi-c-oracle.py /path/to/bash-3.2 /path/to/build /path/to/oracle
The configured build directory supplies config.h; the source remains unchanged.
The normal Cargo/Fish build never runs this script or links this C code.
"""
import hashlib
from pathlib import Path
import subprocess
import sys


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    source, build, output = (Path(arg).resolve() for arg in sys.argv[1:])
    expected = "67fb97f59da166c5cc885800ff940abb75af6ad3a31588c7584ced0244228e07"
    unit = source / "lib/sh/strtrans.c"
    if hashlib.sha256(unit.read_bytes()).hexdigest() != expected:
        raise SystemExit("expected unchanged GNU Bash 3.2 lib/sh/strtrans.c")
    root = Path(__file__).resolve().parents[2]
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([
        "cc", "-std=gnu89", "-include", "string.h", "-DHAVE_CONFIG_H",
        "-I" + str(build), "-I" + str(source), "-I" + str(source / "include"),
        str(root / "tests/rash_ansi_c_oracle.c"), str(unit), "-o", str(output),
    ], check=True)
    print(output)


if __name__ == "__main__":
    main()
