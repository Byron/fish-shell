#!/usr/bin/env python3
"""PTY checks for the shared Fish/Rash signal broker. Requires pexpect."""
import os
from pathlib import Path
import signal
import sys
import tempfile

import pexpect

from bash_input_integration import Shell


with tempfile.TemporaryDirectory(prefix="fish-rash-signals-") as tmp:
    observed = Path(tmp) / "fish-signal-count"
    shell = Shell(Path(sys.argv[1]).resolve(), tmp, config=(
        "set -g fish_signal_count 0; "
        "function fish_signal_observer --on-signal USR1; "
        "set -g fish_signal_count (math $fish_signal_count + 1); "
        f"printf '%s\\n' $fish_signal_count > {observed}; end; "
    ))
    try:
        shell.run("count=0; trap '((count+=1))' USR1")
        os.kill(shell.child.pid, signal.SIGUSR1)
        shell.run("printf 'TRAPPED:%s\\n' \"$count\"", "TRAPPED:1\r\n")
        os.kill(shell.child.pid, signal.SIGUSR1)
        shell.run("printf 'RESUMED:%s\\n' \"$count\"", "RESUMED:2\r\n")
        assert observed.read_text() == "2\n"

        shell.run("(trap '' USR1); value=$(trap '' USR1; printf child); kill -USR1 $$; "
                  "printf 'SCOPED:%s:%s\\n' \"$count\" \"$value\"", "SCOPED:3:child\r\n")
        shell.run("(kill -USR1 $$); printf 'PARENT:%s\\n' \"$count\"", "PARENT:4\r\n")
        shell.run("(trap 'echo unsupported' USR1)", status=2)
        shell.run("(trap '' USR1; /bin/sh -c 'kill -USR1 $$; printf \"CHILD-IGNORED\\n\"')", "CHILD-IGNORED\r\n")
        shell.run("trap '' USR1")
        shell.run("/bin/sh -c 'kill -USR1 $$; printf \"IGNORED\\n\"'", "IGNORED\r\n")
        shell.run("trap - USR1")
        shell.run('private_delay=12345; /bin/sleep "$private_delay" & jobs > "$HOME/job-display"')
        display = (Path(tmp) / "job-display").read_text()
        assert '$private_delay' in display and '12345' not in display and 'command --' not in display, display
        shell.run('kill "$!"; wait "$!"', status=128 + signal.SIGTERM)
        shell.signal_job("while true; do :; done", "c", 130)
        shell.run("printf 'ALIVE\\n'", "ALIVE\r\n")
        shell.child.sendline("exit")
        shell.child.expect(pexpect.EOF)
    finally:
        if sys.exc_info()[0]:
            Path("/private/tmp/fish-rash-signals.log").write_bytes(shell.log.getvalue())
        shell.close()
    print("PASS Fish listeners coexist with Rash traps, "
          "child trap scopes restore, external commands inherit ignored signals, and builtin loops interrupt")
