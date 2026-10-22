#!/usr/bin/env python3
"""Check native Fish multiline prompt cursor placement and preserved output.

Usage: python tests/native_prompt_tmuxx.py FISH_EXE TMUXX_EXE [--starship EXE]

A private tmuxx owner, copied Fish release, configuration, history, and Starship
configuration keep this check independent of existing sessions and dotfiles.
The ANSI fixture always runs; a real Starship fixture also runs when installed.
Raw pipe-pane output checks every VT operation during typing and completion
navigation, including temporary erasure that a final screen capture would miss.
"""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


def touched_rows(data, position, width, height):
    """Track the narrow VT subset ConPTY emits while editing our ASCII fixture.

    Inspect every operation, including an erase immediately followed by a write
    in the same chunk. Unknown control sequences fail instead of hiding writes.
    This is deliberately not a general terminal emulator.
    """
    text = data.decode("ascii")
    column, row = position
    saved = position
    pending_wrap = False
    touched = set()
    offset = 0

    def advance_row():
        nonlocal row
        row += 1
        if row == height:
            touched.update(range(height))
            row -= 1

    while offset < len(text):
        char = text[offset]
        if char == "\x1b":
            if text.startswith(("\x1b7", "\x1b8"), offset):
                if text[offset + 1] == "7":
                    saved = (column, row)
                else:
                    column, row = saved
                pending_wrap = False
                offset += 2
                continue
            if text.startswith("\x1b]", offset):
                end = re.search(r"\x07|\x1b\\", text[offset + 2:])
                assert end, f"incomplete OSC in {data!r}"
                offset += 2 + end.end()
                continue
            match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", text[offset:])
            assert match, f"unrecognized VT at {text[offset:]!r}"
            parameters, intermediate, command = match.groups()
            offset += match.end()
            if command == "m" or (command in "hl" and parameters in ("?12", "?25", "?2026")):
                continue
            assert not intermediate and not parameters.startswith("?"), match[0]
            values = [int(value or "0") for value in parameters.split(";")]
            number = values[0] or 1
            if command in "Hf":
                row = number - 1
                column = (values[1] or 1) - 1 if len(values) > 1 else 0
            elif command == "G":
                column = number - 1
            elif command == "d":
                row = number - 1
            elif command in "ABCD":
                if command == "A":
                    row = max(0, row - number)
                elif command == "B":
                    row = min(height - 1, row + number)
                elif command == "C":
                    column = min(width - 1, column + number)
                else:
                    column = max(0, column - number)
            elif command in "EF":
                row = max(0, min(height - 1, row + (number if command == "E" else -number)))
                column = 0
            elif command in "KX@P":
                touched.add(row)
            elif command == "J":
                mode = values[0]
                assert mode in (0, 1, 2), match[0]
                touched.update(range(row, height) if mode == 0 else
                               range(row + 1) if mode == 1 else range(height))
            elif command in "LM":
                touched.update(range(row, height))
            elif command in "ST":
                touched.update(range(height))
            elif command == "s":
                saved = (column, row)
            elif command == "u":
                column, row = saved
            else:
                raise AssertionError(f"unrecognized VT operation {match[0]!r} in {data!r}")
            pending_wrap = False
            continue
        offset += 1
        if char == "\r":
            column = 0
            pending_wrap = False
        elif char == "\n":
            advance_row()
            pending_wrap = False
        elif char == "\b":
            column = max(0, column - 1)
            pending_wrap = False
        elif char == "\t":
            column = min(width - 1, (column // 8 + 1) * 8)
            pending_wrap = False
        elif char == "\x07":
            pass
        else:
            assert " " <= char <= "~", f"unrecognized control {char!r}"
            if pending_wrap:
                advance_row()
                column = 0
            touched.add(row)
            column += 1
            pending_wrap = column == width
            column = min(column, width - 1)
    return touched, (column, row)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    parser.add_argument("--starship", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and tmuxx ConPTY")
    tmuxx = str(args.tmuxx.resolve())
    starship = args.starship or shutil.which("starship")
    if starship:
        starship = Path(starship).resolve()
    label = "fish-prompt-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-prompt-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        sink = root / "pipe_sink.py"
        sink.write_text(
            "import msvcrt, os, pathlib, sys\n"
            "msvcrt.setmode(0, os.O_BINARY)\n"
            "with open(sys.argv[1], 'wb', buffering=0) as output:\n"
            "    pathlib.Path(sys.argv[2]).touch()\n"
            "    while data := os.read(0, 65536):\n"
            "        output.write(data)\n"
            "pathlib.Path(sys.argv[3]).touch()\n",
            encoding="utf-8",
        )
        starship_config = root / "starship.toml"
        colors = ("bold #ff1122", "bold #22ff33", "bold #3344ff")
        starship_top = "".join(f"[{char}]({colors[index % len(colors)]})"
                               for index, char in enumerate("PROMPT_TOP"))
        starship_config.write_text(
            'add_newline = true\n'
            f'format = "{starship_top}$line_break$character"\n'
            '[character]\n'
            'success_symbol = "[PROMPT>](bold green)"\n'
            'error_symbol = "[PROMPT>](bold red)"\n',
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
            STARSHIP_CONFIG=str(starship_config),
            STARSHIP_CACHE=str(root / "starship-cache"),
            STARSHIP_SHELL="fish",
            TERM="xterm-256color",
        )
        env.pop("TMUX", None)
        env.pop("TMUXX_ENDPOINT", None)
        env.pop("NO_COLOR", None)
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(
                base + list(arguments), env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, timeout=10, check=check,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture(screen=False, joined=False):
            return call("capture-pane", "-p", *(["-J"] if joined else []), "-t", pane,
                        *([] if screen else ["-S", "-", "-E", "-"]))

        def cursor():
            value = call("display-message", "-pt", pane,
                         "#{cursor_x}|#{cursor_y}").strip()
            return tuple(int(part) for part in value.split("|"))

        def wait_for(predicate, description):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture()
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}; cursor={cursor()}:\n{text[-8000:]}")

        def expect_input(source):
            expected = "PROMPT> " + source
            return wait_for(
                lambda text: bool(text.rstrip().splitlines())
                and text.rstrip().splitlines()[-1].rstrip() == expected.rstrip(),
                f"missing edited input {expected!r}",
            )

        def expect_output(line):
            return wait_for(lambda text: line in text.splitlines(),
                            f"missing command output {line!r}")

        def keys(*names):
            call("send-keys", "-t", pane, *names)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        def require_cursor(expected, phase):
            deadline = time.monotonic() + 2
            actual = cursor()
            while actual != expected and time.monotonic() < deadline:
                time.sleep(0.03)
                actual = cursor()
            assert actual == expected, (
                f"{phase}: cursor {actual}, expected {expected}\n{capture().rstrip()}"
            )

        def require_sentinels():
            text = capture()
            missing = [line for line in sentinels if line not in text.splitlines()]
            assert not missing, f"editing erased preceding output {missing}:\n{text.rstrip()}"

        def record_change(description, action, settled):
            """Capture one settled edit from the actual ConPTY byte stream."""
            token = uuid.uuid4().hex
            log, ready, done = (root / (token + suffix) for suffix in (".vt", ".ready", ".done"))
            arguments = [sys.executable, "-u", str(sink), str(log), str(ready), str(done)]
            assert all(not any(char in argument for char in '\"%\r\n') for argument in arguments)
            command = " ".join('"' + argument + '"' for argument in arguments)
            call("pipe-pane", "-t", pane, command)
            deadline = time.monotonic() + 10
            while not ready.exists():
                assert time.monotonic() < deadline, "raw output sink did not become ready"
                time.sleep(0.01)
            before = capture(screen=True).splitlines()
            start = cursor()
            dimensions = tuple(int(value) for value in call(
                "display-message", "-pt", pane, "#{pane_width}|#{pane_height}"
            ).strip().split("|"))
            try:
                action()
                settled()
                # Final screen delivery and pipe delivery are independent. Wait
                # for a nonempty, flushed stream to settle before closing it.
                previous = -1
                quiet_since = time.monotonic()
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    size = log.stat().st_size
                    if size != previous:
                        previous = size
                        quiet_since = time.monotonic()
                    if size and time.monotonic() - quiet_since >= 0.1:
                        break
                    time.sleep(0.01)
                else:
                    raise AssertionError("no settled ConPTY output for " + description)
                after = capture(screen=True).splitlines()
                end = cursor()
            finally:
                call("pipe-pane", "-t", pane)
            deadline = time.monotonic() + 10
            while not done.exists():
                assert time.monotonic() < deadline, "raw output sink did not finish"
                time.sleep(0.01)
            data = log.read_bytes()
            return before, start, after, end, dimensions, data

        def unchanged_rows(description, action, settled):
            """Reject writes to rows whose visible text stays unchanged."""
            before, start, after, end, dimensions, data = record_change(description, action, settled)
            changed, actual_cursor = touched_rows(data, start, *dimensions)
            assert actual_cursor == end, (description, actual_cursor, end, data)
            protected = {row for row, (old, new) in enumerate(zip(before, after))
                         if old.strip() and old == new}
            repainted = protected & changed
            assert not repainted, (
                f"{description}: repainted unchanged rows "
                f"{[(row, before[row]) for row in sorted(repainted)]}; "
                f"raw prefix={data[:180]!r} ({len(data)} bytes)"
            )
            return len(data)

        def exercise(name, prompt_definition):
            nonlocal pane, sentinels
            sentinels = [f"PROMPT_preserved_{name}_{index}" for index in range(6)]
            (config / "config.fish").write_text(
                "set -g fish_autosuggestion_enabled 0\n"
                "function PROMPT_menu; echo PROMPT_menu_result:$argv; end\n"
                "complete -c PROMPT_menu -f -a 'alpha beta gamma zebra'\n"
                + "".join(f"echo {line}\n" for line in sentinels)
                + prompt_definition + "\n",
                encoding="utf-8",
            )
            pane = call("new-session", "-dP", "-s", name, "-x", "100", "-y", "40",
                        "-F", "#{pane_id}", str(fish), "-i").strip()
            assert pane.startswith("%"), pane
            expect_input("")
            screen = capture(screen=True).splitlines()
            prompt_row = next(index for index, line in enumerate(screen)
                              if line.rstrip() == "PROMPT>")
            # Inspect the initial cursor and several repainting keystrokes even
            # when the first is already wrong, so a failure shows upward drift
            # and lost output rather than hiding them behind the column error.
            failures = []
            byte_counts = {}

            def check_redraw(description, action, settled):
                try:
                    byte_counts[description] = unchanged_rows(name + " " + description, action, settled)
                except AssertionError as error:
                    failures.append(str(error))

            typed = ""
            for character in ("", "e", "c", "h", "o"):
                if character:
                    typed += character
                    check_redraw(f"typing {typed!r}", lambda: literal(character),
                                 lambda: expect_input(typed))
                for check in (
                    lambda: require_cursor((8 + len(typed), prompt_row),
                                           f"{name} input {typed!r}"),
                    require_sentinels,
                ):
                    try:
                        check()
                    except AssertionError as error:
                        failures.append(str(error))
            check_redraw("shortening input", lambda: keys("BSpace"), lambda: expect_input("ech"))
            check_redraw("restoring input", lambda: literal("o"), lambda: expect_input("echo"))

            literal(" PROMPT_command_result")
            expect_input("echo PROMPT_command_result")
            require_cursor((8 + len("echo PROMPT_command_result"), prompt_row),
                           "completed command")
            keys("Enter")
            expect_output("PROMPT_command_result")
            expect_input("")
            sentinels.append("PROMPT_command_result")
            require_sentinels()
            next_row = cursor()[1]
            require_cursor((8, next_row), "prompt after command output")
            assert next_row > prompt_row, "prompt moved above its completed command"

            # Incomplete input adds editable rows below the complete prompt.
            send("begin")
            wait_for(lambda text: text.rstrip().endswith("PROMPT> begin"),
                     "missing incomplete block")
            require_cursor((4, next_row + 1), "indented continuation")
            literal("echo PROMPT_multiline_result")
            wait_for(lambda text: "    echo PROMPT_multiline_result" in text,
                     "missing multiline input")
            require_cursor((4 + len("echo PROMPT_multiline_result"), next_row + 1),
                           "multiline command")
            keys("Home")
            require_cursor((0, next_row + 1), "multiline Home")
            keys("Up", "End")
            require_cursor((8 + len("begin"), next_row), "earlier input row")
            keys("Down", "End", "Enter")
            send("end")
            expect_output("PROMPT_multiline_result")
            expect_input("")
            sentinels.append("PROMPT_multiline_result")
            require_sentinels()

            # Opening/filtering/cancelling changes menu rows; navigation changes
            # only the old and new selection. The raw stream must leave every
            # identical visible row, including prompt and input, untouched.
            literal("PROMPT_menu ")
            expect_input("PROMPT_menu ")

            def menu(selected=None):
                return wait_for(
                    lambda text: "Search:" in text and "zebra" in text
                    and (selected is None or "> " + selected in text.splitlines()),
                    f"missing completion menu selection {selected!r}",
                )

            check_redraw("opening menu", lambda: keys("Tab"), menu)
            check_redraw("selecting alpha", lambda: keys("Tab"), lambda: menu("alpha"))
            check_redraw("selecting beta", lambda: keys("C-n"), lambda: menu("beta"))
            check_redraw("selecting alpha again", lambda: keys("C-p"), lambda: menu("alpha"))
            check_redraw("filtering menu", lambda: literal("gam"),
                         lambda: wait_for(lambda text: "Search: gam_" in text and "> gamma" in text,
                                          "missing filtered menu"))
            assert not any(line.strip() in ("alpha", "beta", "zebra", "> alpha", "> beta", "> zebra")
                           for line in capture(screen=True).splitlines()), "stale candidates after filtering"
            check_redraw("cancelling menu", lambda: keys("C-g"),
                         lambda: expect_input("PROMPT_menu "))
            assert not any(line.startswith("Search:") or line.strip() == "> gamma"
                           for line in capture(screen=True).splitlines()), "stale cancelled menu"
            keys("C-c")
            expect_input("")
            assert not failures, "\n\n".join(failures)

            # A resize must not leave layout from the old prompt width behind.
            current_row = cursor()[1]
            literal("echo PROMPT_resize")
            expect_input("echo PROMPT_resize")
            # -J retains significant trailing spaces. Clearing a tail must not
            # turn it into content that reflows into blank rows when narrowed.
            visible = "PROMPT> echo PROMPT_resize"
            joined = capture(screen=True, joined=True).splitlines()
            assert [line for line in joined if line.rstrip() == visible] == [visible], joined
            call("resize-window", "-t", name, "-x", "60", "-y", "40")
            literal("_kept")
            expect_input("echo PROMPT_resize_kept")
            require_cursor((8 + len("echo PROMPT_resize_kept"), current_row),
                           "input after resize")
            require_sentinels()
            check_redraw("typing after resize", lambda: literal("_again"),
                         lambda: expect_input("echo PROMPT_resize_kept_again"))
            keys("Enter")
            expect_output("PROMPT_resize_kept_again")
            expect_input("")

            # Explicit clearing may redraw everything, but must seed a new
            # stable frame: the next edit should preserve the restored header.
            literal("echo PROMPT_clear")
            keys("C-l")
            expect_input("echo PROMPT_clear")
            assert capture(screen=True).splitlines().count("PROMPT_TOP") == 1
            check_redraw("typing after Ctrl-L", lambda: literal("_kept"),
                         lambda: expect_input("echo PROMPT_clear_kept"))
            keys("C-c")
            expect_input("")

            # Opening a menu at the bottom must scroll. Once it fits, repeated
            # selection changes should again touch only changed candidate rows.
            send("for item in " + " ".join(str(index) for index in range(45))
                 + "; echo PROMPT_scroll_$item; end")
            expect_output("PROMPT_scroll_44")
            expect_input("")
            assert cursor()[1] == 39, ("prompt did not reach screen bottom", cursor())
            literal("PROMPT_menu ")
            keys("Tab")
            menu()
            assert cursor()[1] < 39, ("opening menu did not scroll input upward", cursor())
            for key, selected in (("C-n", "alpha"), ("C-n", "beta"), ("C-p", "alpha")):
                check_redraw("bottom menu " + selected, lambda: keys(key),
                             lambda: menu(selected))
            check_redraw("cancelling bottom menu", lambda: keys("C-g"),
                         lambda: expect_input("PROMPT_menu "))
            keys("C-c")
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
            assert not failures, "\n\n".join(failures)
            first_key = byte_counts["typing 'e'"]
            print(f"PASS native prompt {name}: cursor/output, unchanged prompt/menu rows, "
                  "multiline, resize, Ctrl-L, bottom scrolling; "
                  f"typing {first_key} B, selection {byte_counts['selecting beta']} B")

        def exercise_margin():
            nonlocal pane, sentinels
            sentinels = [f"PROMPT_preserved_margin_{index}" for index in range(6)]
            prompt = "PROMPT_EDGE".ljust(100, ".")
            assert len(prompt) == 100
            (config / "config.fish").write_text(
                "set -g fish_autosuggestion_enabled 0\n"
                + "".join(f"echo {line}\n" for line in sentinels)
                + "function fish_prompt; "
                + f"printf '\\n\\e[38;2;111;222;123m{prompt}\\e[0m'; end\n",
                encoding="utf-8",
            )
            pane = call("new-session", "-dP", "-s", "margin", "-x", "100", "-y", "40",
                        "-F", "#{pane_id}", str(fish), "-i").strip()
            wait_for(lambda text: prompt in text.splitlines(), "missing exact-width prompt")
            prompt_row = capture(screen=True).splitlines().index(prompt)
            require_cursor((0, prompt_row + 1), "exact-width prompt before input")
            typed = ""
            for character in "echo":
                literal(character)
                typed += character
                wait_for(lambda text: text.rstrip().splitlines()[-1] == typed,
                         "missing input after exact-width prompt")
                require_cursor((len(typed), prompt_row + 1),
                               f"exact-width prompt input {typed!r}")
                screen = capture(screen=True).splitlines()
                assert screen[prompt_row] == prompt, screen
                require_sentinels()
            keys("Home")
            require_cursor((0, prompt_row + 1), "exact-width prompt Home")
            keys("End")
            literal(" PROMPT_edge_result")
            keys("Enter")
            expect_output("PROMPT_edge_result")
            wait_for(lambda text: text.rstrip().splitlines()[-1] == prompt,
                     "missing exact-width prompt after execution")
            require_sentinels()
            send("exit")
            print("PASS native prompt margin: exact-width ANSI prompt and first keystrokes")

        pane = ""
        sentinels = []
        owner = subprocess.Popen(
            base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode != 0:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                if time.monotonic() >= deadline:
                    raise AssertionError("isolated tmuxx owner did not become ready")
                time.sleep(0.03)
            # Enough nonprinting color bytes to wrap if they are incorrectly
            # included in layout; the visible top row remains just ten cells.
            ansi_top = "".join("\\e[38;2;111;222;123m" + char for char in "PROMPT_TOP")
            fixtures = [("ansi", "function fish_prompt; "
                         f"printf '\\n{ansi_top}\\e[0m\\n\\e[1;32mPROMPT> \\e[0m'; end")]
            if starship:
                # Exercise Starship's own SGR output without reading user config
                # or requiring unrelated Starship initialization functionality.
                executable = starship.as_posix().replace("'", "\\'")
                fixtures.append(("starship", "function fish_prompt; "
                                 f"'{executable}' prompt --status=$status; end"))
            else:
                print("SKIP actual Starship: executable not found; ANSI fixture still runs")
            failures = []
            for name, definition in fixtures:
                try:
                    exercise(name, definition)
                except AssertionError as error:
                    failures.append(f"{name}: {error}")
            try:
                exercise_margin()
            except AssertionError as error:
                failures.append(f"margin: {error}")
            assert not failures, "\n\n".join(failures)
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
