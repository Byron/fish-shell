#!/usr/bin/env python3
"""Differential evaluation and lifecycle checks. Requires pexpect and an explicit external Bash oracle."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

import pexpect
from bash_input_integration import Shell

binary = Path(sys.argv[1]).resolve()
backend = Path(sys.argv[2]).resolve()
workloads = [
    "a=(zero 'one two'); declare -A m=([key]=value); printf 'RESULT:%s:%s\\n' \"${a[1]}\" \"${m[key]}\"",
    "n=3; declare -n ref=n; ((ref+=4)); printf 'RESULT:%s:%s\\n' \"$n\" \"${unset_value:-fallback}\"",
    "set -o pipefail; true | false; printf 'RESULT:%s:%s\\n' \"$?\" \"${PIPESTATUS[*]}\"",
    "text=$(printf 'a b'); [[ $text == a* ]]; printf 'RESULT:%s:%s\\n' \"$?\" \"$text\"",
    "read -r first rest <<< 'one two three'; printf 'RESULT:%s:%s\\n' \"$first\" \"$rest\"",
    "for x in one two; do case $x in one) printf 'RESULT:first\\n';; *) printf 'RESULT:second\\n';; esac; done",
    "cat <<'END'\nRESULT:literal $text\nEND",
]
with tempfile.TemporaryDirectory(prefix='fish-bash-semantics-') as tmp:
    shell = Shell(binary, tmp)
    try:
        for source in workloads:
            reference = subprocess.run([str(backend), '--noprofile', '--norc', '-c', source],
                                       env=shell.env, capture_output=True, check=True)
            expected = reference.stdout.replace(b'\n', b'\r\n')
            shell.child.sendline(source)
            shell.child.expect_exact(expected)
            shell.finished(0)
        shell.child.sendline('exit')
        shell.child.expect(pexpect.EOF)
    finally:
        shell.close()
    print(f'PASS {len(workloads)} Bash differential workloads (arrays, nameref, status, substitutions, read, case, heredoc)', flush=True)

    for sig, disown in [(signal.SIGTERM, False), (signal.SIGHUP, False), (signal.SIGHUP, True)]:
        job_file = Path(tmp) / 'job'
        shell = Shell(binary, tmp)
        pid = None
        try:
            suffix = '; disown' if disown else ''
            shell.run(f'sleep 30 & echo $! > {job_file}' + suffix)
            pid = int(job_file.read_text())
            os.kill(shell.child.pid, sig)
            shell.child.expect(pexpect.EOF)
            shell.close()
            def alive():
                try:
                    os.kill(pid, 0)
                    return True
                except ProcessLookupError:
                    return False
            deadline = time.monotonic() + 3
            while not disown and alive() and time.monotonic() < deadline:
                time.sleep(.02)
            if alive() != disown:
                diagnostic = subprocess.run(['ps', '-o', 'pid,ppid,pgid,state,command', '-p', str(pid)], capture_output=True, text=True).stdout
                raise AssertionError((sig, disown, pid, diagnostic))
        finally:
            shell.close()
            if pid:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
    print('PASS HUP/TERM job cleanup and disown survival', flush=True)
