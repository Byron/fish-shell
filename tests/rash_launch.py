#!/usr/bin/env python3
"""PTY coverage of Fish's Rash launcher defaults and argument forwarding."""
import os
from pathlib import Path
import shlex
import sys
import tempfile
import time

from bash_input_integration import Shell


def fixture(home, selected='.bash_profile'):
    (home / 'config/fish').mkdir(parents=True)
    (home / 'config/fish/config.fish').write_text(
        'printf "config\\n" >> "$HOME/fish-startups"\n')
    files = ('.bash_profile', '.bash_login', '.profile')
    for name in ('.bashrc', *files[files.index(selected):]):
        (home / name).write_text(
            f'export STARTUP_FILE={shlex.quote(name)}\n'
            'export STARTUP_INIT="$WRAPPER_INIT"\n'
            f'printf "{name}\\n" >> "$HOME/bash-startups"\n')
    (home / '.bash_logout').write_text('printf "logout\\n" >> "$HOME/logouts"\n')


def main(binary):
    with tempfile.TemporaryDirectory(prefix='fish-rash-launch-') as tmp:
        root = Path(tmp).resolve()
        cases = [
            ('nonlogin', False, [], '.bashrc', False, '.bash_profile'),
            ('login', True, [], '.bash_profile', True, '.bash_profile'),
            ('bash-login-fallback', True, [], '.bash_login', True, '.bash_login'),
            ('unreadable-profile', True, [], '.bash_login', True, '.bash_login'),
            ('profile-fallback', True, [], '.profile', True, '.profile'),
            ('no-login', True, ['--no-login'], '.bashrc', False, '.bash_profile'),
            ('explicit-login', False, ['--login'], '.bash_profile', True, '.bash_profile'),
            ('short-login', False, ['-l'], '.bash_profile', True, '.bash_profile'),
            ('no-startup', False, ['--no-startup'], None, False, '.bash_profile'),
            ('login-no-startup', True, ['--no-startup'], None, False, '.bash_profile'),
            ('no-fish-config', False, ['-N'], '.bashrc', False, '.bash_profile'),
            ('init-help-argument', False, ['-C', '--help'], '.bashrc', False, '.bash_profile'),
            ('init-startup-argument', False, ['-C', '--no-startup'], '.bashrc', False, '.bash_profile'),
            ('private-parent', False, [], '.bashrc', False, '.bash_profile'),
        ]
        for name, parent_login, args, startup, logout, selected in cases:
            home = root / name
            fixture(home, selected)
            if name == 'unreadable-profile':
                denied = home / '.bash_profile'
                denied.write_text('export STARTUP_FILE=unreadable\n')
                denied.chmod(0)
                if os.access(denied, os.R_OK):
                    print('SKIP unreadable-profile: current user can read mode-000 files', flush=True)
                    continue
            private = name == 'private-parent'
            history = home / 'data/fish/bash-history/fish_history'
            existing = '- cmd: echo private_history_sentinel\n  when: 1\n'
            if private:
                history.parent.mkdir(parents=True)
                history.write_text(existing)
            shell = Shell(binary, home, bash=False, login=parent_login, private=private,
                          config='set -g startup --bash-startup; '
                                 'set -g _flag_help user; set -g _flag_no_startup user; '
                                 'set -g _flag_login user; set -g _flag_no_login user; ')
            try:
                assert not (home / 'bash-startups').exists(), 'Fish must not load Bash startup'
                init = ("set -g fish_greeting; function fish_prompt; printf 'RASH> '; end; "
                        "set -gx WRAPPER_INIT forwarded")
                if private:
                    init += (f"; bind ctrl-g 'commandline --current-buffer > {shell.dump}; "
                             f"commandline --cursor > {shell.cursor}'; "
                             "bind ctrl-x 'commandline --replace \"\"'")
                command = ['rash', '--features=mark-prompt', '-C', init, *args]
                shell.child.sendline(shlex.join(command))
                shell.marker(b'\x1b]133;C;')
                shell.marker(b'RASH> ')
                shell.marker(b'\x1b]133;B')
                child_pid = home / 'child-pid'
                shell.run(f'printf "%s\\n" "$$" > {shlex.quote(str(child_pid))}')
                assert os.tcgetpgrp(shell.child.child_fd) == int(child_pid.read_text())
                expected = startup or 'unset'
                forwarded = 'forwarded' if startup else 'unset'
                shell.run('printf "STARTUP:%s:%s:%s\\n" "${STARTUP_FILE-unset}" '
                          '"${STARTUP_INIT-unset}" "$WRAPPER_INIT"',
                          f'STARTUP:{expected}:{forwarded}:forwarded\r\n')
                if startup:
                    assert (home / 'bash-startups').read_text() == startup + '\n'
                else:
                    assert not (home / 'bash-startups').exists()
                config_count = 1 if '-N' in args else 2
                assert (home / 'fish-startups').read_text() == 'config\n' * config_count
                if private:
                    shell.child.send('echo private_history_')
                    time.sleep(.15)
                    shell.child.send('\x1b[C')
                    assert shell.buffer()[0] == 'echo private_history_'
                    shell.clear()
                    shell.run('echo private_launch_value', 'private_launch_value\r\n')
                shell.child.sendline('exit 23')
                shell.finished(23)
                assert os.tcgetpgrp(shell.child.child_fd) == shell.child.pid
                assert (home / 'logouts').exists() == logout
                if logout:
                    assert (home / 'logouts').read_text() == 'logout\n'
                shell.run('set -q STARTUP_FILE; or set -q WRAPPER_INIT', status=1)
                shell.run('printf "GLOBAL:%s\\n" $startup', 'GLOBAL:--bash-startup\r\n')
                if private:
                    assert history.read_text() == existing
                    assert not (home / 'data/fish/fish_history').exists()
            finally:
                if sys.exc_info()[0]:
                    Path('/private/tmp/fish-rash-launch.log').write_bytes(shell.log.getvalue())
                shell.close()
        print('PASS Rash startup/login/private inheritance, profile precedence, overrides, -N separation and -C forwarding', flush=True)

        home = root / 'help'
        fixture(home)
        shell = Shell(binary, home, bash=False,
                      config="function __fish_print_help; printf 'HELP:%s\\n' $argv; end; ")
        try:
            for option in ('--help', '-h'):
                shell.run('rash ' + option, 'HELP:rash\r\n')
            shell.run('rash -- --help', status=2)
            assert not (home / 'bash-startups').exists()
            assert (home / 'fish-startups').read_text() == 'config\n'
        finally:
            if sys.exc_info()[0]:
                Path('/private/tmp/fish-rash-launch.log').write_bytes(shell.log.getvalue())
            shell.close()
        print('PASS wrapper help and option terminator preserve argument boundaries', flush=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
