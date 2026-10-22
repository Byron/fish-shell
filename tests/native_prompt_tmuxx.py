#!/usr/bin/env python3
"""Check native Fish multiline prompt cursor placement and preserved output.

Usage: python tests/native_prompt_tmuxx.py FISH_EXE TMUXX_EXE [--starship EXE]

A private tmuxx owner, copied Fish release, configuration, history, and Starship
configuration keep this check independent of existing sessions and dotfiles.
The ANSI fixture always runs; a real Starship fixture also runs when installed.
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

        def capture(screen=False):
            return call("capture-pane", "-p", "-t", pane,
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

        def exercise(name, prompt_definition):
            nonlocal pane, sentinels
            sentinels = [f"PROMPT_preserved_{name}_{index}" for index in range(6)]
            (config / "config.fish").write_text(
                "set -g fish_autosuggestion_enabled 0\n"
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
            typed = ""
            for character in ("", "e", "c", "h", "o"):
                if character:
                    literal(character)
                    typed += character
                    expect_input(typed)
                for check in (
                    lambda: require_cursor((8 + len(typed), prompt_row),
                                           f"{name} input {typed!r}"),
                    require_sentinels,
                ):
                    try:
                        check()
                    except AssertionError as error:
                        failures.append(str(error))
            assert not failures, "\n\n".join(failures)

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

            # A resize must not leave layout from the old prompt width behind.
            current_row = cursor()[1]
            literal("echo PROMPT_resize")
            expect_input("echo PROMPT_resize")
            call("resize-window", "-t", name, "-x", "60", "-y", "40")
            literal("_kept")
            expect_input("echo PROMPT_resize_kept")
            require_cursor((8 + len("echo PROMPT_resize_kept"), current_row),
                           "input after resize")
            require_sentinels()
            keys("Enter")
            expect_output("PROMPT_resize_kept")
            expect_input("")
            send("exit")
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
            print(f"PASS native prompt {name}: cursor, output, multiline editing, resize")

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
