#!/usr/bin/env python3
"""Exercise native fish_key_reader decoding and mode restoration in ConPTY.

Usage: python tests/native_key_reader_tmuxx.py FISH_KEY_READER_EXE TMUXX_EXE
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


DRIVER = r'''
import ctypes, json, pathlib, subprocess, sys
from ctypes import wintypes
kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.GetStdHandle.argtypes = [wintypes.DWORD]
kernel.GetStdHandle.restype = wintypes.HANDLE
kernel.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
def modes():
    result = []
    for fd in (-10, -11):
        value = wintypes.DWORD()
        assert kernel.GetConsoleMode(kernel.GetStdHandle(fd), ctypes.byref(value))
        result.append(value.value)
    return result
before = modes()
status = subprocess.call(sys.argv[2:])
after = modes()
pathlib.Path(sys.argv[1]).write_text(json.dumps(dict(status=status, before=before, after=after)))
print("KEY_READER_RESTORED" if before == after else "KEY_READER_MODES_CHANGED", flush=True)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("key_reader", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("requires Windows and tmuxx ConPTY")
    label = "fish-key-reader-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-key-reader-") as temporary:
        root = Path(temporary)
        executable = root / "fish_key_reader.exe"
        shutil.copy2(args.key_reader.resolve(), executable)
        driver = root / "driver.py"
        driver.write_text(DRIVER, encoding="utf8")
        config = root / "tmuxx.conf"
        config.write_text("set -gw remain-on-exit on\n", encoding="utf8")
        env = dict(os.environ)
        env.pop("TMUX", None)
        env.pop("TMUXX_ENDPOINT", None)
        base = [str(args.tmuxx.resolve()), "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(base + list(arguments), env=env, cwd=root,
                                  capture_output=True, stdin=subprocess.DEVNULL,
                                  timeout=10, check=check, creationflags=subprocess.CREATE_NO_WINDOW)

        def call(*arguments):
            return run(*arguments).stdout.decode("utf8").replace("\r", "")

        def wait_for(predicate, description):
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                if predicate():
                    return
                time.sleep(0.03)
            raise AssertionError(description)

        owner = subprocess.Popen(base + ["-D", "-f", str(config)], env=env, cwd=root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            wait_for(lambda: run("show-options", "-gwqv", "remain-on-exit", check=False).returncode == 0,
                     "tmuxx owner did not start")

            def case(name, flags, actions, expected):
                result_path = root / (name + ".json")
                pane = call("new-session", "-dP", "-s", name, "-x", "140", "-y", "45",
                            "-F", "#{pane_id}", sys.executable, str(driver), str(result_path),
                            str(executable), *flags).strip()

                def capture():
                    return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

                wait_for(lambda: "Press a key:" in capture(), "key reader did not become ready: " + name)
                for literal, keys in actions:
                    call("send-keys", "-t", pane, *( ["-l", keys] if literal else keys ))
                wait_for(result_path.exists, "key reader did not exit: " + name)
                result = json.loads(result_path.read_text(encoding="utf8"))
                assert result["status"] == 0, (name, result, capture())
                assert result["before"] == result["after"], (name, result)
                output = capture()
                for line in expected:
                    assert line in output, (name, line, output)
                run("kill-session", "-t", name)

            case("left", [], [(False, ["Left"])], ["bind left 'do something'"])
            case("escape", [], [(False, ["Escape"])], ["bind escape 'do something'"])
            case("control", [], [(False, ["C-c"])], ["bind ctrl-c 'do something'"])
            case("continuous", ["-cV"], [(False, ["C-a", "M-x", "Up"]), (True, "🐟"),
                 (False, ["C-c", "C-c"])], ["bind ctrl-a", "bind alt-x", "bind up", "bind '🐟'", "# decoded key:"])
            case("quit", ["-c"], [(True, "quit")], ["bind q", "bind u", "bind i", "bind t"])
            case("eof-key", ["-c"], [(False, ["C-d", "C-d"])], ["bind ctrl-d"])
            print("native key reader ConPTY checks passed (6 cases, console modes restored)")
        finally:
            run("kill-server", check=False)
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.terminate()
                owner.wait(timeout=10)


if __name__ == "__main__":
    main()
