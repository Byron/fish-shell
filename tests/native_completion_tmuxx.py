#!/usr/bin/env python3
"""Exercise live native completion callbacks in an isolated tmuxx ConPTY.

Usage: python tests/native_completion_tmuxx.py FISH_EXE TMUXX_EXE
"""

import argparse
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and tmuxx ConPTY")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-completion-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-completion-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        python = str(Path(sys.executable)).replace("\\", "/").replace("'", "\\'")
        (root / "generator.py").write_text(
            "import ctypes, os, pathlib, sys, time\n"
            "mode = sys.argv[1]\n"
            "if mode == 'read':\n"
            "    assert sys.stdin.buffer.read(1) == b''\n"
            "    print('eofchoice')\n"
            "elif mode == 'console':\n"
            "    import msvcrt\n"
            "    saved = ctypes.c_ulong()\n"
            "    assert ctypes.windll.kernel32.GetConsoleMode("
            "ctypes.c_void_p(msvcrt.get_osfhandle(0)), ctypes.byref(saved))\n"
            "    assert saved.value & 0x207 == 7, hex(saved.value)\n"
            "    print('CMP_console_restored')\n"
            "else:\n"
            "    pathlib.Path(mode + '.pid').write_text(str(os.getpid()))\n"
            "    time.sleep(60)\n",
            encoding="utf-8",
        )
        (config / "config.fish").write_text(
            "function fish_prompt; printf 'CMP> '; end\n"
            "set -g fish_autosuggestion_enabled 0\n"
            "set -g CMP_calls\n"
            "function CMP_candidates\n"
            "    set -ga CMP_calls invoked\n"
            "    echo chosen\n"
            "end\n"
            "function cmp; echo CMP_result:$argv calls:(count $CMP_calls); end\n"
            "complete -c cmp -f -a '(CMP_candidates)'\n"
            "function cmpmenu; echo CMP_menu_result:$argv; end\n"
            "function CMP_notice --on-event completion_notice; echo CMP_event_output; end\n"
            "complete -c cmpmenu -f -n 'echo CMP_condition_stderr >&2; emit completion_notice; true' -a 'alpha beta'\n"
            "function cmpslow; echo CMP_after_cancel:$argv; end\n"
            f"complete -c cmpslow -f -a \"('{python}' generator.py slow)\"\n"
            "function cmpread; echo CMP_read_result:$argv; end\n"
            f"complete -c cmpread -f -a \"('{python}' generator.py read)\"\n"
            "complete -c cmpexit -f -n 'exit 23' -a done\n",
            encoding="utf-8",
        )
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        env = dict(
            os.environ,
            HOME=temporary,
            USERPROFILE=temporary,
            FISH_NATIVE_CONFIG_HOME=str(root / "config"),
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"),
            XDG_CACHE_HOME=str(root / "cache"),
        )
        env.pop("TMUX", None)
        env.pop("TMUXX_ENDPOINT", None)
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(
                base + list(arguments), env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=10, check=check, creationflags=subprocess.CREATE_NO_WINDOW,
            )

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture():
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def wait_for(predicate, description):
            deadline = time.monotonic() + 12
            text = ""
            while time.monotonic() < deadline:
                text = capture()
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}:\n{text[-9000:]}")

        def expect(line, count=1):
            return wait_for(lambda text: text.splitlines().count(line) >= count,
                            f"missing {line!r} ({count} times)")

        def expect_input(source):
            return wait_for(
                lambda text: bool(text.rstrip().splitlines()) and
                text.rstrip().splitlines()[-1].rstrip() == ("CMP> " + source).rstrip(),
                f"missing edited input {source!r}",
            )

        def keys(*names):
            call("send-keys", "-t", pane, *names)
            if names and names[-1] == "Escape":
                time.sleep(0.06)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        def metadata(format_string):
            return call("display-message", "-pt", pane, format_string).strip()

        def wait_pid(name):
            path = root / (name + ".pid")
            wait_for(lambda _: path.exists(), f"{name} process did not start")
            return int(path.read_text())

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.GetExitCodeProcess.restype = wintypes.BOOL
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]

        def alive(pid):
            handle = kernel.OpenProcess(0x1000, False, pid)
            if not handle:
                return False
            try:
                status = wintypes.DWORD()
                assert kernel.GetExitCodeProcess(handle, ctypes.byref(status))
                return status.value == 259
            finally:
                kernel.CloseHandle(handle)

        owner = subprocess.Popen(
            base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                if time.monotonic() >= deadline:
                    raise AssertionError("isolated tmuxx owner did not become ready")
                time.sleep(0.03)
            pane = call("new-session", "-dP", "-s", "completion", "-x", "140", "-y", "45",
                        "-F", "#{pane_id}", str(fish), "-i").strip()
            assert pane.startswith("%"), pane
            expect_input("")

            # The single-candidate path must reuse the pager's result. Its
            # registered source mutates the authoritative evaluator once.
            literal("cmp ch")
            keys("Tab")
            expect_input("cmp chosen ")
            keys("Enter")
            expect("CMP_result:chosen calls:1")
            expect_input("")

            # A callback must receive EOF instead of consuming editor input.
            literal("cmpread eo")
            keys("Tab")
            expect_input("cmpread eofchoice ")
            keys("Enter")
            expect("CMP_read_result:eofchoice")
            expect_input("")

            # Multiple aggregates may queue while keyboard input is pending.
            # Event output is incidental, never an extra candidate.
            literal("cmpmenu ")
            keys("Tab")
            wait_for(lambda text: "alpha" in text and "beta" in text and "2/2" in text,
                     "completion menu did not open")
            keys("Escape")
            expect_input("cmpmenu ")
            keys("Tab", "C-g", "Tab", "C-g")
            expect("CMP_condition_stderr", 3)
            expect("CMP_event_output", 3)
            expect_input("cmpmenu ")
            keys("C-u")

            # Ownership handoff must retain jobs launched before readline.
            send(f"'{python}' generator.py background &")
            background = wait_pid("background")
            expect_input("")
            send("set -g CMP_background $last_pid; echo CMP_started:$CMP_background")
            text = wait_for(lambda text: re.search(r"(?m)^CMP_started:(\d+)$", text),
                            "background job identity was not retained")
            job_pid = int(re.search(r"(?m)^CMP_started:(\d+)$", text)[1])
            expect_input("")
            literal("cmp ch")
            keys("Tab")
            expect_input("cmp chosen ")
            keys("C-u")
            send("echo CMP_jobs:(jobs -p)")
            expect(f"CMP_jobs:{job_pid}")
            assert alive(background), "completion dropped the existing process host"
            send("kill $CMP_background; wait $CMP_background")
            expect_input("")

            # Ctrl-C must interrupt a slow generator without losing either
            # the line or its mid-line cursor. The child must be reaped.
            send("false")
            expect_input("")
            literal("cmpslow tail")
            keys("Home", *("Right",) * len("cmpslow "))
            cursor = metadata("#{cursor_x}")
            keys("Tab")
            slow = wait_pid("slow")
            keys("C-c")
            expect_input("cmpslow tail")
            wait_for(lambda _: not alive(slow), "cancelled generator remained alive")
            assert metadata("#{cursor_x}") == cursor, "completion cancellation moved the cursor"
            keys("End", "C-u")
            send("echo CMP_status:$status")
            expect("CMP_status:1")
            send(f"'{python}' generator.py console")
            expect("CMP_console_restored")
            expect_input("")

            # Query status restoration also applies to explicit callback exit;
            # the restored zero status matches POSIX completion semantics.
            literal("cmpexit ")
            keys("Tab")
            wait_for(lambda _: metadata("#{pane_dead}") == "1", "callback exit left reader running")
            assert metadata("#{pane_dead_status}") == "0"
            print("native completion ConPTY checks passed")
        finally:
            run("kill-server", check=False)
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.terminate()
                owner.wait(timeout=10)


if __name__ == "__main__":
    main()
