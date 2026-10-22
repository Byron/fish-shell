#!/usr/bin/env python3
"""Exercise the native Windows editor through an isolated tmuxx ConPTY owner.

Usage: python tests/native_interactive_tmuxx.py FISH_EXE TMUXX_EXE

The fixture uses a unique server label and private config, cwd and history. It
never changes an existing Rustty/tmuxx session or the user's shell configuration.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and the packaged tmuxx ConPTY runtime")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-editor-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-editor-") as temporary:
        root = Path(temporary)
        fish = str(root / "fish.exe")
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        (config / "config.fish").write_text("set -g editor_config configured\n", encoding="utf-8")
        (root / "EDITOR filename 🐟.txt").write_text("completion fixture\n", encoding="utf-8")
        commands = root / "commands"
        commands.mkdir()
        shutil.copy2(fish, commands / "EDITOR-probe.exe")
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

        def expect(pane, line, count=1):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture(pane)
                if text.splitlines().count(line) >= count:
                    return text
                time.sleep(0.03)
            raise AssertionError(f"missing output {line!r} in pane {pane}:\n{text[-6000:]}")

        def expect_prompt(pane, language="fish"):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                text = capture(pane)
                if language + ">" in text:
                    return
                time.sleep(0.03)
            raise AssertionError(f"no native editor prompt:\n{text}")

        def keys(pane, *names):
            call("send-keys", "-t", pane, *names)

        def literal(pane, source):
            call("send-keys", "-t", pane, "-l", source)

        def send(pane, source):
            literal(pane, source)
            keys(pane, "Enter")

        def new_session(name, *arguments):
            pane = call("new-session", "-dP", "-s", name, "-x", "120", "-y", "40",
                        "-F", "#{pane_id}", fish, *arguments).strip()
            assert pane.startswith("%"), pane
            return pane

        def wait_dead(pane, status=0):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                state = call("display-message", "-pt", pane, "#{pane_dead}|#{pane_dead_status}").strip()
                if state.startswith("1|"):
                    assert state == f"1|{status}", (state, capture(pane))
                    return
                time.sleep(0.03)
            raise AssertionError(f"pane did not exit: {pane}\n{capture(pane)}")

        owner = subprocess.Popen(base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode != 0:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                if time.monotonic() >= deadline:
                    raise AssertionError("isolated tmuxx owner did not become ready")
                time.sleep(0.03)

            pane = new_session("editor", "-i")
            expect_prompt(pane)
            send(pane, "echo EDITOR_config:$editor_config")
            expect(pane, "EDITOR_config:configured")

            literal(pane, "echo EDITOR_lft")
            keys(pane, "Left", "Left")
            literal(pane, "e")
            keys(pane, "Right", "End", "Enter")
            expect(pane, "EDITOR_left")

            literal(pane, "xecho EDITOR_home")
            keys(pane, "Home", "Delete", "End", "Enter")
            expect(pane, "EDITOR_home")

            literal(pane, "echo EDITOR_backspacex")
            keys(pane, "BSpace", "Enter")
            expect(pane, "EDITOR_backspace")

            literal(pane, "echo EDITOR_🐟é")
            keys(pane, "Left", "BSpace")
            literal(pane, "🦀")
            keys(pane, "End", "Enter")
            expect(pane, "EDITOR_🦀é")

            literal(pane, "echo 'EDITOR fil")
            keys(pane, "Tab", "Enter")
            expect(pane, "EDITOR filename 🐟.txt")

            literal(pane, "ech")
            keys(pane, "Tab")
            literal(pane, " EDITOR_command")
            keys(pane, "Enter")
            expect(pane, "EDITOR_command")

            send(pane, "function EDITOR_function; echo EDITOR_function_complete; end")
            literal(pane, "EDITOR_fun")
            keys(pane, "Tab", "Enter")
            expect(pane, "EDITOR_function_complete")

            # Updating the shell's PATH must update completion immediately.
            send(pane, "set -gx PATH '" + commands.as_posix() + "'")
            literal(pane, "EDITOR-pr")
            keys(pane, "Tab")
            literal(pane, " -N -c 'echo EDITOR_path'")
            keys(pane, "Enter")
            expect(pane, "EDITOR_path")

            send(pane, "for item in EDITOR_multi_one EDITOR_multi_two")
            send(pane, "echo $item")
            send(pane, "end")
            expect(pane, "EDITOR_multi_one")
            expect(pane, "EDITOR_multi_two")

            literal(pane, "echo EDITOR_must_not_execute")
            keys(pane, "C-c")
            send(pane, "echo EDITOR_after_interrupt")
            expect(pane, "EDITOR_after_interrupt")
            assert "EDITOR_must_not_execute" not in capture(pane).splitlines()

            literal(pane, "echo EDITOR_suspend_must_not_execute")
            keys(pane, "C-z")
            send(pane, "echo EDITOR_after_suspend_key")
            expect(pane, "EDITOR_after_suspend_key")
            assert "EDITOR_suspend_must_not_execute" not in capture(pane).splitlines()

            send(pane, "echo EDITOR_history")
            expect(pane, "EDITOR_history")
            keys(pane, "Up", "Down")
            send(pane, "echo EDITOR_history_last")
            expect(pane, "EDITOR_history_last")
            keys(pane, "C-d")
            wait_dead(pane)

            restored = new_session("restored", "-i")
            expect_prompt(restored)
            keys(restored, "Up", "Enter")
            expect(restored, "EDITOR_history_last")
            keys(restored, "C-d")
            wait_dead(restored)

            history_path = root / "data/fish/native_fish_history.yaml"
            history_before_private = history_path.read_bytes()
            private = new_session("private", "-i", "--private")
            expect_prompt(private)
            keys(private, "Up", "Enter")
            send(private, "echo EDITOR_private_not_saved")
            expect(private, "EDITOR_private_not_saved")
            assert "EDITOR_history_last" not in capture(private).splitlines()
            keys(private, "C-d")
            wait_dead(private)
            assert history_path.read_bytes() == history_before_private, "private mode rewrote Fish history"
            history = history_path.read_text(encoding="utf-8")
            assert history.startswith("- cmd: "), history
            assert "EDITOR_history_last" in history
            assert "EDITOR_private_not_saved" not in history
            assert not (root / "data/fish/native_fish_history").exists()
            saved = subprocess.run(
                [fish, "-N", "-c", "history search --null"], env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=10, check=True, creationflags=subprocess.CREATE_NO_WINDOW,
            ).stdout
            assert saved.endswith(b"\0"), saved
            saved_commands = saved[:-1].decode("utf-8").split("\0")
            assert "echo EDITOR_history_last" in saved_commands
            assert "echo EDITOR_🦀é" in saved_commands
            assert ("for item in EDITOR_multi_one EDITOR_multi_two\n    echo $item\n    end") in saved_commands, saved_commands
            assert "echo EDITOR_private_not_saved" not in saved_commands

            skipped = new_session("no_config", "-N", "-c",
                                  "if set -q editor_config; echo CONFIG_BAD; else; echo CONFIG_SKIPPED; end")
            wait_dead(skipped)
            expect(skipped, "CONFIG_SKIPPED")

            custom = new_session("custom_prompt", "-N", "-i", "-P")
            expect_prompt(custom)
            send(custom, "function fish_prompt; printf 'EDITOR_prompt[%s]> ' $status; end")

            def expect_custom_prompt(status):
                deadline = time.monotonic() + 10
                expected = f"EDITOR_prompt[{status}]>"
                while time.monotonic() < deadline:
                    lines = [line.rstrip() for line in capture(custom).splitlines() if line.strip()]
                    if lines and lines[-1] == expected:
                        return
                    time.sleep(0.03)
                raise AssertionError(f"missing custom prompt {expected!r}:\n{capture(custom)}")

            expect_custom_prompt(0)
            send(custom, "false")
            expect_custom_prompt(1)
            send(custom, "echo EDITOR_prompt_status:$status")
            expect(custom, "EDITOR_prompt_status:1")
            send(custom, "exit 0")
            wait_dead(custom)

            rash = new_session("rash_editor", "--rash-shell", "-i")
            expect_prompt(rash, "rash")
            keys(rash, "Up", "Enter")
            literal(rash, "builti")
            keys(rash, "Tab")
            literal(rash, " printf '%s\\n' EDITOR_rash_completion")
            keys(rash, "Enter")
            expect(rash, "EDITOR_rash_completion")
            send(rash, "EDITOR_rash_function() { echo EDITOR_rash_function_complete; }")
            literal(rash, "EDITOR_rash_fun")
            keys(rash, "Tab", "Enter")
            expect(rash, "EDITOR_rash_function_complete")
            assert "EDITOR_history_last" not in capture(rash).splitlines()
            literal(rash, "echo EDITOR_rash_cancelled")
            keys(rash, "C-c")
            send(rash, "echo EDITOR_rash_history")
            expect(rash, "EDITOR_rash_history")
            assert "EDITOR_rash_cancelled" not in capture(rash).splitlines()
            keys(rash, "C-d")
            wait_dead(rash)

            rash_restored = new_session("rash_restored", "--rash-shell", "-i")
            expect_prompt(rash_restored, "rash")
            keys(rash_restored, "Up", "Enter")
            expect(rash_restored, "EDITOR_rash_history")
            keys(rash_restored, "C-d")
            wait_dead(rash_restored)
            rash_history = (root / "data/fish/native_rash_history").read_text(encoding="utf-8")
            assert rash_history.startswith("#V2\n"), rash_history
            assert "EDITOR_rash_history" in rash_history
            assert "EDITOR_history_last" not in rash_history
            assert "EDITOR_rash_history" not in history

            print("PASS native ConPTY editor: cursor/delete/backspace, Unicode, filename/command/PATH "
                  "completion, multiline input, Ctrl-C/Ctrl-Z recovery, history reload/private mode, "
                  "config/-N, fish_prompt/status, functions, Rash completion/recovery/separate history")
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
    main()
