#!/usr/bin/env python3
"""Compare binaries built alike. Requires pexpect; use release builds for latency.

Usage: python3 tests/bash_input_bench.py BASELINE PATCHED
Times include PTY transport and a Fish keybinding that reports the buffer.
The observer uses the PTY directly, without filesystem polling.
"""
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import tempfile
import time

from bash_input_integration import Shell


class BenchmarkShell(Shell):
    def __init__(self, binary, home, mode, startup=False):
        observer = '''bind ctrl-g 'printf "\\x1e"; commandline --current-buffer;
                      printf "\\x1f"; commandline --cursor; printf "\\x1e"'; '''
        super().__init__(binary, home, bash=mode == "rash", dialect=mode == "bash-style",
                         config=observer, startup=startup)

    def buffer(self):
        self.child.sendcontrol('g')
        self.child.expect(re.compile(rb'\x1e(.*?)\r?\n\x1f(\d+)\r?\n\x1e', re.S))
        return self.child.match[1].decode(), int(self.child.match[2])


def summary(samples):
    ordered = sorted(samples)
    return {'n': len(samples), 'p50_ms': round(statistics.median(samples), 3),
            'p95_ms': round(ordered[min(len(samples) - 1, int(len(samples) * .95))], 3),
            'samples_ms': [round(value, 6) for value in samples]}


def bench(binary, home, repo, mode, startup=False):
    shell = BenchmarkShell(binary, home, mode, startup)
    try:
        if startup:
            shell.run('test "$BENCH_RC" = loaded')
        prompts = []
        for _ in range(30):
            begin = time.perf_counter()
            shell.run('true')
            prompts.append((time.perf_counter() - begin) * 1000)
        shell.run(f"repo='{repo}'" if mode != "fish" else f"set -g repo '{repo}'")
        completions = {}
        for source in ['git --ver', 'git -C "$repo" switch topic-']:
            timings = []
            for i in range(21):
                shell.child.send(source)
                assert shell.buffer()[0] == source
                begin = time.perf_counter()
                shell.child.send('\t')
                text, _ = shell.buffer()
                elapsed = (time.perf_counter() - begin) * 1000
                assert text != source, (source, text)
                if i:  # Warm autoload/provider path; first completion is excluded.
                    timings.append(elapsed)
                shell.clear()
            completions[source] = summary(timings)
        shell.child.sendline('exit')
        shell.child.expect(__import__('pexpect').EOF)
        return {'binary': str(binary), 'mode': mode, 'bash_startup': startup,
                'true_to_prompt': summary(prompts), 'completion': completions}
    finally:
        shell.close()


if __name__ == '__main__':
    baseline, patched = [Path(p).resolve() for p in sys.argv[1:]]
    with tempfile.TemporaryDirectory(prefix='fish-bash-bench-') as tmp:
        root = Path(tmp)
        repo = root / 'repo ü space'
        repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        '-c', 'commit.gpgsign=false', 'commit', '-q', '--allow-empty', '-m', 'fixture'], check=True)
        subprocess.run(['git', '-C', str(repo), 'branch', 'topic-completion'], check=True)
        cases = [(baseline, "fish", False), (patched, "fish", False),
                 (patched, "bash-style", False), (patched, "rash", False), (patched, "rash", True)]
        starts = [[] for _ in cases]
        for i, (_, _, startup) in enumerate(cases):
            home = root / str(i)
            home.mkdir()
            if startup:
                (home / '.bashrc').write_text(
                    "export BENCH_RC=loaded\nalias bench_alias=true\nbench_function() { :; }\n")
        # Interleave and rotate startup cases to reduce execution-order bias.
        for sample in range(15):
            for offset in range(len(cases)):
                i = (sample + offset) % len(cases)
                binary, mode, startup = cases[i]
                begin = time.perf_counter()
                shell = BenchmarkShell(binary, root / str(i), mode, startup)
                starts[i].append((time.perf_counter() - begin) * 1000)
                shell.child.sendline('exit')
                shell.child.expect_exact(b'\x1b]133;C;')
                shell.child.expect(__import__('pexpect').EOF)
                shell.close()
        for i, (binary, mode, startup) in enumerate(cases):
            result = bench(binary, root / str(i), repo, mode, startup)
            result['configured_startup'] = summary(starts[i])
            print(json.dumps(result), flush=True)
