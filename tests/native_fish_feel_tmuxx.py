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
            "function fish_prompt; printf 'FEEL> '; end\n"
            "set -g FEEL_variable FEEL_variable_value\n",
            encoding="utf-8",
        )
        directory = root / "FEEL_path_directory"
        directory.mkdir()
        (root / "FEEL_path_file.txt").write_text("not a directory\n", encoding="utf-8")
        commands = root / "commands"
        commands.mkdir()
        shutil.copy2(fish, commands / "FEEL-context-probe.exe")
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        env = dict(
            os.environ,
            HOME=temporary,
            USERPROFILE=temporary,
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"),
            XDG_CACHE_HOME=str(root / "cache"),
            PATH=str(commands) + os.pathsep + os.environ.get("PATH", ""),
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

            def metadata(format_string):
                return call("display-message", "-pt", pane, format_string).strip()

            def expect_metadata(format_string, expected):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    actual = metadata(format_string)
                    if format_string == "#{pane_current_path}":
                        actual = actual.replace("\\", "/").rstrip("/")
                        expected = expected.rstrip("/")
                    if actual == expected:
                        return
                    time.sleep(0.03)
                raise AssertionError(f"metadata {format_string}: {actual!r} != {expected!r}")

            expect_metadata("#{pane_current_path}", str(root).replace("\\", "/"))
            assert metadata("#{pane_last_prompt_time}"), "prompt marker not reported"
            send("false")
            expect_input("")
            expect_metadata("#{pane_command_line}|#{pane_command_status}|#{pane_command_running}",
                            "false|1|0")
            literal("echo FEEL_metadata_cancelled")
            keys("C-c")
            expect_input("")
            expect_metadata("#{pane_command_line}", "false")

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
            send("printf '<%s>\\n' 'FEEL_quote")
            send("continued'")
            expect("<FEEL_quote")
            expect("continued>")

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

            literal("echo $FEEL_var")
            keys("Tab", "Enter")
            expect("FEEL_variable_value")
            literal("command FEEL-context-p")
            keys("Tab")
            literal(" -N -c 'echo FEEL_context_command'")
            keys("Enter")
            expect("FEEL_context_command")

            # A file with the same prefix must not compete with cd's directory.
            literal("cd FEEL_path_")
            keys("Tab", "Enter")
            expect_metadata("#{pane_current_path}", str(directory).replace("\\", "/"))
            send("pwd")
            wait_for(lambda text: str(directory).replace("\\", "/") in
                     [line.replace("\\", "/") for line in text.splitlines()],
                     "directory-only cd completion did not enter the matching directory")
            send("cd ..")
            expect_metadata("#{pane_current_path}", str(root).replace("\\", "/"))

            send("function FEEL_context; echo FEEL_declared $argv; end")
            send("complete -c FEEL_context -f -a 'alpha beta'")
            send("complete -c FEEL_context -l mode -r -a 'fast slow'")
            literal("FEEL_context al")
            keys("Tab", "Enter")
            expect("FEEL_declared alpha")
            literal("FEEL_context --mo")
            keys("Tab")
            literal(" fa")
            keys("Tab", "Enter")
            expect("FEEL_declared --mode fast")

            send("function FEEL_pager; echo FEEL_pager_result:$argv; end")
            send("complete -c FEEL_pager -f -a 'alpha beta 東京 zebra'")
            expect_input("")
            literal("FEEL_pager ")
            keys("Tab")
            wait_for(lambda text: "Search:" in text and "zebra" in text,
                     "Tab did not open completion menu")
            literal("BT")
            wait_for(lambda text: "Search: BT_" in text and "> beta" in text,
                     "typing did not filter completion menu")
            keys("Enter")
            expect_input("FEEL_pager beta ")
            assert "FEEL_pager_result:beta" not in capture().splitlines()
            keys("Enter")
            expect("FEEL_pager_result:beta")

            literal("FEEL_pager ")
            keys("Tab", "BTab", "Enter")
            expect_input("FEEL_pager 東京 ")
            keys("Enter")
            expect("FEEL_pager_result:東京")

            # Menu cancellation preserves the exact cursor and suffix.
            literal("FEEL_pager  tail")
            keys("Home", *("Right" for _ in "FEEL_pager "), "Tab")
            literal("nothing_matches")
            wait_for(lambda text: "No matches" in text, "menu filter did not update")
            keys("Escape")
            literal("alpha")
            keys("End", "Enter")
            expect("FEEL_pager_result:alpha FEEL_pager_result:tail")

            send("abbr --add fe 'echo FEEL_abbr'")
            literal("fe ")
            expect_input("echo FEEL_abbr ")
            literal("space")
            keys("Enter")
            expect("FEEL_abbr space")
            literal("fe")
            keys("Enter")
            # Expansion can suggest the earlier suffix, but neither Enter
            # accepts that suggestion: the second executes just the expansion.
            expect_input("echo FEEL_abbr space")
            assert "FEEL_abbr" not in capture().splitlines()
            keys("Enter")
            expect("FEEL_abbr")
            assert capture().splitlines().count("FEEL_abbr space") == 1
            send("echo fe argument")
            expect("fe argument")

            send("bind ctrl-t complete")
            literal("ech")
            keys("C-t")
            literal("FEEL_bound_completion")
            keys("Enter")
            expect("FEEL_bound_completion")
            send("bind --erase ctrl-t")
            literal("ecoh")
            keys("C-t")
            literal(" FEEL_binding_restored")
            keys("Enter")
            expect("FEEL_binding_restored")

            send("bind tab backward-char")
            literal("echo FEEL_tab_X")
            keys("Tab")
            literal("Y")
            keys("Enter")
            expect("FEEL_tab_YX")
            send("bind --erase tab")
            literal("ech")
            keys("Tab")
            literal("FEEL_tab_restored")
            keys("Enter")
            expect("FEEL_tab_restored")

            send("fish_vi_key_bindings")
            expect_input("")
            literal("echo FEEL_vi_X")
            keys("Escape")
            literal("xAok")
            keys("Enter")
            expect("FEEL_vi_ok")
            literal("echo FEEL_vi_ay")
            keys("Left", "Left", "Right")
            literal("X")
            keys("End", "Enter")
            expect("FEEL_vi_aXy")
            suggestion("FEEL_vi_hint", "Right")
            send("echo FEEL_vi_word alpha beta")
            expect("FEEL_vi_word alpha beta")
            literal("echo FEEL_vi_word")
            keys("M-f")
            literal(" FEEL_partial")
            keys("Enter")
            expect("FEEL_vi_word alpha FEEL_partial")
            send("begin")
            send("echo FEEL_vi_multiline")
            send("end")
            expect("FEEL_vi_multiline")
            send("bind x forward-char")
            literal("echo FEEL_vi_binding_XY")
            keys("Escape")
            literal("hxrZ")
            keys("Enter")
            expect("FEEL_vi_binding_XZ")
            send("bind --erase --all")
            send("fish_default_key_bindings")
            expect_input("")

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

            send("set -g fish_color_command magenta; set -g fish_color_param green")
            expect_input("")
            source = "echo FEEL_custom_palette"
            styles = styled_input(source)
            assert styles[0] == 35, styles
            assert styles[source.index("FEEL_")] == 32, styles
            send("set -e fish_color_command; set -e fish_color_param")
            expect_input("")
            send("function fish_prompt; set_color green; printf 'FEEL> '; set_color normal; end")
            expect_input("")
            send("echo FEEL_disabled_hint suffix")
            expect("FEEL_disabled_hint suffix")
            send("set -g fish_autosuggestion_enabled 0")
            expect_input("")
            literal("echo FEEL_disabled_hint")
            expect_input("echo FEEL_disabled_hint")
            keys("C-c")
            send("set -e fish_autosuggestion_enabled")
            expect_input("")

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
                        "editable multiline/cancellation, variable/command/cd/declarative "
                        "completion and searchable menu, abbreviations, custom bindings and vi, "
                        "custom colors/prompts, terminal metadata")
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
