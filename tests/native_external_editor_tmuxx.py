#!/usr/bin/env python3
"""Check Ctrl-X Ctrl-E and external editor ownership in a private tmuxx session.

Usage: python tests/native_external_editor_tmuxx.py FISH_EXE TMUXX_EXE [--helix HX_EXE]
The matching fish_indent.exe must be beside FISH_EXE.
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


def quote(value):
    return "'" + str(value).replace("\\", "\\\\").replace("'", "\\'") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    parser.add_argument("--helix", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("native Windows and tmuxx are required")
    fish_source = args.fish.resolve()
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-external-editor-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-external-editor-") as temporary:
        root = Path(temporary) / "work Ω"
        root.mkdir()
        (root / "tmp").mkdir()
        fish = root / "fish.exe"
        shutil.copy2(fish_source, fish)
        shutil.copy2(fish_source.with_name("fish_indent.exe"), root / "fish_indent.exe")
        config = root / "config" / "fish"
        (config / "functions").mkdir(parents=True)
        (config / "functions" / "fish_user_key_bindings.fish").write_text(
            "function seed_trailing_editor\n"
            "    commandline -r -- 'echo trailing\n\n'\n"
            "    commandline -C 999999\n"
            "    commandline -f edit-command-buffer\n"
            "end\n"
            "function fish_user_key_bindings\n"
            "    bind ctrl-e 'if commandline --search-mode; or commandline --paging-mode; "
            "commandline -f execute; else; commandline -f end-of-line; end'\n"
            "    bind ctrl-x,ctrl-e edit_command_buffer\n"
            "    bind ctrl-x,ctrl-t seed_trailing_editor\n"
            "end\n",
            encoding="utf-8",
        )
        events = root / "events"
        (config / "config.fish").write_text(
            "function fish_prompt; printf 'EDIT> '; end\n"
            "set -g fish_autosuggestion_enabled 0\n"
            + "".join(
                f"function record_{event} --on-event {event}; echo {event} >> {quote(events)}; end\n"
                for event in ("fish_preexec", "fish_postexec", "fish_cancel")
            ), encoding="utf-8",
        )
        editor = root / "editor.py"
        editor.write_text(
            "import ctypes, json, os, pathlib, sys\n"
            "root = pathlib.Path(os.environ['EDITOR_FIXTURE']).resolve()\n"
            "path = pathlib.Path(sys.argv[-1]).resolve()\n"
            "assert path.is_relative_to(root), path\n"
            "assert sys.stdin.isatty() and sys.stdout.isatty() and sys.stderr.isatty()\n"
            "import msvcrt\n"
            "mode = ctypes.c_ulong()\n"
            "assert ctypes.windll.kernel32.GetConsoleMode(ctypes.c_void_p(msvcrt.get_osfhandle(0)), ctypes.byref(mode))\n"
            "assert mode.value & 0x207 == 7, hex(mode.value)\n"
            "operation = os.environ.get('EDITOR_OPERATION', 'success')\n"
            "with (root / 'calls.jsonl').open('a', encoding='utf-8') as output:\n"
            "    output.write(json.dumps({'operation': operation, 'path': str(path), 'source': path.read_text(encoding='utf-8')}) + '\\n')\n"
            "if operation == 'cancel':\n"
            "    path.write_text('echo EDITOR_wrong_cancel', encoding='utf-8')\n"
            "print('EDITOR_READY:' + operation, flush=True)\n"
            "sys.stdin.readline()\n"
            "replacement = os.environ.get('EDITOR_REPLACEMENT', 'echo EDITOR_saved_🐟')\n"
            "path.write_text('' if operation == 'empty' else replacement, encoding='utf-8')\n"
            "sys.exit(7 if operation == 'fail' else 0)\n",
            encoding="utf-8",
        )
        env = dict(os.environ, HOME=str(root), USERPROFILE=str(root),
                   FISH_NATIVE_CONFIG_HOME=str(root / "config"), XDG_CONFIG_HOME=str(root / "config"),
                   XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                   TMP=str(root / "tmp"), TEMP=str(root / "tmp"), EDITOR_FIXTURE=str(root),
                   VISUAL=quote(Path(sys.executable).as_posix()) + " " + quote(editor.as_posix()))
        for name in ("TMUX", "TMUXX_ENDPOINT", "TMPDIR", "EDITOR_OPERATION", "EDITOR_REPLACEMENT"):
            env.pop(name, None)
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(base + list(arguments), cwd=root, env=env,
                                  stdin=subprocess.DEVNULL, capture_output=True, timeout=12,
                                  check=check, creationflags=subprocess.CREATE_NO_WINDOW)

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture():
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def wait_for(predicate, description):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                text = capture()
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(description + ":\n" + text[-10000:])

        def expect(line, count=1):
            return wait_for(lambda text: text.splitlines().count(line) >= count, repr(line))

        ready_counts = {}

        def expect_ready(operation):
            ready_counts[operation] = ready_counts.get(operation, 0) + 1
            expect("EDITOR_READY:" + operation, ready_counts[operation])

        def expect_input(source):
            expected = ("EDIT> " + source).rstrip()
            return wait_for(lambda text: text.rstrip().splitlines()[-1:] == [expected], expected)

        def keys(*names):
            call("send-keys", "-t", pane, *names)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")
            expect_input("")

        def event_log():
            return events.read_bytes() if events.exists() else b""

        def no_temporary_commands():
            assert not list(root.rglob("command-line.*")), "temporary command text was retained"

        owner = subprocess.Popen(base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode:
                assert owner.poll() is None, owner.stderr.read().decode(errors="replace")
                assert time.monotonic() < deadline, "private tmuxx owner did not start"
                time.sleep(0.03)
            pane = call("new-session", "-dP", "-s", "editor", "-x", "160", "-y", "45",
                        "-F", "#{pane_id}", str(fish), "-i").strip()
            expect_input("")
            assert "unsupported key" not in capture()

            before = event_log()
            keys("C-x", "C-e")
            expect_ready("success")
            literal("save")
            keys("Enter")
            expect_input("echo EDITOR_saved_🐟")
            assert event_log() == before
            no_temporary_commands()
            keys("C-_")
            expect_input("")

            original = "echo EDITOR_original_Ω"
            literal(original)
            before = event_log()
            keys("C-x", "C-e")
            expect_ready("success")
            literal("save")
            keys("Enter")
            expect_input("echo EDITOR_saved_🐟")
            assert event_log() == before, "editing submitted or cancelled the command"
            assert "EDITOR_saved_🐟" not in capture().splitlines()
            no_temporary_commands()
            keys("C-_")
            expect_input(original)
            keys("C-u")

            preferred_tmp = root / "preferred tmp Ω"
            preferred_tmp.mkdir()
            send("set -gx TMPDIR " + quote(preferred_tmp.as_posix()))

            for operation in ("fail", "empty", "cancel"):
                send(f"set -gx EDITOR_OPERATION {operation}")
                literal(original)
                keys("Home", "Right")
                before = event_log()
                keys("C-x", "C-e")
                expect_ready(operation)
                if operation == "cancel":
                    keys("C-c")
                else:
                    literal("finish")
                    keys("Enter")
                expect_input(original)
                assert event_log() == before, operation + " changed command lifecycle"
                literal("X")
                expect_input("eXcho EDITOR_original_Ω")
                keys("End", "C-u")
                no_temporary_commands()

            bash_edited = "value='β'; echo EDITOR_bash:\"$value\""
            send("set -gx EDITOR_OPERATION success; set -gx EDITOR_REPLACEMENT "
                 + quote(bash_edited) + "; fish_input_mode bash")
            bash_original = "echo ${missing:-EDITOR_bash_original}"
            literal(bash_original)
            before = event_log()
            keys("C-x", "C-e")
            expect_ready("success")
            literal("save")
            keys("Enter")
            expect_input(bash_edited)
            assert event_log() == before
            no_temporary_commands()
            records = [json.loads(line) for line in (root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
            assert Path(records[-1]["path"]).suffix == ".sh", records[-1]
            assert Path(records[-1]["path"]).is_relative_to(preferred_tmp), records[-1]
            assert records[-1]["source"].rstrip("\n") == bash_original, records[-1]
            keys("Enter")
            expect("EDITOR_bash:β")
            expect_input("")

            before = event_log()
            keys("C-x", "C-t")
            expect_ready("success")
            records = [json.loads(line) for line in (root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
            assert Path(records[-1]["path"]).suffix == ".sh", records[-1]
            # commandline appends its output newline to the draft's two newlines.
            assert records[-1]["source"] == "echo trailing\n\n\n", records[-1]
            assert "test: Missing argument" not in capture(), capture()
            literal("save")
            keys("Enter")
            expect_input(bash_edited)
            assert event_log() == before, "editing a multiline draft changed command lifecycle"
            no_temporary_commands()
            keys("C-u")
            expect_input("")

            if args.helix:
                helix = args.helix.resolve()
                helix_config = root / "helix.toml"
                helix_config.write_text('[editor]\nmouse = false\n', encoding="utf-8")
                visual = quote(helix.as_posix()) + " --config " + quote(helix_config.as_posix())
                send("fish_input_mode fish")
                send("set -gx VISUAL " + quote(visual))
                literal("echo EDITOR_before_helix")
                before = event_log()
                keys("C-x", "C-e")
                wait_for(lambda text: "command-line.fish" in text, "Helix did not open the command buffer")
                literal("%c")
                literal("echo EDITOR_helix")
                keys("Escape")
                literal(":wq")
                keys("Enter")
                expect_input("echo EDITOR_helix")
                assert event_log() == before
                assert "EDITOR_helix" not in capture().splitlines()
                no_temporary_commands()
                keys("Enter")
                expect("EDITOR_helix")
                expect_input("")

            print("PASS external editor: terminal ownership, Unicode, undo, failure/empty/cancel recovery, Bash source with trailing blank lines, no submission, cleanup" + (", real Helix" if args.helix else ""))
        finally:
            run("kill-server", check=False)
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.terminate()
                owner.wait(timeout=10)


if __name__ == "__main__":
    main()
