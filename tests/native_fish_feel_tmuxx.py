#!/usr/bin/env python3
"""Check native Fish editing behavior in an isolated tmuxx ConPTY session.

Usage: python tests/native_fish_feel_tmuxx.py FISH_EXE TMUXX_EXE

The private owner, configuration, history, command PATH and working directory
leave existing shells and user configuration untouched. Suggestions are checked
before acceptance as well as after editing; displaying a suggestion must never
execute it.
"""

import argparse
import os
from pathlib import Path
import re
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
        parser.error("this test requires native Windows and tmuxx ConPTY")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-feel-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-feel-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        (config / "config.fish").write_text(
            "function fish_prompt; printf 'FEEL> '; end\n",
            encoding="utf-8",
        )
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        env = dict(
            os.environ,
            HOME=temporary,
            USERPROFILE=temporary,
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"),
            XDG_CACHE_HOME=str(root / "cache"),
        )
        env.pop("TMUX", None)
        env.pop("TMUXX_ENDPOINT", None)
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(
                base + list(arguments),
                env=env,
                cwd=root,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=10,
                check=check,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture(escaped=False):
            return call("capture-pane", "-p", *(["-e"] if escaped else []),
                        "-t", pane, "-S", "-", "-E", "-")

        def wait_for(predicate, description, escaped=False):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture(escaped)
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}:\n{text[-8000:]}")

        def expect(line, count=1):
            return wait_for(lambda text: text.splitlines().count(line) >= count,
                            f"missing output {line!r} ({count} times)")

        def expect_input(source):
            expected = "FEEL> " + source

            def matches(text):
                lines = text.rstrip().splitlines()
                return bool(lines) and lines[-1].rstrip() == expected.rstrip()

            return wait_for(
                matches,
                f"missing edited input {expected!r}",
            )

        def keys(*names):
            call("send-keys", "-t", pane, *names)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        def suggestion(marker, acceptance):
            source = f"echo {marker} alpha beta"
            output = f"{marker} alpha beta"
            send(source)
            expect(output)
            literal(f"echo {marker}")
            text = expect_input(source)
            assert text.splitlines().count(output) == 1, "hint executed before acceptance"
            # tmuxx capture -e preserves cell colors, including the autosuggestion.
            colored = capture(escaped=True).rstrip().splitlines()[-1]
            assert re.search(r"\x1b\[[0-9;:]*m", colored), colored
            keys(acceptance, "Enter")
            expect(output, count=2)

        owner = subprocess.Popen(
            base + ["-D", "-f", str(tmux_config)],
            env=env,
            cwd=root,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode != 0:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                if time.monotonic() >= deadline:
                    raise AssertionError("isolated tmuxx owner did not become ready")
                time.sleep(0.03)
            pane = call("new-session", "-dP", "-s", "feel", "-x", "140", "-y", "50",
                        "-F", "#{pane_id}", str(fish), "-i").strip()
            assert pane.startswith("%"), pane
            expect_input("")

            suggestion("FEEL_hint_right", "Right")
            suggestion("FEEL_hint_ctrl_f", "C-f")
            for marker, key in [("FEEL_hint_word", "M-f"),
                                ("FEEL_hint_alt_right", "M-Right")]:
                send(f"echo {marker} alpha beta")
                expect(f"{marker} alpha beta")
                literal(f"echo {marker}")
                expect_input(f"echo {marker} alpha beta")
                keys(key)
                literal(" FEEL_partial")
                keys("Enter")
                expect(f"{marker} alpha FEEL_partial")

            # A typed substring selects matching history and Down restores the draft.
            send("echo FEEL_search first")
            expect("FEEL_search first")
            send("echo FEEL_between")
            expect("FEEL_between")
            send("echo FEEL_search second")
            expect("FEEL_search second")
            literal("FEEL_search")
            keys("Up")
            expect_input("echo FEEL_search second")
            keys("Up")
            expect_input("echo FEEL_search first")
            keys("Down")
            expect_input("echo FEEL_search second")
            keys("Down")
            expect_input("FEEL_search")
            keys("C-c")
            expect_input("")

            # Incomplete source stays in one editable buffer. Change an earlier
            # line before closing the block and make sure the old text never runs.
            send("for item in FEEL_multiline_wrong")
            keys("Up", "End", "C-w")
            literal("FEEL_multiline_correct")
            keys("Down")
            send("echo $item")
            send("end")
            expect("FEEL_multiline_correct")
            assert "FEEL_multiline_wrong" not in capture().splitlines()

            # Enter evaluates syntax errors instead of trapping the user in a
            # buffer that can never be completed; Ctrl-C also discards blocks.
            send("end")
            expect_input("")
            send("begin")
            literal("echo FEEL_cancelled_block")
            keys("C-c")
            send("echo FEEL_after_block_cancel")
            expect("FEEL_after_block_cancel")
            assert "FEEL_cancelled_block" not in capture().splitlines()

            # Inspect colors attached to individual input characters. Capture
            # may combine SGR attributes, so do not depend on escape spelling.
            def styled_input(source):
                literal(source)
                expect_input(source)
                colored = capture(escaped=True).rstrip().splitlines()[-1]
                foreground = None
                styles = []
                offset = 0
                for match in re.finditer(r"\x1b\[([0-9;]*)m", colored):
                    styles.extend((char, foreground) for char in colored[offset:match.start()])
                    parameters = [int(value or "0") for value in match[1].split(";")]
                    for value in parameters:
                        if value in (0, 39):
                            foreground = None
                        elif 30 <= value <= 37 or 90 <= value <= 97:
                            foreground = value
                    offset = match.end()
                styles.extend((char, foreground) for char in colored[offset:])
                plain = "".join(char for char, _ in styles)
                start = plain.index(source)
                keys("C-c")
                expect_input("")
                return [color for _, color in styles[start:start + len(source)]]

            source = "echo 'FEEL_color' $FEEL_variable # comment"
            styles = styled_input(source)
            assert styles[0] in (34, 94), (source, styles)
            assert styles[source.index("'")] in (33, 93), (source, styles)
            assert styles[source.index("$")] in (36, 96), (source, styles)
            assert styles[source.index("#")] in (90, 37), (source, styles)
            invalid = "FEEL_definitely_missing_command"
            styles = styled_input(invalid)
            assert styles[0] in (31, 91), (invalid, styles)

            send("exit 0")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                state = call("display-message", "-pt", pane,
                             "#{pane_dead}|#{pane_dead_status}").strip()
                if state.startswith("1|"):
                    assert state == "1|0", (state, capture())
                    break
                time.sleep(0.03)
            else:
                raise AssertionError(f"native Fish did not exit:\n{capture()}")
            coverage = ("visible/color hints, full/word acceptance, substring history/draft restore, "
                        "editable multiline/cancellation, syntax/error colors")
            print("PASS native Fish feel: " + coverage)
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
