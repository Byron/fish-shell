#!/usr/bin/env python3
"""PTY checks for shared Fish/dialect state and an isolated nested Rash shell.

The former in-process Fish/Rash mode-switch assertions were replaced: Rash now
has a fixed evaluation language, and exiting its child returns to the parent.
"""
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


def close(shell):
    if sys.exc_info()[0]:
        Path("/private/tmp/fish-bash-switch.log").write_bytes(shell.log.getvalue())
    shell.close()


with tempfile.TemporaryDirectory(prefix="fish-bash-switch-") as tmp:
    root = Path(tmp).resolve()
    home, work = root / "home", root / "work"
    work.mkdir()
    result, sentinel, job = home / "mode-result", home / "must-not-execute", home / "job"
    config = (
        f"bind ctrl-b 'fish_input_mode bash; printf \"%s\\n\" $status > {result}'; "
        "bind ctrl-t undo; "
        "function complete_mode_probe; fish_input_mode bash; "
        f"printf '%s\\n' $status > {result}; echo candidate; end; "
        "complete -c mode_probe -f -a '(complete_mode_probe)'; "
        "function enter_bash; fish_input_mode bash; set -g function_tail retained; end; "
        "function refuse_event_mode --on-event attempt_mode; fish_input_mode bash; "
        f"printf '%s\\n' $status > {result}; end; "
    )
    shell = Shell(binary, home, bash=False, config=config)
    try:
        shell.run("set -g fish_value ordinary; echo DEFAULT:$fish_value", "DEFAULT:ordinary\r\n")
        source = f"touch {sentinel}"
        shell.child.send(source)
        shell.child.sendcontrol("b")
        assert shell.buffer()[0] == source
        assert result.read_text().strip() != "0"
        assert not sentinel.exists()
        shell.clear()
        for source in ("mode_probe ",):
            result.unlink()
            shell.child.send(source + "\t")
            shell.buffer()
            assert result.read_text().strip() != "0"
            shell.clear()
        shell.run("emit attempt_mode")
        assert result.read_text().strip() != "0"
        result.unlink()
        shell.run(f"set -l ignored (fish_input_mode bash; printf '%s\\n' $status > {result})")
        assert result.read_text().strip() != "0"
        result.unlink()
        shell.run("function fish_prompt; if set -q attempt_prompt_mode; set -e attempt_prompt_mode; "
                  f"fish_input_mode bash; printf '%s\\n' $status > {result}; end; printf 'PROBE> '; end; "
                  "set -g attempt_prompt_mode yes")
        assert result.read_text().strip() != "0"
        result.unlink()
        shell.child.sendline("read --prompt-str 'NESTED> ' nested; printf 'NESTED-DONE:%s\\n' \"$nested\"")
        shell.marker(b"\x1b]133;C;")
        shell.marker(b"\x1b]133;B")
        shell.child.sendcontrol("b")
        assert shell.buffer()[0] == ""
        assert result.read_text().strip() != "0"
        shell.child.sendline("nested text")
        shell.child.expect_exact(b"NESTED-DONE:nested text\r\n")
        shell.finished(0)
        print("PASS keybinding/completion/hook/substitution/nested-reader switches refuse without executing input", flush=True)

        shell.signal_job("sleep 30", "z", 0)
        shell.run(f"jobs --pid > {job}")
        pid = int(job.read_text())
        shell.run("enter_bash; set -g submission_tail retained")
        shell.child.sendcontrol("t")
        assert shell.buffer()[0] == "", "mode changes must discard old undo buffers"
        shell.run("fish_value=changed; printf 'SHARED:%s:%s:%s\\n' $fish_value $function_tail $submission_tail",
                  "SHARED:changed:retained:retained\r\n")
        shell.run(f"cd {work}; jobs --pid", f"{pid}\r\n")
        shell.run("fish_input_mode bash; printf 'IDEMPOTENT:%s\\n' $fish_value", "IDEMPOTENT:changed\r\n")
        shell.run("fish_input_mode fish; dialect_tail=retained")
        shell.child.sendcontrol("t")
        assert shell.buffer()[0] == ""
        shell.run("printf 'RETURN:%s:%s:%s\\n' $fish_value $dialect_tail $PWD", f"RETURN:changed:retained:{work}\r\n")
        shell.signal_job("fg", "c", 0)
        assert os.tcgetpgrp(shell.child.child_fd) == shell.child.pid
        shell.child.sendline("exit")
        shell.child.expect(pexpect.EOF)
        print("PASS Fish/dialect switches share variables, cwd/functions/jobs and retain terminal ownership", flush=True)
    finally:
        close(shell)

    home = root / "nested-home"
    (home / "config/fish").mkdir(parents=True)
    (home / "config/fish/config.fish").write_text(
        "if status is-interactive\n    fish_input_mode bash\nend\n"
    )
    (home / ".bashrc").write_text("export RC_MARK=loaded\nprintf 'loaded\\n' >> \"$HOME/rc-count\"\n")
    sourced = home / "state.bash"
    sourced.write_text("child_value=sourced\nchild_function() { echo CHILD-FUNCTION:$child_value; }\n")
    child_pidfile, child_jobs, parent_job = home / "child-pid", home / "child-jobs", home / "parent-job"
    shell = Shell(binary, home, bash=False)
    try:
        shell.run("parent_private=retained; export shared=parent; export bash_jobs=user-owned")
        shell.run(f"cd {work}; sleep 30 & echo $! > {parent_job}")
        parent_pid = int(parent_job.read_text())
        assert not (home / "rc-count").exists(), "dialect configuration must not load Bash startup"
        child_init = ("set -g fish_greeting; function fish_prompt; printf 'RASH> '; end; "
                      "set -gx RASH_LAUNCHED_PATH (status fish-path)")
        shell.child.sendline("rash --features=mark-prompt -C " + shlex.quote(child_init))
        shell.marker(b"\x1b]133;C;")
        shell.marker(b"RASH> ")
        shell.marker(b"\x1b]133;B")
        shell.run(f"printf '%s\\n' \"$$\" > {child_pidfile}")
        child_pid = int(child_pidfile.read_text())
        assert child_pid != shell.child.pid
        assert os.tcgetpgrp(shell.child.child_fd) == child_pid
        shell.run('printf "EXECUTABLE:%s\\n" "$RASH_LAUNCHED_PATH"', f"EXECUTABLE:{binary}\r\n")
        shell.run('printf "CHILD:%s:%s:%s:%s:%s\\n" "${parent_private-unset}" "$shared" "$PWD" "$RC_MARK" "$bash_jobs"',
                  f"CHILD:unset:parent:{work}:loaded:user-owned\r\n")
        shell.run(f"source {sourced}; child_function", "CHILD-FUNCTION:sourced\r\n")
        shell.run("a=('one two' three); printf 'ARRAY:%s\\n' \"${a[0]}\"", "ARRAY:one two\r\n")
        shell.run("fish_input_mode fish", status=127)
        shell.run("fish_input_mode bash", status=127)
        shell.run("type fish_input_mode", status=1)
        shell.run(f"jobs -p > {child_jobs}", status=1)
        assert not child_jobs.read_text(), "Rash must not inherit the parent's job table"
        shell.run("shared=child; export shared; cd /")
        shell.child.sendline("sleep 30")
        shell.marker(b"\x1b]133;C;")
        time.sleep(.15)
        shell.child.sendcontrol("c")
        shell.finished(130)
        assert os.tcgetpgrp(shell.child.child_fd) == child_pid
        shell.child.sendline("exit 23")
        shell.finished(23)
        assert os.tcgetpgrp(shell.child.child_fd) == shell.child.pid
        shell.run("printf 'PARENT:%s:%s:%s:%s\\n' $parent_private $shared $PWD $bash_jobs", f"PARENT:retained:parent:{work}:user-owned\r\n")
        shell.run(f"jobs --pid", f"{parent_pid}\r\n")
        shell.run(f"kill {parent_pid}; wait {parent_pid}")
        shell.child.sendline("exit 0")
        shell.child.expect(pexpect.EOF)
        assert (home / "rc-count").read_text() == "loaded\n"
        print("PASS same-binary Rash child isolates state/jobs, sources Bash, loads startup by default and returns status/terminal", flush=True)
    finally:
        close(shell)

    for command in ("fish_input_mode bash", "fish_input_mode fish", "fish_input_mode --bash-startup bash"):
        result = subprocess.run([binary, "-N", "-c", command], capture_output=True)
        assert result.returncode != 0 and b"fish_input_mode" in result.stderr, result
    print("PASS input-mode command remains interactive-only and no longer accepts Bash startup", flush=True)
