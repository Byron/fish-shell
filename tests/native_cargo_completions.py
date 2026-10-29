#!/usr/bin/env python3
"""Check bundled Cargo completions against an isolated, dependency-free workspace.

Usage: python tests/native_cargo_completions.py FISH_EXE
Requires Cargo, rustup and Python. No compilation or network access is needed.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def quote(value):
    return "'" + str(value).replace("\\", "\\\\").replace("'", "\\'") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fish-cargo-completions-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        (root / "Cargo.toml").write_text('''
[package]
name = "completion-root"
version = "0.1.0"
edition = "2021"
[workspace]
members = ["member"]
[features]
default = ["alpha"]
alpha = []
beta = []
[[bin]]
name = "probe-bin"
path = "src/main.rs"
[[example]]
name = "probe-example"
path = "src/main.rs"
[[test]]
name = "probe-test"
path = "src/main.rs"
[[bench]]
name = "probe-bench"
path = "src/main.rs"
''', encoding="utf-8")
        (root / "src").mkdir()
        (root / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (root / "member/src").mkdir(parents=True)
        (root / "member/Cargo.toml").write_text(
            '[package]\nname="completion-member"\nversion="0.1.0"\nedition="2021"\n',
            encoding="utf-8",
        )
        (root / "member/src/lib.rs").write_text("", encoding="utf-8")
        cases = [
            ("cargo bu", "build"),
            ("cargo build --no", "--no-default-features"),
            ("cargo build --features al", "alpha"),
            ("cargo build --features alpha,b", "alpha,beta"),
            ("cargo build --features=al", "--features=alpha"),
            ("cargo build --features=alpha,b", "--features=alpha,beta"),
            ("cargo build -p completion-m", "completion-member"),
            ("cargo build --package completion-r", "completion-root"),
            ("cargo build --bin probe", "probe-bin"),
            ("cargo build --example probe", "probe-example"),
            ("cargo test --test probe", "probe-test"),
            ("cargo bench --bench probe", "probe-bench"),
            ("cbuild --features al", "alpha"),
            ("echo \"$(cargo build --features al", "alpha"),
        ]
        targets = subprocess.check_output(
            ["rustup", "target", "list", "--installed"], text=True,
            env=dict(os.environ, RUSTUP_AUTO_INSTALL="0"), timeout=30,
        ).splitlines()
        assert targets, "the Cargo toolchain needs an installed target"
        cases.append(("cargo build --target " + targets[0], targets[0]))
        source = [
            f"function __fish_anypython; echo {quote(sys.executable)}; end",
            "function cbuild --wraps 'cargo build'; end",
        ]
        for index, (line, _) in enumerate(cases):
            source += [f"echo CASE:{index}", f"complete -C {quote(line)}"]
        env = dict(os.environ, FISH_NATIVE_CONFIG_HOME=str(root / "config"),
                   XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                   CARGO_NET_OFFLINE="true", RUSTUP_AUTO_INSTALL="0")
        result = subprocess.run([str(fish), "-N", "-c", "; ".join(source)],
                                cwd=root, env=env, capture_output=True, timeout=180)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        assert not result.stderr, result.stderr.decode(errors="replace")
        actual = {}
        current = None
        for line in result.stdout.decode("utf-8").splitlines():
            if line.startswith("CASE:"):
                current = int(line[5:])
                actual[current] = []
            else:
                actual[current].append(line.split("\t", 1)[0])
        failures = [f"{line!r}: expected {expected!r}, got {actual.get(index)!r}"
                    for index, (line, expected) in enumerate(cases)
                    if expected not in actual.get(index, [])]
        assert not failures, "\n".join(failures)
        print(f"{len(cases)} native Cargo completion cases passed")


if __name__ == "__main__":
    main()
