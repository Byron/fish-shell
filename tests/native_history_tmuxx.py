#!/usr/bin/env python3
"""Exercise native history through private Fish/Bash panes and command invocations.

Usage: python tests/native_history_tmuxx.py FISH_EXE TMUXX_EXE

All configuration, history files and tmuxx sessions live in a temporary fixture.
Leading-space harness commands avoid adding the test plumbing to shell history.
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


def quote(value):
    """A literal word accepted by Fish and Bash-style input."""
    return "'" + str(value).replace("'", "'\\''") + "'"


def records(data):
    if not data:
        return []
    assert data.endswith(b"\0"), f"history --null lacks a final delimiter: {data!r}"
    return data[:-1].decode("utf-8").split("\0")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and tmuxx ConPTY")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-history-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-history-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        (config / "config.fish").write_text(
            "function fish_prompt; printf 'HISTORY> '; end\n"
            "set -g fish_autosuggestion_enabled 0\n", encoding="utf-8"
        )
        history_dir = root / "data" / "fish"
        history_dir.mkdir(parents=True)
        legacy = history_dir / "native_fish_history"
        native = history_dir / "native_fish_history.yaml"
        posix = history_dir / "fish_history"
        legacy_entries = ["echo LEGACY_one", "begin\necho LEGACY_🐟\nend"]
        legacy_bytes = ("#V2\n" + "\n".join(
            source.replace("\\", "\\\\").replace("\n", "\\n")
            for source in legacy_entries
        ) + "\n").encode("utf-8")
        legacy.write_bytes(legacy_bytes)
        posix_bytes = b"- cmd: echo POSIX_history_is_separate\n  when: 1\n"
        posix.write_bytes(posix_bytes)
        env = dict(
            os.environ, HOME=temporary, USERPROFILE=temporary,
            XDG_CONFIG_HOME=str(root / "config"), FISH_NATIVE_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
        )
        for name in ("TMUX", "TMUXX_ENDPOINT", "fish_history"):
            env.pop(name, None)
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(
                base + list(arguments), env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=10, check=check, creationflags=subprocess.CREATE_NO_WINDOW,
            )

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture(pane):
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def wait_for(pane, predicate, description):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture(pane)
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}:\n{text[-8000:]}")

        def expect_input(pane, source=""):
            expected = ("HISTORY> " + source).rstrip()
            return wait_for(pane, lambda text: text.rstrip().splitlines()[-1:] == [expected],
                            f"missing edited input {expected!r}")

        def keys(pane, *names):
            call("send-keys", "-t", pane, *names)

        def literal(pane, source):
            call("send-keys", "-t", pane, "-l", source)

        def send(pane, source):
            if "\n" in source:
                path = root / "paste.txt"
                path.write_bytes(source.encode("utf-8"))
                call("load-buffer", "-b", "history", str(path))
                call("paste-buffer", "-p", "-r", "-b", "history", "-t", pane)
            else:
                literal(pane, source)
            keys(pane, "Enter")

        serial = 0

        def execute(pane, source, *, record=False, expected_status=0):
            nonlocal serial
            serial += 1
            marker = f"HISTORY_finished_{serial}"
            status_file = root / f"status-{serial}.txt"
            source += "; printf '%s\\n' $status > " + quote(status_file.as_posix())
            source += "; printf '" + marker + "\\n'"
            source = source if record else " " + source
            send(pane, source)
            wait_for(pane, lambda text: marker in text.splitlines(), f"missing marker {marker}")
            expect_input(pane)
            status = status_file.read_text(encoding="utf-8").strip()
            assert status == str(expected_status), (
                f"command status {status}, expected {expected_status}: {source}\n{capture(pane)[-5000:]}"
            )
            return source

        def output(pane, command="history search --null"):
            path = root / f"query-{serial + 1}.bin"
            execute(pane, command + " > " + quote(path.as_posix()))
            return path.read_bytes()

        def search(pane, arguments=""):
            return records(output(pane, "history search --null " + arguments))

        def append(pane, *sources):
            execute(pane, "history append " + " ".join(map(quote, sources)))

        def delete(pane, source):
            execute(pane, "history delete --exact --case-sensitive " + quote(source))

        def cancel(pane):
            keys(pane, "C-c")
            expect_input(pane)

        def new_pane(name, language="fish", private=False):
            pane = call(
                "new-session", "-dP", "-s", name, "-x", "220", "-y", "40",
                "-c", str(root), "-F", "#{pane_id}", str(fish),
                "--interactive-language=" + language, *(["--private"] if private else []), "-i",
            ).strip()
            assert pane.startswith("%"), pane
            expect_input(pane)
            return pane

        def finish(pane):
            send(pane, " exit 0")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                state = call("display-message", "-pt", pane,
                             "#{pane_dead}|#{pane_dead_status}").strip()
                if state.startswith("1|"):
                    assert state == "1|0", (state, capture(pane))
                    return
                time.sleep(0.03)
            raise AssertionError(f"shell did not exit:\n{capture(pane)}")

        def command(source, *, private=False, command_env=None, expected_status=0):
            result = subprocess.run(
                [str(fish), "-N", *(["--private"] if private else []), "-c", source],
                env=env if command_env is None else command_env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=15, check=False, creationflags=subprocess.CREATE_NO_WINDOW,
            )
            assert result.returncode == expected_status, (source, result.returncode, result.stderr)
            return result.stdout

        def history_snapshot():
            return {path.name: (path.read_bytes(), path.stat().st_mtime_ns)
                    for path in history_dir.iterdir() if path.is_file()}

        owner = subprocess.Popen(
            base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                assert time.monotonic() < deadline, "private tmuxx owner did not become ready"
                time.sleep(0.03)

            pane = new_pane("history")
            assert search(pane) == list(reversed(legacy_entries)), "legacy #V2 import lost entries"
            execute(pane, "history save")
            assert native.is_file(), "native history was not saved separately from legacy history"
            assert legacy.read_bytes() == legacy_bytes, "legacy history was rewritten during migration"
            assert posix.read_bytes() == posix_bytes, "POSIX history was modified"

            upper = "echo History_Alpha"
            lower = "echo history_alpha"
            beta = "printf history_beta"
            multiline = "begin\necho history_魚\nend"
            append(pane, upper, lower, beta, multiline, lower)
            expected = [lower, multiline, beta, upper, *reversed(legacy_entries)]
            assert search(pane) == expected, "history order or duplicate suppression changed"
            assert search(pane, "--exact") == expected, "--exact without terms must list all entries"
            assert output(pane, "history") == ("\n".join(expected) + "\n").encode("utf-8")
            assert search(pane, "--prefix 'ECHO HISTORY_'") == [lower, upper]
            assert search(pane, "--contains 'HISTORY_'") == [lower, multiline, beta, upper]
            assert search(pane, "--exact " + quote(lower)) == [lower, upper]
            assert search(pane, "--exact --case-sensitive " + quote(lower)) == [lower]
            assert search(pane, "--contains --case-sensitive 'History'") == [upper]
            assert search(pane, "--exact 'absent-history-entry'") == [], "no match must succeed"
            assert search(pane, "--max 2") == expected[:2]
            assert search(pane, "--max=2 --reverse") == list(reversed(expected[:2]))
            assert search(pane, "--contains beta Alpha") == [beta, lower, upper]
            assert search(pane, "--contains --max 2 --reverse beta Alpha") == [lower, beta]
            assert records(output(pane, "history --search -e -C -z " + quote(lower))) == [lower]
            assert records(output(pane, "history search -p -C -n 2 -R -z echo")) == [upper, lower]
            assert search(pane, "--exact --case-sensitive " + quote(multiline)) == [multiline]
            legacy_search = " --exact --case-sensitive " + quote(legacy_entries[0])
            assert output(pane, "history search --show-time='%s:'" + legacy_search) == (
                "0:" + legacy_entries[0] + "\n"
            ).encode("utf-8"), "imported legacy history must retain its unknown epoch timestamp"
            assert output(pane, "history search -t%s:" + legacy_search) == (
                "0:" + legacy_entries[0] + "\n"
            ).encode("utf-8")
            color_search = " --exact --case-sensitive " + quote(lower)
            plain = (lower + "\n").encode("utf-8")
            assert output(pane, "history search --color=never" + color_search) == plain
            assert output(pane, "history search --color=auto" + color_search) == plain
            colored = output(pane, "history search --color=always" + color_search)
            assert b"\x1b[" in colored, "--color=always omitted highlighting in redirected output"
            assert re.sub(rb"\x1b\[[0-9;:]*m", b"", colored) == plain, colored

            # The currently executing interactive command is visible only after
            # it finishes, including when that command itself queries history.
            current_output = root / "current-command.bin"
            current = execute(pane, "history search --null > " + quote(current_output.as_posix()),
                              record=True)
            assert current not in records(current_output.read_bytes()), "current command included itself"
            assert current in search(pane), "completed interactive command was not recorded"
            delete(pane, current)
            delete(pane, lower)
            assert search(pane, "--exact --case-sensitive " + quote(lower)) == []
            assert search(pane, "--exact --case-sensitive " + quote(upper)) == [upper]
            execute(pane, "history save")
            assert records(command("history search --null")) == [item for item in expected if item != lower]

            # Updating history must update the live editor's suggestions and
            # substring Up navigation, without restarting the shell.
            execute(pane, "history clear")
            assert search(pane) == []
            older, newer = "echo HIST_hint_older", "echo HIST_hint_newer"
            append(pane, older, newer)
            execute(pane, "set -g fish_autosuggestion_enabled 1")
            literal(pane, "echo HIST_hint_")
            expect_input(pane, newer)
            cancel(pane)
            delete(pane, newer)
            literal(pane, "echo HIST_hint_")
            expect_input(pane, older)
            keys(pane, "Up")
            expect_input(pane, older)
            cancel(pane)
            delete(pane, older)
            literal(pane, "echo HIST_hint_")
            expect_input(pane, "echo HIST_hint_")
            keys(pane, "Up")
            expect_input(pane, "echo HIST_hint_")
            cancel(pane)
            execute(pane, "set -g fish_autosuggestion_enabled 0; history clear")
            append(pane, "echo CONCURRENT_baseline")
            execute(pane, "history save")
            finish(pane)
            print("PASS native history: legacy import, searches/order/flags, Unicode/multiline, "
                  "append/delete/save, current command exclusion, live editor refresh")

            # Both sessions start from the same file, then save interleaved
            # updates. A stale session must neither erase nor resurrect entries.
            first = new_pane("first")
            second = new_pane("second", "bash")
            baseline, first_item, second_item = (
                "echo CONCURRENT_baseline", "echo CONCURRENT_first", "echo CONCURRENT_second"
            )
            assert search(first) == [baseline]
            assert search(second) == [baseline]
            append(first, first_item)
            execute(first, "history save")
            append(second, second_item)
            execute(second, "history save")
            assert set(records(command("history search --null"))) == {baseline, first_item, second_item}
            execute(first, "history merge")
            assert set(search(first)) == {baseline, first_item, second_item}
            delete(first, first_item)
            execute(first, "history save")
            execute(second, "history merge; history save")
            assert set(search(second)) == {baseline, second_item}, "deleted entry was resurrected"
            # Merge establishes a new clear-session boundary. The second
            # session's earlier entry is retained; only subsequent entries
            # from that session are removed by clear-session.
            second_recent = "echo CONCURRENT_second_after_merge"
            append(second, second_recent)
            execute(second, "history save")
            later = "echo CONCURRENT_later"
            append(first, later)
            execute(first, "history save")
            execute(second, "history clear-session; history save")
            execute(first, "history merge")
            assert search(first) == [later, second_item, baseline], (
                "clear-session did not preserve the merge boundary and other sessions' entries"
            )

            # Private interactive history starts empty and remains in memory,
            # including explicit save/merge/clear operations.
            before_private = history_snapshot()
            private = new_pane("private", private=True)
            assert search(private) == [], "private session imported persistent history"
            append(private, "echo PRIVATE_memory")
            assert search(private) == ["echo PRIVATE_memory"]
            execute(private, "history save; history merge", expected_status=2)
            assert "can't merge history in private mode" in capture(private)
            assert search(private) == ["echo PRIVATE_memory"]
            execute(private, "history clear-session")
            assert search(private) == []
            append(private, "echo PRIVATE_clear")
            execute(private, "history clear")
            assert search(private) == []
            finish(private)
            assert history_snapshot() == before_private, "private history touched a persistent file"

            execute(first, "history clear")
            execute(second, "history merge")
            assert search(first) == [] and search(second) == [], "clear did not propagate to another session"
            finish(first)
            finish(second)
            print("PASS native history: concurrent Fish/Bash saves, merge/deletion/clear-session, "
                  "shared clear, private interactive isolation")

            # Noninteractive invocations share persistence. Pipeline workers
            # receive in-memory history, even in a private session with no files.
            command("history append 'echo NONINTERACTIVE'; history save")
            assert records(command("history search --null")) == ["echo NONINTERACTIVE"]
            assert command("history search --exact --case-sensitive 'echo NONINTERACTIVE' | string upper") == (
                b"ECHO NONINTERACTIVE\n"
            )
            before_private = history_snapshot()
            assert command("history search --null", private=True) == b""
            assert command("history append pipe_memory; history search --exact --case-sensitive pipe_memory "
                           "| string upper", private=True) == b"PIPE_MEMORY\n"
            command("history append private; history save; history merge", private=True, expected_status=2)
            command("history clear; history clear-session", private=True)
            assert history_snapshot() == before_private, "private -c changed persistent history"
            private_env = dict(env, XDG_DATA_HOME=str(root / "private-data"))
            command("history append private; history save; history merge", private=True,
                    command_env=private_env, expected_status=2)
            assert not (root / "private-data").exists(), "private mode created a history directory"
            assert legacy.read_bytes() == legacy_bytes
            assert posix.read_bytes() == posix_bytes
            print("PASS native history: noninteractive persistence, pipeline snapshot, private no-file behavior, "
                  "legacy/POSIX files preserved")
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
