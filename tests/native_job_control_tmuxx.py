#!/usr/bin/env python3
"""Check native Windows job control through a private tmuxx ConPTY owner.

Usage: python tests/native_job_control_tmuxx.py FISH_EXE TMUXX_EXE

Only this fixture's UUID-labelled server and owned panes/processes are controlled.
Its executable copy, HOME, configuration, working directory and probes are private.
"""

import argparse
import ctypes
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


def probe(arguments):
    """Small native external programs, including an inherited process tree."""
    action, *arguments = arguments
    if action == "wait":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetStdHandle.restype = ctypes.c_void_p
        kernel.GetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        mode = ctypes.c_ulong()
        success = kernel.GetConsoleMode(kernel.GetStdHandle(-10), ctypes.byref(mode))
        print(f"JOB_console_mode:{success}:{mode.value:x}", flush=True)
        print(arguments[0], flush=True)
        while True:
            time.sleep(1)
    elif action == "copy":
        for line in sys.stdin.buffer:
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
    elif action == "tick":
        with open(arguments[0], "ab", buffering=0) as output:
            while True:
                output.write(b".")
                time.sleep(0.02)
    elif action == "tree":
        child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                  "--probe", "tick", arguments[0]])
        print(arguments[1], flush=True)
        sys.exit(child.wait())
    else:
        raise AssertionError(action)


def quote(value):
    return "'" + str(value).replace("\\", "/").replace("'", "'\\''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and the packaged tmuxx ConPTY runtime")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-jobs-" + uuid.uuid4().hex
    helper = quote(sys.executable) + " -u " + quote(Path(__file__).resolve()) + " --probe "
    with tempfile.TemporaryDirectory(prefix="fish-jobs-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        env = dict(os.environ, HOME=temporary, USERPROFILE=temporary,
                   XDG_CONFIG_HOME=str(root / "config"), FISH_NATIVE_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"))
        env.pop("TMUX", None)
        env.pop("TMUXX_ENDPOINT", None)
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(base + list(arguments), env=env, cwd=root,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=10, check=check,
                                  creationflags=subprocess.CREATE_NO_WINDOW)

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture(pane):
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def until(condition, message):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                result = condition()
                if result:
                    return result
                time.sleep(0.03)
            raise AssertionError(message())

        def expect(pane, line):
            return until(lambda: line in capture(pane).splitlines(),
                         lambda: f"missing output {line!r}:\n{capture(pane)[-8000:]}")

        def prompt(pane, language):
            def present():
                lines = [line for line in capture(pane).splitlines() if line.strip()]
                return lines and lines[-1].rstrip().endswith(language + ">")
            until(present, lambda: f"no {language} prompt:\n{capture(pane)[-8000:]}")

        def keys(pane, *names):
            call("send-keys", "-t", pane, *names)

        def send(pane, source):
            call("send-keys", "-t", pane, "-l", source)
            keys(pane, "Enter")

        def wait_dead(pane):
            def dead():
                state = call("display-message", "-pt", pane, "#{pane_dead}|#{pane_dead_status}").strip()
                if state.startswith("1|"):
                    assert state == "1|0", (state, capture(pane))
                    return True
                return False
            until(dead, lambda: f"pane did not exit:\n{capture(pane)[-8000:]}")

        def size(path):
            return path.stat().st_size if path.exists() else 0

        def growing(path, previous=0):
            until(lambda: size(path) > previous,
                  lambda: f"owned descendant did not make progress: {path}")

        def stable(path):
            time.sleep(0.05)
            previous = size(path)
            time.sleep(0.15)
            assert size(path) == previous, f"owned descendant is still running: {path}"
            return previous

        owner = subprocess.Popen(base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            def ready():
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                return run("show-options", "-gwqv", "remain-on-exit", check=False).returncode == 0
            until(ready, lambda: "isolated tmuxx owner did not become ready")

            for language in ("fish", "rash"):
                arguments = ["--rash-shell"] if language == "rash" else []
                pane = call("new-session", "-dP", "-s", language, "-x", "160", "-y", "50",
                            "-F", "#{pane_id}", str(fish), "-N", "-i", *arguments).strip()
                assert pane.startswith("%"), pane
                prompt(pane, language)
                status = "$status" if language == "fish" else "$?"

                def check_status(label, expected):
                    send(pane, f"echo JOB_{label}:" + status)
                    expect(pane, f"JOB_{label}:{expected}")

                def interrupt(source, marker, label):
                    send(pane, source)
                    expect(pane, marker)
                    keys(pane, "C-c")
                    prompt(pane, language)
                    check_status(label, 130)
                    send(pane, "echo JOB_recovered_" + label)
                    expect(pane, "JOB_recovered_" + label)

                interrupt(helper + "wait JOB_external_ready", "JOB_external_ready", "external")
                loop = "while true; end" if language == "fish" else "while true; do :; done"
                interrupt("echo JOB_parent_ready; " + loop, "JOB_parent_ready", "parent")
                worker = ("begin; echo JOB_worker_ready; " + loop + "; end"
                          if language == "fish" else "{ echo JOB_worker_ready; " + loop + "; }")
                interrupt(worker + " | " + helper + "copy", "JOB_worker_ready", "worker")

                def background(name, worker=False):
                    path = root / (language + "-" + name)
                    assign = "set -g job_pid $last_pid" if language == "fish" else "job_pid=$!"
                    command = helper + "tree " + quote(path) + " JOB_" + name + "_ready"
                    if worker:
                        command = ("begin; " + command + "; end" if language == "fish"
                                   else "{ " + command + "; }")
                    send(pane, command + " & " + assign + "; echo JOB_" + name + "_pid:$job_pid")
                    # A background child can write after the next prompt has
                    # already been painted on the same line.
                    until(lambda: re.search(r"(?m)^(?:" + language + r"> )?JOB_" + name
                                            + r"_ready$", capture(pane)),
                          lambda: f"no background readiness marker:\n{capture(pane)[-8000:]}")
                    text = until(lambda: re.search(r"(?m)^JOB_" + name + r"_pid:(\d+)$", capture(pane)),
                                 lambda: f"no owned PID:\n{capture(pane)[-8000:]}")
                    growing(path)
                    return path, text.group(1)

                isolated, _ = background("isolation", worker=True)
                interrupt(helper + "wait JOB_isolated_foreground_ready",
                          "JOB_isolated_foreground_ready", "isolated_foreground")
                growing(isolated, size(isolated))
                send(pane, "kill -TERM $job_pid; wait $job_pid; echo JOB_isolation_cleanup:" + status)
                expect(pane, "JOB_isolation_cleanup:143")
                stable(isolated)

                heartbeat, pid = background("background")
                send(pane, "jobs -l; echo JOB_listed")
                expect(pane, "JOB_listed")
                assert re.search(r"(?m)^\[\d+\] .*\b" + pid + r"\b.*Running", capture(pane))
                send(pane, "kill -STOP $job_pid; jobs -s; echo JOB_stopped")
                expect(pane, "JOB_stopped")
                assert re.search(r"(?m)^\[\d+\] Stopped\b", capture(pane))
                stopped = stable(heartbeat)
                send(pane, "bg $job_pid; echo JOB_resumed")
                expect(pane, "JOB_resumed")
                growing(heartbeat, stopped)
                send(pane, "kill -STOP $job_pid; echo JOB_stopped_again")
                expect(pane, "JOB_stopped_again")
                stopped = stable(heartbeat)
                send(pane, "fg $job_pid")
                growing(heartbeat, stopped)
                keys(pane, "C-c")
                prompt(pane, language)
                check_status("foreground", 130)
                stable(heartbeat)

                terminated, _ = background("terminate")
                send(pane, "kill -TERM $job_pid; wait $job_pid; echo JOB_terminated:" + status)
                expect(pane, "JOB_terminated:143")
                stable(terminated)
                send(pane, f"kill -0 {os.getpid()}; echo JOB_unowned:" + status)
                expect(pane, "JOB_unowned:1")
                shutdown, _ = background("shutdown")
                send(pane, "exit 0")
                wait_dead(pane)
                stable(shutdown)
                print(f"PASS native {language} ConPTY jobs: external/parent/worker Ctrl-C=130, "
                      "background worker isolation, prompt recovery, owned PID listing, "
                      "tree stop/bg/fg/TERM, ownership, shutdown", flush=True)
        finally:
            if owner.poll() is None:
                try:
                    run("kill-server", check=False)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.kill()
                owner.wait(timeout=10)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--probe":
        probe(sys.argv[2:])
    else:
        main()
