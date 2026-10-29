#!/usr/bin/env python3
"""PTY regressions for Bash-style input evaluated with Fish semantics.

Usage: python3 tests/bash_style_input.py target/debug/fish
Requires pexpect and Git. The bundled completion definitions remain unchanged.
"""
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

import pexpect

from bash_input_integration import Shell, assert_file_completions


def main(binary):
    with tempfile.TemporaryDirectory(prefix="fish-bash-style-") as tmp:
        root = Path(tmp).resolve()
        home = root / "home"
        legacy_rash = home / "data/fish/bash-history/fish_history"
        legacy_rash.parent.mkdir(parents=True)
        legacy_rash_text = "- cmd: echo legacy_rash_${missing:-value}\n  when: 1\n"
        legacy_rash.write_text(legacy_rash_text)
        legacy_fish = home / "data/fish/fish_history"
        legacy_fish.write_text("- cmd: echo legacy_fish_history\n  when: 1\n")
        legacy_dialect = home / "data/fish/bash-style-history/fish_history"
        legacy_dialect.parent.mkdir(parents=True)
        legacy_dialect_text = "- cmd: echo legacy_dialect_history\n  when: 1\n"
        legacy_dialect.write_text(legacy_dialect_text)
        work = root / "work"
        child = work / "child"
        child.mkdir(parents=True)
        repo = root / "repo ü space"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        subprocess.run(["git", "-C", str(repo), "-c", "commit.gpgsign=false", "-c", "user.name=Test",
                        "-c", "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "fixture"], check=True)
        subprocess.run(["git", "-C", str(repo), "branch", "topic-completion"], check=True)
        observed, context, context_cursor = home / "observed", home / "context", home / "context-cursor"
        config = (
            "set -g private_fish original; set -g words 'one two' three; "
            "function ..; cd ..; end; "
            "function fish_only; printf 'FISH:%s:%s\\n' $private_fish $argv; end; "
            "alias hi fish_only; alias g git; abbr --add fh fish_only; "
            "function observe --on-event fish_postexec; "
            f"printf '%s\\n' $status $PWD $private_fish > {observed}; end; "
            "function inspect_context; "
            f"commandline --current-process > {context}; commandline --cursor > {context_cursor}; "
            "printf 'context-value\\tDynamic description\\n'; end; "
            "complete -c inspect_probe -f -a '(inspect_context)'; "
            "complete -c option_probe -f -l target -r -a value -d 'Option description'; "
            "complete -c ordered_probe -f -k -a 'zeta alpha' -d 'Ordered description'; "
            "complete -c mutate_probe -f -a '(commandline -r overwritten; echo candidate)'; "
            "bind ctrl-v backward-token; "
        )
        shell = Shell(binary, home, dialect=True, config=config)
        try:
            shell.run("hi hello", "FISH:original:hello\r\n")
            shell.child.send("fh ")
            assert shell.buffer()[0] == "fish_only "
            shell.clear()
            shell.run("fh abbreviated", "FISH:original:abbreviated\r\n")
            shell.run("private_fish=changed; fish_only now", "FISH:changed:now\r\n")
            assert observed.read_text().splitlines()[-1] == "changed"
            shell.run("printf '<%s>\\n' $words", "<one two>\r\n<three>\r\n")
            shell.run("scalar='a b'; printf 'SCALAR:<%s>\\n' $scalar", "SCALAR:<a b>\r\n")
            shell.run("printf 'SLICE:<%s>\\n' $words[1]", "SLICE:<one two>\r\n")
            shell.run(f"cd {shlex.quote(str(child))}; ..; printf 'PWD:%s\\n' \"$PWD\"",
                      f"PWD:{work}\r\n")
            assert observed.read_text().splitlines()[1] == str(work)
            shell.run("false", status=1)
            shell.run("printf 'STATUS:%s\\n' $?", "STATUS:1\r\n")
            print("PASS Fish aliases/functions/abbreviations, immediate cwd/state, scalar/list expansion and status", flush=True)

            tick_sentinel = home / "backticks-must-not-run"
            literal_command = f"`touch {tick_sentinel}`"
            shell.run("printf '%s\\n' \"" + literal_command + "\"", literal_command + "\r\n")
            assert not tick_sentinel.exists()
            shell.run('printf "%s\\n" "unmatched `"', 'unmatched `\r\n')
            shell.run('printf "%s\\n" "\\`" bare`', '\\`\r\nbare`\r\n')
            shell.run('printf "%s\\n" "$(printf \'nested`\')"', 'nested`\r\n')
            shell.run('false; printf "STATUS:%s:PID:%s\\n" $? $$',
                      f'STATUS:1:PID:{shell.child.pid}\r\n')
            shell.child.send('echo `raw next')
            shell.child.sendcontrol('v')
            assert shell.buffer() == ('echo `raw next', len('echo `raw '))
            shell.child.sendcontrol('v')
            assert shell.buffer() == ('echo `raw next', len('echo '))
            shell.clear()
            print("PASS literal/unmatched backticks, nested substitutions, status/PID and token movement", flush=True)

            shell.child.sendline("for item in a b; do")
            time.sleep(.05)
            shell.child.sendline("printf 'LOOP:%s\\n' $item")
            shell.child.sendline("done")
            shell.child.expect_exact(b"LOOP:a\r\nLOOP:b\r\n")
            shell.finished(0)
            shell.run("if false; then echo wrong; elif true; then echo CONDITIONAL; else echo wrong; fi", "CONDITIONAL\r\n")
            shell.run("greet() { fish_only $argv; }; greet body", "FISH:changed:body\r\n")
            fish_source = root / "state.fish"
            fish_source.write_text("set -g private_fish sourced\nfunction sourced_helper; echo SOURCE:$private_fish; end\n")
            shell.run(f"source {shlex.quote(str(fish_source))}; sourced_helper", "SOURCE:sourced\r\n")
            sentinel = root / "must-not-run"
            for unsupported in ("echo ${missing:=fallback}", "((counter+=1))", "(echo subshell)",
                                "echo $(printf before; echo ${missing:=fallback})"):
                source = f"touch {sentinel}; {unsupported}"
                shell.child.sendline(source)
                text, _ = shell.buffer()
                assert text.rstrip("\n") == source, text
                assert not sentinel.exists(), unsupported
                shell.clear()
            shell.run("echo AFTER-REJECTION", "AFTER-REJECTION\r\n")
            print("PASS multiline/control/function translation, Fish source and atomic unsupported-syntax rejection", flush=True)

            shell.run(f"repo={shlex.quote(str(repo))}")
            for source, expected in (
                ('git -C "$repo" switch topic-', 'git -C "$repo" switch topic-completion '),
                ('g -C "$repo" switch topic-', 'g -C "$repo" switch topic-completion '),
                ("git --ver", "git --version "),
                ("fish_on", "fish_only "),
                ("option_probe --target=va", "option_probe --target=value "),
            ):
                shell.child.send(source + "\t")
                text, cursor = shell.buffer()
                assert text == expected, (source, text)
                assert cursor == len(text), (text, cursor)
                shell.clear()
            shell.child.send("git --ver suffix")
            shell.child.send("\x1b[D" * len(" suffix"))
            shell.child.send("\t")
            assert shell.buffer() == ("git --version suffix", len("git --version "))
            shell.clear()
            shell.child.send("inspect_probe \t")
            assert shell.buffer()[0] == "inspect_probe context-value "
            assert context.read_text() == "inspect_probe \n", context.read_text()
            assert int(context_cursor.read_text()) == len("inspect_probe ")
            shell.clear()
            start = len(shell.log.getvalue())
            shell.child.send("ordered_probe \t")
            shell.buffer()
            menu = shell.log.getvalue()[start:]
            assert b"Ordered description" in menu and b"zeta" in menu and b"alpha" in menu, menu
            assert menu.index(b"zeta") < menu.index(b"alpha"), menu
            shell.clear()
            for source in ("mutate_probe ", f"echo $(touch {sentinel}) "):
                shell.child.send(source + "\t")
                assert shell.buffer()[0] == source
                assert not sentinel.exists()
                shell.clear()
            shell.run(f"cd {shlex.quote(str(root))}")
            for filename, prefix in (("topic-escaped", "\\t"), ("ünicode-completion", "\\ü")):
                (root / filename).write_text("ESCAPED-FILE\n")
                shell.child.send("cat " + prefix + "\t")
                completed, _ = shell.buffer()
                assert completed.startswith("cat " + prefix) and completed.endswith(" "), completed
                shell.child.sendline("")
                shell.child.expect_exact(b"ESCAPED-FILE\r\n")
                shell.finished(0)
            filename = "fixture-ü ;$()` file"
            (root / filename).write_text("LITERAL-FILE\n")
            shell.child.send("cat fixture-\t")
            completed, _ = shell.buffer()
            assert completed.startswith("cat fixture-") and completed.endswith(" "), completed
            shell.child.sendline("")
            shell.child.expect_exact(b"LITERAL-FILE\r\n")
            shell.finished(0)
            (root / "directory-completion").mkdir()
            shell.child.send("cd directory-\t")
            assert shell.buffer()[0] == "cd directory-completion/"
            shell.clear()
            (home / "tilde-file ü ;$()`").write_text("TILDE-FILE\n")
            shell.child.send("cat ~/tilde-f\t")
            completed, _ = shell.buffer()
            assert completed.startswith("cat ~/tilde-file"), completed
            shell.child.sendline("")
            shell.child.expect_exact(b"TILDE-FILE\r\n")
            shell.finished(0)
            (home / "tilde-directory").mkdir()
            shell.child.send("cd ~/tilde-d\t")
            assert shell.buffer()[0] == "cd ~/tilde-directory/"
            shell.clear()
            shell.child.send("cd ~/tilde-d")
            time.sleep(.2)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == "cd ~/tilde-directory/"
            shell.clear()
            assert_file_completions(shell, root)
            (root / "glob-source.txt").write_text("GLOB-FILE\n")
            for source, expected in (
                ("cp *.txt directory-", "cp *.txt directory-completion/"),
                ("cp **/*.txt directory-", "cp **/*.txt directory-completion/"),
                ("cp glob-*.txt directory-", "cp glob-*.txt directory-completion/"),
                ("cat glob-*.txt > directory-", "cat glob-*.txt > directory-completion/"),
                ("cat glob-*", "cat glob-* "),
            ):
                shell.child.send(source + "\t")
                assert shell.buffer()[0] == expected, (source, shell.buffer())
                shell.clear()
            (root / "bracket[one].txt").write_text("BRACKET-FILE\n")
            shell.child.send("cat bracket[\t")
            assert shell.buffer()[0] != "cat bracket["
            shell.child.sendline("")
            shell.child.expect_exact(b"BRACKET-FILE\r\n")
            shell.finished(0)
            print("PASS unchanged Git/function providers, live variables, options, cursor/trailing context, descriptions and literal filenames", flush=True)

            shell.child.send("echo legacy_rash_")
            time.sleep(.15)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == "echo legacy_rash_"
            shell.clear()
            shell.child.send("echo legacy_fish_")
            time.sleep(.2)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == "echo legacy_fish_history"
            shell.clear()
            history_line = "echo unique_bash_style_history"
            shell.run(history_line, "unique_bash_style_history\r\n")
            shell.child.send("echo unique_bash_style_")
            time.sleep(.2)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == history_line
            shell.clear()
            shell.child.send("\x1b[A")
            assert shell.buffer()[0] == history_line
            shell.clear()
            shell.run("fish_input_mode fish")
            shell.child.send("echo unique_bash_style_")
            time.sleep(.2)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == history_line
            shell.clear()
            shell.run("echo DEFAULT:$private_fish", "DEFAULT:sourced\r\n")
            shell.run("echo unique_fish_history", "unique_fish_history\r\n")
            shell.run("fish_input_mode bash")
            shell.child.send("echo unique_fish_")
            time.sleep(.2)
            shell.child.send("\x1b[C")
            assert shell.buffer()[0] == "echo unique_fish_history"
            shell.clear()
            shell.child.sendline("exit")
            shell.child.expect(pexpect.EOF)
            output = shell.log.getvalue()
            for marker in (b"\x1b]133;A", b"\x1b]133;B", b"\x1b]133;C", b"\x1b]133;D", b"\x1b]7;file://"):
                assert marker in output, marker
            assert re.search(rb"\x1b\]7;file://[^/\x07]*" + re.escape(str(root).encode()) + rb"/?\x07", output)
        finally:
            if sys.exc_info()[0]:
                Path("/private/tmp/fish-bash-style.log").write_bytes(shell.log.getvalue())
            shell.close()
        fish_history = home / "data/fish/fish_history"
        assert "unique_bash_style_history" in fish_history.read_text()
        assert "unique_fish_history" in fish_history.read_text()
        assert "echo DEFAULT:" in fish_history.read_text()
        assert legacy_rash.read_text() == legacy_rash_text
        assert legacy_dialect.read_text() == legacy_dialect_text
        assert "legacy_fish_history" in fish_history.read_text()
        print("PASS shared Fish history recall/suggestions, preserved legacy files, OSC 133 and OSC 7", flush=True)

        one, two = Shell(binary, home, dialect=True), Shell(binary, home, bash=False)
        try:
            one.run("echo concurrent_style_one", "concurrent_style_one\r\n")
            two.run("echo concurrent_fish_two", "concurrent_fish_two\r\n")
            # Fish's history timestamps have one-second resolution.
            time.sleep(1.1)
            for session, prefix, expected in (
                (one, "echo concurrent_fish_", "echo concurrent_fish_two"),
                (two, "echo concurrent_style_", "echo concurrent_style_one"),
            ):
                session.run("history merge")
                session.child.send(prefix)
                time.sleep(.2)
                session.child.send("\x1b[C")
                assert session.buffer()[0] == expected
                session.clear()
            for session in (one, two):
                session.child.sendline("exit")
                session.child.expect(pexpect.EOF)
        finally:
            one.close()
            two.close()
        assert "concurrent_style_one" in fish_history.read_text()
        assert "concurrent_fish_two" in fish_history.read_text()
        private = Shell(binary, home, dialect=True, private=True)
        try:
            private.child.send("echo concurrent_style_")
            time.sleep(.15)
            private.child.send("\x1b[C")
            assert private.buffer()[0] == "echo concurrent_style_"
            private.clear()
            private.run("echo private_style", "private_style\r\n")
            private.run("fish_input_mode fish")
            private.child.send("echo private_")
            time.sleep(.2)
            private.child.send("\x1b[C")
            assert private.buffer()[0] == "echo private_style"
            private.clear()
            private.child.send("echo concurrent_fish_")
            time.sleep(.2)
            private.child.send("\x1b[C")
            assert private.buffer()[0] == "echo concurrent_fish_"
            private.clear()
            private.run("echo unique_private_fish_entry", "unique_private_fish_entry\r\n")
            private.child.sendline("exit")
            private.child.expect(pexpect.EOF)
        finally:
            private.close()
        assert "private_style" not in fish_history.read_text()
        assert "unique_private_fish_entry" not in fish_history.read_text()
        assert legacy_dialect.read_text() == legacy_dialect_text
        print("PASS concurrent Fish/dialect history merges and shared private history leaves no entries", flush=True)


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve())
