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
        mode_probe = root / "console_mode.py"
        mode_probe.write_text(
            "import ctypes, msvcrt\n"
            "mode = ctypes.c_ulong()\n"
            "assert ctypes.windll.kernel32.GetConsoleMode("
            "ctypes.c_void_p(msvcrt.get_osfhandle(0)), ctypes.byref(mode))\n"
            "assert mode.value & 0x207 == 7, hex(mode.value)\n"
            "print('FEEL_console_mode_restored')\n",
            encoding="utf-8",
        )
        commands = root / "commands"
        commands.mkdir()
        shutil.copy2(fish, commands / "FEEL-context-probe.exe")
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        env = dict(
            os.environ,
            HOME=temporary,
            USERPROFILE=temporary,
            XDG_CONFIG_HOME=str(root / "config"), FISH_NATIVE_CONFIG_HOME=str(root / "config"),
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
            if names and names[-1] == "Escape":
                # Distinguish a standalone Escape from Alt+the next character.
                time.sleep(0.06)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        def configure(source):
            # Keymap, abbreviation and terminal mode tests need the new reader
            # frame before sending their next control key or bracketed paste.
            submitted = "FEEL> " + source
            previous = capture().splitlines().count(submitted)
            send(source)
            wait_for(
                lambda text: text.splitlines().count(submitted) > previous and
                text.rstrip().splitlines()[-1].rstrip() == "FEEL>",
                f"configuration did not reach a fresh prompt: {source!r}",
            )

        def paste(source):
            # Exercise terminal mode negotiation too: tmuxx emits delimiters
            # only when the pane has enabled bracketed paste.
            path = root / "paste.txt"
            path.write_bytes(source.encode("utf-8"))
            call("load-buffer", "-b", "feel", str(path))
            call("paste-buffer", "-p", "-r", "-b", "feel", "-t", pane)

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
            cursor_before = int(metadata("#{cursor_x}"))
            keys(acceptance)
            wait_for(lambda _: metadata("#{cursor_x}") == str(cursor_before + len(" alpha beta")),
                     "accepting the hint did not advance the input cursor")
            text = expect_input(source)
            assert text.splitlines().count(output) == 1, "accepting a hint executed the command"
            keys("Enter")
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
            suggestion("FEEL_hint_ctrl_e_魚", "C-e")
            literal("echo FEEL_end_of_line")
            keys("Left", "Left", "C-e")
            literal("_moved")
            keys("Enter")
            expect("FEEL_end_of_line_moved")
            configure("bind ctrl-e end-of-line")
            suggestion("FEEL_hint_bound_end", "C-e")
            configure("bind --erase ctrl-e")
            paste("history append 'echo FEEL_hint_line first\necho FEEL_hint_extra'")
            keys("Enter")
            expect_input("")
            literal("echo FEEL_hint_line")
            wait_for(lambda text: "echo FEEL_hint_extra" in text.rstrip().splitlines()[-1:],
                     "missing multiline autosuggestion")
            # The second press must not accept the newline or the next command.
            keys("C-e", "C-e")
            literal("_edited")
            expect_input("echo FEEL_hint_line first_edited")
            assert "FEEL_hint_line first" not in capture().splitlines()
            keys("Enter")
            expect("FEEL_hint_line first_edited")
            assert "FEEL_hint_extra" not in capture().splitlines()
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
            keys("Up", "C-e", "C-w")
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

            # Ctrl-N/P belong to the open menu in either input dialect, then
            # resume normal history navigation without losing the edited draft.
            configure("set -g fish_autosuggestion_enabled 0")
            for input_mode in ("fish", "bash"):
                configure(f"fish_input_mode {input_mode}")
                older = f"echo FEEL_ctrl_history_{input_mode}_older"
                newer = f"echo FEEL_ctrl_history_{input_mode}_newer"
                send(older)
                expect(older.removeprefix("echo "))
                send(newer)
                expect(newer.removeprefix("echo "))
                result = "FEEL_pager_result:zebra"
                before = capture().splitlines().count(result)

                def expect_menu(query, selected, count):
                    def matches(text):
                        # Inspect only the active menu, never stale scrollback.
                        active = text.rsplit("FEEL> ", 1)[-1].splitlines()
                        return (bool(active) and active[0].rstrip() == "FEEL_pager"
                                and any(f"Search: {query}_  ({count}/4)" in line
                                        for line in active)
                                and f"> {selected}" in active)

                    text = wait_for(
                        matches,
                        f"{input_mode}: Ctrl-N/P did not select {selected!r} "
                        f"in completion menu filtered by {query!r}",
                    )
                    assert text.splitlines().count(result) == before, \
                        "menu navigation executed the command"

                literal("FEEL_pager ")
                keys("Tab", "C-n")
                expect_menu("", "alpha", 4)
                keys("C-n")
                expect_menu("", "beta", 4)
                keys("C-p")
                expect_menu("", "alpha", 4)
                keys("C-p")
                expect_menu("", "東京", 4)
                literal("a")
                expect_menu("a", "alpha", 3)
                keys("C-n")
                expect_menu("a", "beta", 3)
                keys("C-n")
                expect_menu("a", "zebra", 3)
                keys("C-n")
                expect_menu("a", "alpha", 3)
                keys("C-p")
                expect_menu("a", "zebra", 3)
                keys("Enter")
                expect_input("FEEL_pager zebra ")
                assert capture().splitlines().count(result) == before, \
                    "accepting a completion executed the command"

                keys("C-p")
                expect_input(newer)
                keys("C-p")
                expect_input(older)
                keys("C-n")
                expect_input(newer)
                keys("C-n")
                expect_input("FEEL_pager zebra ")
                keys("Enter")
                expect(result, count=before + 1)

                literal("FEEL_pager ")
                keys("Tab", "C-n", "C-e")
                expect_input("FEEL_pager ")
                keys("C-p")
                expect_input("FEEL_pager zebra ")
                keys("C-n")
                expect_input("FEEL_pager ")
                keys("C-c")
                expect_input("")
            configure("fish_input_mode fish")
            configure("set -e fish_autosuggestion_enabled")

            # Menu cancellation preserves the exact cursor and suffix.
            literal("FEEL_pager  tail")
            keys("Home", *("Right" for _ in "FEEL_pager "), "Tab")
            literal("nothing_matches")
            wait_for(lambda text: "No matches" in text, "menu filter did not update")
            keys("Escape")
            literal("alpha")
            keys("End", "Enter")
            expect("FEEL_pager_result:alpha FEEL_pager_result:tail")

            configure("set -g fish_autosuggestion_enabled 0")
            paste("printf '<%s>\\n' 'FEEL_paste_🐟\r\nline'\r\necho FEEL_paste_second\n")
            wait_for(lambda text: "echo FEEL_paste_second" in text,
                     "multiline terminal paste did not enter the edit buffer")
            assert "FEEL_paste_second" not in capture().splitlines()
            keys("Enter")
            expect("<FEEL_paste_🐟")
            expect("line>")
            expect("FEEL_paste_second")
            # Command output precedes the next reader's bracketed-paste mode
            # negotiation. Wait for that frame before asking tmuxx to paste.
            expect_input("")

            literal("echo ")
            paste("FEEL_paste_undo\necho FEEL_paste_never")
            wait_for(lambda text: "echo FEEL_paste_never" in text,
                     "paste undo fixture was not inserted")
            keys("C-_")
            expect_input("echo ")
            keys("C-c")
            expect_input("")

            # A fragmented paste remains one pending edit across a resize.
            literal("\x1b[200~echo FEEL_paste_fragment\r\n")
            call("resize-window", "-t", "feel", "-x", "90", "-y", "50")
            literal("echo FEEL_paste_after_resize\x1b[20")
            assert "FEEL_paste_fragment" not in capture().splitlines()
            literal("1~")
            wait_for(lambda text: "echo FEEL_paste_after_resize" in text,
                     "fragmented paste did not finish after resize")
            assert "FEEL_paste_after_resize" not in capture().splitlines()
            keys("Enter")
            expect("FEEL_paste_fragment")
            expect("FEEL_paste_after_resize")
            call("resize-window", "-t", "feel", "-x", "140", "-y", "50")
            send(f"python '{mode_probe.as_posix()}'")
            expect("FEEL_console_mode_restored")

            literal("FEEL_pager ")
            keys("Tab")
            wait_for(lambda text: "Search:" in text, "paste menu fixture did not open")
            paste("alpha")
            expect_input("FEEL_pager alpha")
            keys("Enter")
            expect("FEEL_pager_result:alpha")
            configure("set -e fish_autosuggestion_enabled")

            configure("abbr --add fe 'echo FEEL_abbr'")
            paste("fe ")
            expect_input("fe ")
            keys("C-c")
            expect_input("")
            literal("fe ")
            expect_input("echo FEEL_abbr ")
            literal("space")
            keys("Enter")
            expect("FEEL_abbr space")
            literal("fe")
            keys("Enter")
            # Enter expands and submits the accepted source without accepting
            # the earlier command's history suggestion as an extra argument.
            expect("FEEL_abbr")
            assert capture().splitlines().count("FEEL_abbr space") == 1
            paste("fe suffix")
            keys("Home", "Right", "Right", "Enter")
            expect("FEEL_abbr suffix")
            configure("abbr --add fb begin")
            literal("fb")
            keys("Enter")
            keys("C-_")
            expect_input("fb")
            keys("C-c")
            expect_input("")
            literal("fb")
            keys("Enter")
            literal("echo FEEL_abbr_block")
            wait_for(lambda text: "    echo FEEL_abbr_block" in text,
                     "unfinished abbreviation did not indent continued input")
            keys("Enter")
            send("end")
            expect("FEEL_abbr_block")
            configure("abbr --add multi 'echo FEEL_abbr_first; echo FEEL_abbr_second'")
            send("multi")
            expect("FEEL_abbr_first")
            expect("FEEL_abbr_second")
            send("echo fe argument")
            expect("fe argument")

            configure("bind ctrl-t complete")
            literal("ech")
            keys("C-t")
            literal("FEEL_bound_completion")
            keys("Enter")
            expect("FEEL_bound_completion")
            configure("bind --erase ctrl-t")
            literal("ecoh")
            keys("C-t")
            literal(" FEEL_binding_restored")
            keys("Enter")
            expect("FEEL_binding_restored")

            configure("bind tab backward-char")
            literal("echo FEEL_tab_X")
            keys("Tab")
            literal("Y")
            keys("Enter")
            expect("FEEL_tab_YX")
            configure("bind --erase tab")
            literal("ech")
            keys("Tab")
            literal("FEEL_tab_restored")
            keys("Enter")
            expect("FEEL_tab_restored")

            configure("fish_vi_key_bindings")
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
            suggestion("FEEL_vi_hint_ctrl_e", "C-e")
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
            configure("bind x forward-char")
            literal("echo FEEL_vi_binding_XY")
            keys("Escape")
            literal("hxrZ")
            keys("Enter")
            expect("FEEL_vi_binding_XZ")
            configure("bind --erase --all")
            paste("echo FEEL_vi_paste_")
            literal("after")
            keys("Enter")
            expect("FEEL_vi_paste_after")
            configure("fish_default_key_bindings")

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

            configure("set -g fish_color_command magenta; set -g fish_color_param green")
            source = "echo FEEL_custom_palette"
            styles = styled_input(source)
            assert styles[0] == 35, styles
            assert styles[source.index("FEEL_")] == 32, styles
            configure("set -e fish_color_command; set -e fish_color_param")
            configure("function fish_prompt; set_color green; printf 'FEEL> '; set_color normal; end")
            send("echo FEEL_disabled_hint suffix")
            expect("FEEL_disabled_hint suffix")
            configure("set -g fish_autosuggestion_enabled 0")
            literal("echo FEEL_disabled_hint")
            expect_input("echo FEEL_disabled_hint")
            keys("C-c")
            configure("set -e fish_autosuggestion_enabled")

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
                        "completion and searchable menu with Ctrl-N/P in Fish/Bash modes, "
                        "bracketed paste/undo/resize, "
                        "abbreviations, custom bindings and vi, "
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
