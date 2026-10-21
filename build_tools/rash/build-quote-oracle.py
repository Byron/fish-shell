#!/usr/bin/env python3
"""Build a test-only quoting oracle from the prepared GNU Bash 3.2 source.

Usage: build-quote-oracle.py SOURCE BUILD OUTPUT
Run fetch.py --build first; SOURCE is bash-3.2 and BUILD contains config.h/syntax.c.
The normal Cargo/Fish build never runs this script or links this C code.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    source, build, output = (Path(arg).resolve() for arg in sys.argv[1:])
    manifest = json.loads(Path(__file__).with_name("sources.json").read_text())
    for name in ["lib/sh/shquote.c", "syntax.h", "mksyntax.c"]:
        if hashlib.sha256((source / name).read_bytes()).hexdigest() != manifest["ported_source_sha256"][name]:
            raise SystemExit(f"expected the pinned GNU Bash 3.2.57 {name}")
    root = Path(__file__).resolve().parents[2]
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([
        "cc", "-std=gnu89", "-include", "string.h", "-DHAVE_CONFIG_H",
        "-I" + str(build), "-I" + str(source), "-I" + str(source / "include"),
        str(root / "tests/rash_quote_oracle.c"), str(source / "lib/sh/shquote.c"),
        str(build / "syntax.c"), "-o", str(output),
    ], check=True)
    print(output)


if __name__ == "__main__":
    main()
