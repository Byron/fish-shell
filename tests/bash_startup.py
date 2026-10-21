#!/usr/bin/env python3
"""Isolated PTY coverage of explicit Bash startup and live parser state."""
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time

import pexpect
from bash_input_integration import Shell


def exited(shell, status):
    shell.marker(pexpect.EOF)
    shell.child.close()
    assert shell.child.exitstatus == status, (status, shell.child.exitstatus, shell.log.getvalue())


def main(binary):
    with tempfile.TemporaryDirectory(prefix='fish-bash-startup-') as tmp:
        root = Path(tmp).resolve()
        counter = 0

        def home():
            nonlocal counter
            counter += 1
            path = root / str(counter)
            (path / 'config/fish').mkdir(parents=True)
            return path

        for login in (False, True):
            h = home()
            for name in ('.bashrc', '.bash_profile', '.bash_login', '.profile'):
                (h / name).write_text('echo loaded >> "$HOME/unexpected"\n')
            s = Shell(binary, h, login=login)
            try:
                s.run('echo NORMAL', 'NORMAL\r\n')
                assert not (h / 'unexpected').exists()
            finally:
                s.close()
        print('PASS Bash startup stays off by default, including Fish login', flush=True)

        h = home()
        (h / '.bashrc').write_text('export WRONG_STARTUP=yes\n')
        (h / 'posix-env').write_text('export POSIX_STARTUP=yes\n')
        s = Shell(binary, h, startup=True,
                  config=f'set -gx POSIXLY_CORRECT 1; set -gx ENV {h}/posix-env; ')
        try:
            s.run('echo POSIX:$POSIX_STARTUP:${WRONG_STARTUP-unset}', 'POSIX:yes:unset\r\n')
        finally:
            s.close()
        print('PASS native interactive POSIX ENV startup selection', flush=True)

        h = home()
        repo = h / 'repo ü space'
        repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), '-c', 'commit.gpgsign=false', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-q', '--allow-empty', '-m', 'fixture'], check=True)
        subprocess.run(['git', '-C', str(repo), 'branch', 'topic-startup'], check=True)
        (h / 'config/fish/config.fish').write_text(
            'set -gx ORDER fish-config\nfunction fish_only; echo FISH_FUNCTION; end\n')
        (h / '.bashrc').write_text('''
export ORDER="$ORDER:bash-rc"
repo="$HOME/repo ü space"
cd "$repo"
rc_func() { printf 'RC:%s\\n' "$ORDER"; }
alias rc_alias=rc_func
alias begin='if true; then'
shopt -s extglob
bind '"\\C-a": beginning-of-line'
PS1='$(touch "$HOME/ps1-executed")BAD_PS1'
HISTFILE="$HOME/native-history"
history -s rc-history-entry
prompt_observer() { local previous=$?; printf '%s\\n' "$previous" >> "$HOME/prompts"; }
PROMPT_COMMAND=prompt_observer
trap 'printf D >> "$HOME/debug-trap"' DEBUG
trap 'printf C >> "$HOME/chld-trap"' CHLD
''')
        (h / 'native-history').write_text('existing Bash history\n')
        probe = h / 'syntax-result'
        config = ("set -gx ORDER $ORDER:fish-init; "
                  f"bind ctrl-v 'commandline --is-valid; echo $status > {probe}'; "
                  f"function observe --on-event fish_postexec; printf '%s\\n' $ORDER $PWD > {h}/published; end; ")
        s = Shell(binary, h, startup=True, config=config)
        try:
            assert (h / 'prompts').read_text().splitlines() == ['0']
            assert not (h / 'ps1-executed').exists()
            assert b'line editing not enabled' not in s.log.getvalue()
            assert b'BAD_PS1' not in s.log.getvalue()
            s.run('rc_alias', 'RC:fish-config:fish-init:bash-rc\r\n')
            assert (h / 'published').read_text().splitlines() == ['fish-config:fish-init:bash-rc', str(repo)]
            s.run('[[ abc == +(a|b|c) ]]; echo EXT:$?', 'EXT:0\r\n')
            s.run('begin echo ALIAS_GRAMMAR; fi', 'ALIAS_GRAMMAR\r\n')
            s.run('false', status=1)
            assert (h / 'prompts').read_text().splitlines() == ['0', '0', '0', '0', '1']
            s.run('echo PREVIOUS:$?', 'PREVIOUS:1\r\n')
            before = {name: (h / name).read_bytes() if (h / name).exists() else b''
                      for name in ('debug-trap', 'chld-trap', 'prompts')}
            source = 'echo "$(touch "$HOME/unfinished-executed")" "${probe_assigned:=bad}"'
            s.child.send(source)
            for _ in range(3):
                probe.unlink(missing_ok=True)
                s.child.sendcontrol('v')
                deadline = time.monotonic() + 5
                while not probe.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                assert probe.read_text() == '0\n'
            assert not (h / 'unfinished-executed').exists()
            assert before == {name: (h / name).read_bytes() if (h / name).exists() else b'' for name in before}
            s.clear()
            s.run('echo UNSET:${probe_assigned-unset}', 'UNSET:unset\r\n')
            s.child.send('git -C "$repo" switch topic-\t')
            text, cursor = s.buffer()
            assert text == 'git -C "$repo" switch topic-startup ', (text, cursor)
            s.clear()
            s.run('trap - DEBUG CHLD')
            s.run('set -v')
            s.run('echo unexpected EOF', 'unexpected EOF\r\n')
            s.run('set +v')
            s.child.sendline('exit')
            exited(s, 0)
            assert (h / 'native-history').read_text() == 'existing Bash history\n'
            assert not (h / 'ps1-executed').exists()
        finally:
            s.close()
        print('PASS Fish/config/init/rc order, state publication, aliases/extglob, Git Tab, PROMPT_COMMAND, safe syntax and history', flush=True)

        for selected in ('.bash_profile', '.bash_login', '.profile'):
            h = home()
            names = ('.bash_profile', '.bash_login', '.profile')
            for name in names[names.index(selected):]:
                (h / name).write_text(f'export STARTUP_FILE={shlex.quote(name)}\n')
            (h / '.bashrc').write_text('export RC_UNEXPECTED=yes\n')
            (h / '.bash_logout').write_text('echo logout >> "$HOME/logout"\n')
            s = Shell(binary, h, startup=True, login=True)
            try:
                s.run('echo LOGIN:$STARTUP_FILE:${RC_UNEXPECTED-unset}', f'LOGIN:{selected}:unset\r\n')
                s.run('trap \'echo trap >> "$HOME/logout"\' EXIT')
                assert not (h / 'logout').exists()
                s.child.sendline('exit 17')
                exited(s, 17)
                assert (h / 'logout').read_text() == 'logout\ntrap\n'
            finally:
                s.close()
        print('PASS login file precedence, no implicit login bashrc, logout before EXIT trap and exit status', flush=True)

        for startup, login, trap_status in ((True, True, None), (True, True, 23),
                                           (False, True, None), (True, False, None)):
            h = home()
            (h / '.bash_logout').write_text('echo logout >> "$HOME/logout"\n')
            s = Shell(binary, h, startup=startup, login=login)
            try:
                trap = 'echo trap >> "$HOME/logout"'
                if trap_status is not None:
                    trap += f'; exit {trap_status}'
                s.run('trap ' + shlex.quote(trap) + ' EXIT')
                s.run("/bin/sh -c 'exit 17'", status=17)
                s.child.sendcontrol('d')
                exited(s, trap_status or 17)
                expected = 'logout\ntrap\n' if startup and login else 'trap\n'
                assert (h / 'logout').read_text() == expected
            finally:
                s.close()
        print('PASS Ctrl-D runs login logout before EXIT once, honors trap status and startup/login flags', flush=True)

        for ending, expected, status in (("/bin/sh -c 'exit 17'", 'trap\n', 17),
                                        ('exit 17', 'logout\ntrap\n', 17)):
            h = home()
            (h / '.bash_profile').write_text('')
            (h / '.bash_logout').write_text('echo logout >> "$HOME/logout"\n')
            script = h / 'script.bash'
            script.write_text('trap \'echo trap >> "$HOME/logout"\' EXIT\n' + ending + '\n')
            result = subprocess.run([binary, '-N', '--login', '--bash-startup',
                                     '--script-language=bash', script],
                                    env=dict(HOME=str(h), PATH='/usr/bin:/bin'), capture_output=True)
            assert result.returncode == status, (result.returncode, result.stderr)
            assert (h / 'logout').read_text() == expected
        print('PASS noninteractive login EOF skips logout while explicit exit runs it before EXIT', flush=True)

        h = home()
        (h / '.bashrc').write_text('export RC_SOURCED=yes\n')
        (h / '.bash_profile').write_text('. "$HOME/.bashrc"\n')
        (h / '.bash_logout').write_text('echo logout > "$HOME/logout"\n')
        s = Shell(binary, h, startup=True, login=True)
        try:
            s.run('echo SOURCED:$RC_SOURCED', 'SOURCED:yes\r\n')
            s.run('fish_input_mode fish', status=127)
            assert not (h / 'logout').exists()
            s.run('rash_variable=normal; echo "$rash_variable"', 'normal\r\n')
            s.child.sendline('exit')
            exited(s, 0)
            assert (h / 'logout').read_text() == 'logout\n'
        finally:
            s.close()
        print('PASS explicit profile sourcing, fixed Rash language and logout on shell exit', flush=True)

        for source, status in [('exit 23', 23), ("exec /bin/sh -c 'sleep .1; exit 29'", 29),
                               ("PROMPT_COMMAND='exit 31'", 31)]:
            h = home()
            (h / '.bashrc').write_text(source + '\n')
            s = Shell(binary, h, startup=True, wait_prompt=False)
            try:
                exited(s, status)
                assert b'PROBE>' not in s.log.getvalue()
            finally:
                s.close()
        print('PASS rc/PROMPT_COMMAND exit and exec preserve status without a Fish prompt', flush=True)

        for exec_exit in (False, True):
            h = home()
            (h / 'native-history').write_text('untouched\n')
            (h / '.bashrc').write_text('HISTFILE="$HOME/native-history"\nhistory -s rc-entry\n')
            s = Shell(binary, h, startup=True)
            try:
                if exec_exit:
                    s.child.sendline("exec /bin/sh -c 'exit 0'")
                    exited(s, 0)
            finally:
                s.close()
            assert (h / 'native-history').read_text() == 'untouched\n'
        print('PASS exec and HUP cannot automatically save into rc-selected Bash history', flush=True)

        for source in ('false\nexport AFTER_ERROR=yes', 'fish_input_mode fish\nexport AFTER_MISSING_COMMAND=yes'):
            h = home()
            (h / '.bashrc').write_text(source + '\n')
            s = Shell(binary, h, startup=True)
            try:
                if source.startswith('false'):
                    s.run('echo ERROR_CONTINUED:$AFTER_ERROR', 'ERROR_CONTINUED:yes\r\n')
                else:
                    s.run('echo STILL_RASH:$AFTER_MISSING_COMMAND', 'STILL_RASH:yes\r\n')
                assert os.tcgetpgrp(s.child.child_fd) == s.child.pid
            finally:
                s.close()
        h = home()
        (h / '.bashrc').write_text('sleep 5.2\nexport SLOW_FINISHED=yes\n')
        s = Shell(binary, h, startup=True)
        try:
            s.run('echo SLOW:$SLOW_FINISHED', 'SLOW:yes\r\n')
        finally:
            s.close()
        h = home()
        (h / '.bashrc').write_text('echo STARTUP_SLEEP\nsleep 30\n')
        s = Shell(binary, h, startup=True, wait_prompt=False)
        try:
            s.marker(b'STARTUP_SLEEP')
            time.sleep(.1)
            s.child.sendcontrol('c')
            s.marker(b'\x1b]133;B')
            assert os.tcgetpgrp(s.child.child_fd) == s.child.pid
            s.run('echo INTERRUPTED_STARTUP', 'INTERRUPTED_STARTUP\r\n')
        finally:
            s.close()
        print('PASS rc errors, unavailable Fish command, slow rc and interrupt restore terminal ownership', flush=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
