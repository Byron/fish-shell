#!/usr/bin/env python3
"""Report source-loading compatibility for the bundled completion corpus.

Usage: python tests/native_completion_audit.py FISH_BINARY REPORT_JSON

Each script runs in a fresh native Fish process with isolated configuration,
data, cache and vendor directories, an empty PATH, and packaged utilities when
enabled in the binary.
The executable is copied first so this audit can run alongside a new build.

This measures source loading and registration counts, not dynamic completion
correctness. A script can register nothing when its external command is absent,
or fail while probing an unavailable program. Diagnostics and exit statuses are
preserved in the JSON report; the audit does not treat these results as a single
pass/fail assertion. Use installed-tool and fresh-home integration checks to
validate actual suggestions and startup behavior.
"""

import argparse
from collections import Counter
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def check_script(fish, case, script, base_env, timeout):
    case.mkdir()
    home = case / "home"
    home.mkdir()
    env = dict(
        base_env,
        HOME=str(home),
        USERPROFILE=str(home),
        PATH="",
        FISH_NATIVE_CONFIG_HOME=str(case / "config"),
        XDG_CONFIG_HOME=str(case / "config"),
        XDG_DATA_HOME=str(case / "data"),
        XDG_CACHE_HOME=str(case / "cache"),
        XDG_DATA_DIRS=str(case / "vendor"),
        TEMP=str(case),
        TMP=str(case),
        AUDIT_SOURCE=str(script),
    )
    source = (
        'builtin source "$AUDIT_SOURCE"; '
        'builtin set -l audit_status $status; '
        'builtin printf "AUDIT:%s:%s\\n" $audit_status '
        '(builtin count (builtin complete)); builtin exit $audit_status'
    )
    try:
        result = subprocess.run(
            [str(fish), "-N", "-c", source],
            cwd=case,
            env=env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        return dict(
            script=script.name,
            classification="timeout",
            status="timeout",
            source_status=None,
            registrations=None,
            stdout=(error.stdout or b"").decode("utf-8", errors="replace"),
            stderr=(error.stderr or b"").decode("utf-8", errors="replace"),
        )

    stdout = result.stdout.decode("utf-8", errors="replace")
    stderr = result.stderr.decode("utf-8", errors="replace")
    source_status = registrations = None
    for line in stdout.splitlines():
        parts = line.split(":")
        if len(parts) == 3 and parts[0] == "AUDIT":
            try:
                source_status, registrations = map(int, parts[1:])
            except ValueError:
                continue
    if source_status is None:
        classification = "unreported"
    elif result.returncode or source_status:
        classification = "failed"
    elif registrations:
        classification = "loaded"
    else:
        classification = "empty"
    return dict(
        script=script.name,
        classification=classification,
        status=result.returncode,
        source_status=source_status,
        registrations=registrations,
        stdout=stdout,
        stderr=stderr,
    )


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("binary", type=Path, help="native Fish executable")
    parser.add_argument("report", type=Path, help="destination JSON report")
    parser.add_argument("--jobs", type=positive_int, default=6, help="parallel processes (default: 6)")
    parser.add_argument("--timeout", type=positive_int, default=8, help="seconds per script (default: 8)")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"Fish executable does not exist: {binary}")

    repo = Path(__file__).resolve().parent.parent
    scripts = sorted((repo / "share/completions").glob("*.fish"))
    scratch = repo / "target"
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="completion-audit-", dir=scratch) as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(binary, fish)
        base_env = {
            key: value for key, value in os.environ.items()
            if key.upper() in ("SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "COMSPEC")
        }

        def check(pair):
            index, script = pair
            return check_script(fish, root / str(index), script, base_env, args.timeout)

        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            results = list(pool.map(check, enumerate(scripts)))

    args.report.resolve().write_text(
        json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8"
    )
    counts = Counter(item["classification"] for item in results)
    diagnostics = Counter(
        item["classification"] for item in results if item["stderr"]
    )
    print(json.dumps(dict(
        coverage="source loading only",
        total=len(results),
        counts=dict(counts),
        with_diagnostics=dict(diagnostics),
    )))
    errors = Counter(
        item["stderr"].splitlines()[-1] if item["stderr"] else str(item["status"])
        for item in results if item["classification"] in ("failed", "timeout", "unreported")
    )
    for message, count in errors.most_common(20):
        print(f"{count:4}: {message}")


if __name__ == "__main__":
    main()
