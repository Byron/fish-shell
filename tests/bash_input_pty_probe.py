#!/usr/bin/env python3
"""Manual execution feasibility probe; this does not launch a Bash mode in Fish.

Run with python3 tests/bash_input_pty_probe.py. A failing transport is reported
as a feasibility failure (exit 1), never silently counted as shell compatibility.
"""

import errno
import os
from pathlib import Path
import select
import signal
import socket
import sys
import time


def run(control):
    source = (Path(__file__).resolve().parents[1] / "src/bash_input_session.rs").read_text()
    driver = source.split('const DRIVER: &str = r#"', 1)[1].split('"#;', 1)[0]
    parent, peer = socket.socketpair()
    pid, terminal = os.forkpty()
    if pid == 0:
        parent.close()
        os.dup2(peer.fileno(), 9, inheritable=True)
        if peer.fileno() != 9:
            peer.close()
        for sig in (signal.SIGINT, signal.SIGQUIT, signal.SIGTSTP,
                    signal.SIGTTIN, signal.SIGTTOU, signal.SIGCHLD):
            signal.signal(sig, signal.SIG_DFL)
        env = {k: v for k, v in os.environ.items() if k not in ("BASH_ENV", "ENV")}
        os.execve("/bin/bash", ["bash", "--noprofile", "--norc", "-i", "-c", driver], env)
    peer.close()
    os.set_blocking(terminal, False)
    output = bytearray()
    jobs = []

    def request(command, control=None):
        parent.sendall(command.encode() + b"\0")
        started = time.monotonic()
        pending = bytearray()
        fields = []
        while time.monotonic() - started < 3:
            if control and time.monotonic() - started >= 0.2:
                os.write(terminal, bytes([control]))
                control = None
            readable, _, _ = select.select([parent, terminal], [], [], 0.02)
            if terminal in readable:
                try:
                    output.extend(os.read(terminal, 65536))
                except OSError as e:
                    if e.errno != errno.EIO:
                        raise
            if parent in readable:
                chunk = parent.recv(65536)
                if not chunk:
                    raise RuntimeError("worker exited before its state response")
                pending.extend(chunk)
                while b"\0" in pending:
                    field, _, rest = pending.partition(b"\0")
                    pending[:] = rest
                    if len(fields) >= 2 and len(fields) % 2 == 0 and not field:
                        return int(fields[0]), dict(zip(fields[2::2], fields[3::2]))
                    fields.append(bytes(field))
        raise RuntimeError("worker did not return a state response within 3 seconds")

    try:
        status, state = request("test -t 0; terminal=$?; sleep 30 & worker=$!")
        assert status == 0 and state[b"terminal"] == b"0"
        jobs.append(int(state[b"worker"]))
        assert os.tcgetpgrp(terminal) == pid
        status, state = request("fg; signal_status=$?", control)
        expected_signal = signal.SIGTSTP if control == 26 else signal.SIGINT
        assert int(state[b"signal_status"]) == 128 + expected_signal
        assert os.tcgetpgrp(terminal) == pid
        print(f"PASS: control={control}, persistent driver response")
        return 0
    except (RuntimeError, AssertionError, BrokenPipeError) as e:
        print(f"FAIL: control={control}: {e}")
        print(f"Terminal transcript: {bytes(output[-4096:])!r}")
        return 1
    finally:
        parent.close()
        for job in jobs:
            try:
                os.kill(job, signal.SIGKILL)
            except ProcessLookupError:
                pass
        os.close(terminal)
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        # A kernel tty teardown problem must not hang the rest of the investigation.
        for _ in range(50):
            if os.waitpid(pid, os.WNOHANG)[0]:
                break
            time.sleep(0.02)
        else:
            print(f"WARNING: worker {pid} has not finished kernel teardown")


def native():
    """Control experiment using Bash's actual interactive loop (requires pexpect)."""
    import pexpect

    env = os.environ.copy()
    env.update(PS1="BASH_PROBE> ", TERM="dumb")
    env.pop("PROMPT_COMMAND", None)
    shell = pexpect.spawn("/bin/bash", ["--noprofile", "--norc", "-i"], env=env, timeout=5)
    shell.delaybeforesend = 0
    try:
        shell.expect_exact(b"BASH_PROBE> ")
        shell.sendline("probe_var=retained")
        shell.expect_exact(b"BASH_PROBE> ")
        shell.sendline("sleep 30")
        time.sleep(.2)
        shell.sendcontrol("z")
        shell.expect_exact(b"Stopped")
        shell.expect_exact(b"BASH_PROBE> ")
        shell.sendline("fg")
        time.sleep(.2)
        shell.sendcontrol("c")
        shell.expect_exact(b"BASH_PROBE> ")
        shell.sendline("printf 'VALUE=%s\\n' \"$probe_var\"")
        shell.expect_exact(b"\r\nVALUE=retained\r\n")
        shell.expect_exact(b"BASH_PROBE> ")
        shell.sendline("exit")
        shell.expect(pexpect.EOF)
        print("PASS: native Bash Ctrl-Z, fg, Ctrl-C, session retained")
        return 0
    finally:
        shell.close(force=True)


if __name__ == "__main__":
    raise SystemExit(native() if sys.argv[1:] == ["--native"] else max(run(26), run(3)))
