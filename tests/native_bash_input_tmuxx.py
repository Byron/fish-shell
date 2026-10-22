#!/usr/bin/env python3
"""Check Bash-style Fish input in a private native tmuxx ConPTY session.

Usage: python tests/native_bash_input_tmuxx.py FISH_EXE TMUXX_EXE

This exercises the input dialect and shared Fish session, not a Bash evaluator.
The owner, configuration, history and files are private to this test.
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
        parser.error("this test requires native Windows and tmuxx ConPTY")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-bash-input-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-bash-input-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        configuration = (
            "set -g DIA_config 'Fish config Ω'\n"
            "function DIA_from_config; echo DIA_config_function:$DIA_config; end\n"
            "function fish_prompt; printf 'DIA> '; end\n"
        )
        (config / "config.fish").write_text(configuration, encoding="utf-8")
        (root / ".bashrc").write_text(
            "printf wrong > DIA_bash_startup_marker\n", encoding="utf-8"
        )
        work = root / "work directory Ω"
        work.mkdir()
        (root / "DIA filename Ω.txt").write_text("completion fixture\n", encoding="utf-8")
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
        for name in ("TMUX", "TMUXX_ENDPOINT", "DIA_private"):
            env.pop(name, None)
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

        def capture():
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def wait_for(predicate, description):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture()
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}:\n{text[-9000:]}")

        def expect(line, count=1):
            return wait_for(
                lambda text: text.splitlines().count(line) >= count,
                f"missing output {line!r}",
            )

        def expect_input(source):
            expected = ("DIA> " + source).rstrip()
            return wait_for(
                lambda text: text.rstrip().splitlines()[-1:] == [expected],
                f"missing edited input {expected!r}",
            )

        def keys(*names):
            call("send-keys", "-t", pane, *names)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        def paste(source):
            path = root / "paste.txt"
            path.write_text(source, encoding="utf-8", newline="")
            call("load-buffer", "-b", "dialect", str(path))
            call("paste-buffer", "-p", "-r", "-b", "dialect", "-t", pane)

        def start(name, *options):
            new_pane = call(
                "new-session", "-dP", "-s", name, "-x", "160", "-y", "50",
                "-F", "#{pane_id}", str(fish), *options, "-i",
            ).strip()
            assert new_pane.startswith("%"), new_pane
            return new_pane

        def exit_shell():
            send("exit 0")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                state = call(
                    "display-message", "-pt", pane, "#{pane_dead}|#{pane_dead_status}"
                ).strip()
                if state.startswith("1|"):
                    assert state == "1|0", (state, capture())
                    return
                time.sleep(0.03)
            raise AssertionError(f"shell did not exit:\n{capture()}")

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
                if time.monotonic() > deadline:
                    raise AssertionError("private tmuxx owner did not become ready")
                time.sleep(0.03)
            pane = start("dialect", "--interactive-language=bash")
            expect_input("")
            send("DIA_from_config")
            expect("DIA_config_function:Fish config Ω")
            assert not (root / "DIA_bash_startup_marker").exists()
            literal("if true; then DIA_from_c")
            keys("Tab")
            expect_input("if true; then DIA_from_config")
            literal("; fi")
            keys("Enter")
            expect("DIA_config_function:Fish config Ω", count=2)
            literal("printf 'DIA_file:%s\\n' 'DIA fi")
            keys("Tab")
            expect_input("printf 'DIA_file:%s\\n' DIA\\ filename\\ Ω.txt")
            keys("Enter")
            expect("DIA_file:DIA filename Ω.txt")

            send("DIA_private='value 🐟'")
            expect_input("")
            literal("printf 'DIA_variable:%s\\n' ${DIA_pr")
            keys("Tab")
            expect_input("printf 'DIA_variable:%s\\n' ${DIA_private}")
            keys("Enter")
            expect("DIA_variable:value 🐟")
            send("complete -c DIA_option -f -l flavor -r -a 'vanilla chocolate'")
            expect_input("")
            literal("if true; then DIA_option --fl")
            keys("Tab")
            expect_input("if true; then DIA_option --flavor")
            literal("van")
            keys("Tab")
            expect_input("if true; then DIA_option --flavor vanilla")
            keys("C-c")
            expect_input("")
            send("DIA_function() { printf 'DIA_function:%s:%s\\n' \"$DIA_private\" \"$1\"; }")
            expect_input("")
            send("DIA_function 'argument Ω'")
            expect("DIA_function:value 🐟:argument Ω")
            send("set DIA_list one 'two words'; printf 'DIA_list:<%s>\\n' $DIA_list")
            expect("DIA_list:<one>")
            expect("DIA_list:<two words>")
            send("if false; then echo DIA_wrong; elif true; then echo DIA_if; else echo DIA_wrong; fi")
            expect("DIA_if")
            send("DIA_run=yes; while test \"$DIA_run\" = yes; do DIA_run=no; echo DIA_while; done")
            expect("DIA_while")
            send("DIA_sub=$(printf 'a b'); printf 'DIA_sub:<%s>\\n' \"$DIA_sub\"")
            expect("DIA_sub:<a b>")
            send("DIA_effect() { echo wrong > DIA_backtick_marker; }")
            expect_input("")
            send("printf '%s\\n' \x60DIA_effect\x60")
            expect("\x60DIA_effect\x60")
            assert not (root / "DIA_backtick_marker").exists()

            # Unexported values remain in the Fish session.
            send(f"'{fish.as_posix()}' -N -c 'set -q DIA_private; echo DIA_child:$status'")
            expect("DIA_child:1")
            send(f"cd '{work.as_posix()}'")
            expect_input("")
            send("fish_input_mode fish; DIA_tail='dialect tail'")
            expect_input("")
            send("echo DIA_shared:$DIA_private:$DIA_tail")
            expect("DIA_shared:value 🐟:dialect tail")
            send("function DIA_fish_function; echo DIA_fish:$DIA_private; end")
            expect_input("")
            send("DIA_function 'Fish caller'")
            expect("DIA_function:value 🐟:Fish caller")
            send("fish_input_mode bash; set -g DIA_fish_tail 'Fish tail'")
            expect_input("")
            send("DIA_fish_function; printf 'DIA_tail:%s\\n' \"$DIA_fish_tail\"")
            expect("DIA_fish:value 🐟")
            expect("DIA_tail:Fish tail")
            send("pwd")
            wait_for(
                lambda text: work.as_posix().lower() in text.replace("\\", "/").lower().splitlines(),
                "mode switch lost the working directory",
            )

            # Edit the first line after entering an unfinished loop's body.
            send("for DIA_item in one two; do")
            literal("printf 'DIA_multi:%s\\n' \"$DIA_item\"")
            keys("Enter")
            literal("done")
            text = wait_for(
                lambda text: bool(text.rstrip().splitlines())
                and text.rstrip().splitlines()[-1].strip() == "done",
                "multiline body missing",
            )
            assert "...>" not in text[text.rfind("DIA>"):], text
            keys("Up", "Up", "Home", "C-k")
            literal("for DIA_item in edited two; do")
            keys("Down", "Down", "End", "Enter")
            expect("DIA_multi:edited")
            expect("DIA_multi:two")
            assert "DIA_multi:one" not in capture().splitlines()

            paste("DIA_pasted='a b'\nprintf 'DIA_paste:%s\\n' \"$DIA_pasted\"")
            wait_for(lambda text: bool(text.rstrip().splitlines())
                     and "$DIA_pasted" in text.rstrip().splitlines()[-1],
                     "pasted Bash source missing")
            assert "DIA_paste:a b" not in capture().splitlines()
            keys("Enter")
            expect("DIA_paste:a b")

            # Reject the entire submission before its first redirect executes.
            for rejected in (
                "printf wrong > DIA_rejected_marker; [[ yes == yes ]]",
                "printf wrong > DIA_rejected_marker; values=(one two)",
                "printf wrong > DIA_rejected_marker; if ; then true; fi",
            ):
                send(rejected)
                expect_input("")
                assert not (work / "DIA_rejected_marker").exists(), rejected
            send("DIA_after_rejection=usable; echo DIA_recovered:$DIA_after_rejection")
            expect("DIA_recovered:usable")
            assert not (work / "DIA_rejected_marker").exists()
            send("DIA_ignored=$(fish_input_mode fish)")
            expect_input("")
            send("DIA_after_substitution=bash; echo DIA_mode:$DIA_after_substitution")
            expect("DIA_mode:bash")

            history_source = "DIA_history='original source 🐟'"
            send(history_source)
            expect_input("")
            literal("DIA_history=")
            expect_input(history_source)
            keys("Right", "Enter")
            send("printf 'DIA_suggestion:%s\\n' \"$DIA_history\"")
            expect("DIA_suggestion:original source 🐟")
            send("fish_input_mode fish")
            expect_input("")
            literal("DIA_history=")
            keys("Up")
            expect_input(history_source)
            keys("C-c")
            expect_input("")
            exit_shell()
            history = root / "data" / "fish" / "native_fish_history"
            contents = history.read_text(encoding="utf-8")
            assert history_source in contents, contents
            assert "builtin set -- DIA_history" not in contents
            assert not (root / "data" / "fish" / "native_rash_history").exists()

            # Configuration runs in Fish after the initial CLI mode selection.
            (config / "config.fish").write_text(
                configuration + "fish_input_mode bash\n", encoding="utf-8"
            )
            pane = start("configured", "--interactive-language", "fish")
            expect_input("")
            send("DIA_configured=bash; echo DIA_startup:$DIA_configured:$DIA_config")
            expect("DIA_startup:bash:Fish config Ω")
            assert not (root / "DIA_bash_startup_marker").exists()
            exit_shell()
            print(
                "PASS native Bash-style Fish input: Fish config/shared private state, "
                "assignments/blocks/functions/substitutions, literal backticks, "
                "editable multiline/paste, switching, original history, rejection before effects"
            )
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
