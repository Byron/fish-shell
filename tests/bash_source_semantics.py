#!/usr/bin/env python3
"""PTY sourcing checks for the retained Bash session. Requires pexpect and an explicit external Bash oracle."""
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time

import pexpect

from bash_input_integration import Shell


binary = Path(sys.argv[1]).resolve()
backend = Path(sys.argv[2]).resolve()
quote = lambda value: shlex.quote(str(value))


def compare(shell, command, status=0):
    reference = subprocess.run(
        [backend, "--noprofile", "--norc", "-i", "-c", command],
        env=shell.env, capture_output=True,
    )
    assert reference.returncode == status, reference
    shell.child.sendline(command)
    if reference.stdout:
        shell.child.expect_exact(reference.stdout.replace(b"\n", b"\r\n"))
    shell.finished(status)


with tempfile.TemporaryDirectory(prefix="fish-bash-source-") as tmp:
    root = Path(tmp).resolve()
    home = root / "home"
    home.mkdir()
    work = root / "directory ü with spaces"
    work.mkdir()
    observed = home / "observed"
    state = root / "source ü 'quoted';$.bash"
    state.write_text(
        "scalar='from source'\n"
        "declare -a sourced_array=(zero 'one two')\n"
        "declare -A sourced_map=([key]='map value')\n"
        "sourced_function() { printf 'FUNCTION:%s\\n' \"$scalar\"; }\n"
        "alias sourced_alias=sourced_function\n"
        "export SOURCE_SHARED='from bash'\n"
        "shopt -s extglob\n"
        "set -o pipefail\n"
        f"cd -- {quote(work)}\n"
        "printf 'SOURCE:%s:%s:%s\\n' \"$#\" \"$1\" \"$2\"\n"
        "return 7\n"
        "printf 'unreachable\\n'\n"
    )
    shell = Shell(binary, home, config=(
        "function observe_source --on-event fish_postexec; "
        f"printf '%s\\n' $status $PWD $SOURCE_SHARED > {observed}; end; "
    ))
    try:
        shell.run("set -- outer 'old two'")
        shell.run(f"source {quote(state)} inner 'new two'", "SOURCE:2:inner:new two\r\n", 7)
        actual = observed.read_text().splitlines()
        assert actual == ["7", str(work.resolve()), "from bash"], actual
        shell.run('printf "AFTER:%s:%s:%s\\n" "$#" "$1" "$2"', "AFTER:2:outer:old two\r\n")
        shell.run('printf "STATE:%s|%s|%s|%s\\n" "$scalar" "${sourced_array[1]}" "${sourced_map[key]}" "$SOURCE_SHARED"',
                  "STATE:from source|one two|map value|from bash\r\n")
        shell.run("sourced_alias", "FUNCTION:from source\r\n")
        shell.run("shopt -q extglob; a=$?; false | true; b=$?; printf 'OPTIONS:%s:%s\\n' \"$a\" \"$b\"",
                  "OPTIONS:0:1\r\n")
        shell.run("case item in @(item|other)) printf 'OPTION:extglob\\n';; esac", "OPTION:extglob\r\n")
        print("PASS source retains functions, aliases, arrays, options, cwd and exports; Fish hooks see state", flush=True)

        inner = root / "inner.bash"
        inner.write_text("nested_value=retained\nprintf 'INNER:%s:%s\\n' \"$#\" \"$1\"\nreturn 6\n")
        outer = root / "outer.bash"
        outer.write_text(
            f". {quote(inner)} nested\n"
            "nested_status=$?\n"
            "printf 'OUTER:%s:%s:%s\\n' \"$nested_status\" \"$#\" \"$1\"\n"
            "return 9\n"
        )
        compare(shell, f"set -- original 'two words'; . {quote(outer)} supplied; code=$?; "
                "printf 'RESTORED:%s:%s:%s:%s:%s\\n' \"$code\" \"$#\" \"$1\" \"$2\" \"$nested_value\"")
        print("PASS native differential: nested source/dot, return statuses and temporary positional arguments", flush=True)

        redirected = root / "redirected.bash"
        redirected.write_text("printf 'source stdout\\n'; printf 'source stderr\\n' >&2\nredirected_value=retained\n")
        stdout, stderr = root / "stdout", root / "stderr"
        shell.run(f"source {quote(redirected)} > {quote(stdout)} 2> {quote(stderr)}")
        assert stdout.read_text() == "source stdout\n"
        assert stderr.read_text() == "source stderr\n"
        shell.run('printf "REDIRECT:%s\\n" "$redirected_value"', "REDIRECT:retained\r\n")

        trapped = root / "trapped.bash"
        trapped.write_text("trap 'SOURCE_TRAP=handled' USR1\nreturn 5\n")
        compare(shell, f"source {quote(trapped)}; code=$?; kill -USR1 \"$$\"; "
                "printf 'TRAP:%s:%s\\n' \"$code\" \"$SOURCE_TRAP\"; trap - USR1")
        returned = root / "returned.bash"
        returned.write_text("return 6\n")
        compare(shell, "trap 'printf \"RETURN:%s\\n\" \"$?\"' RETURN; "
                f"source {quote(returned)}; code=$?; trap - RETURN; printf 'RETURN-STATUS:%s\\n' \"$code\"")
        print("PASS native source redirections, persistent traps and RETURN trap/status behavior", flush=True)

        debugged = root / "debugged.bash"
        debug_log, sentinel = root / "debug-log", root / "unfinished-substitution"
        debugged.write_text(f"DEBUG_LOG={quote(debug_log)}\n"
                           "trap 'printf \"debug\\n\" >> \"$DEBUG_LOG\"' DEBUG\n")
        shell.run(f"source {quote(debugged)}")
        previous = debug_log.read_bytes() if debug_log.exists() else b""
        shell.child.sendline(f'echo "$(touch {quote(sentinel)})')
        time.sleep(.1)
        shell.clear()
        assert not sentinel.exists()
        assert (debug_log.read_bytes() if debug_log.exists() else b"") == previous
        shell.run("trap - DEBUG")
        assert debug_log.read_bytes() == previous + b"debug\n"
        print("PASS unfinished input checking does not execute substitutions or sourced DEBUG traps", flush=True)

        shell.run(f"source {quote(root / 'missing.bash')}", status=1)
        shell.run('printf "MISSING:%s\\n" "$?"', "MISSING:1\r\n")
        malformed = root / "malformed.bash"
        malformed.write_text("before_error=preserved\nif true; then\n")
        shell.run(f"source {quote(malformed)}", status=2)
        shell.run('printf "ERROR:%s:%s\\n" "$?" "$before_error"', "ERROR:2:preserved\r\n")
        assert os.tcgetpgrp(shell.child.child_fd) == shell.child.pid
        print("PASS source errors retain prior state/status and return the terminal to Fish", flush=True)

        background = root / "background.bash"
        background.write_text("sleep 30 &\n")
        shell.run(f"source {quote(background)}")
        shell.run("fish_input_mode fish", status=127)
        shell.run("jobs", "running\tsleep 30")
        shell.signal_job("fg", "c", 130)
        shell.run('printf "SOURCE-EXPORT:%s\\n" "$SOURCE_SHARED"', "SOURCE-EXPORT:from bash\r\n")
        shell.child.sendline("exit")
        shell.child.expect(pexpect.EOF)
        print("PASS Rash retains a sourced job through fg/Ctrl-C and preserves exported state", flush=True)
    finally:
        if sys.exc_info()[0]:
            Path("/private/tmp/fish-bash-source.log").write_bytes(shell.log.getvalue())
        shell.close()

    exiting = root / "exiting.bash"
    exiting.write_text("trap 'printf \"SOURCE-EXIT:%s\\n\" \"$?\"' EXIT\nexit 23\n")
    shell = Shell(binary, home)
    try:
        shell.child.sendline(f"source {quote(exiting)}")
        shell.child.expect_exact(b"SOURCE-EXIT:23\r\n")
        shell.child.expect(pexpect.EOF)
        shell.child.close()
        assert shell.child.exitstatus == 23, (shell.child.exitstatus, shell.child.signalstatus)
        print("PASS sourced exit executes the native EXIT trap and terminates the Fish session with its status", flush=True)
    finally:
        shell.close()
