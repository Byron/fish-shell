#!/usr/bin/env python3
"""Bounded PTY acceptance checks for the dedicated Rash shell. Requires pexpect.

Usage: python3 tests/bash_input_integration.py target/debug/fish
The Bash evaluator is the in-process Rash crate.
"""
import io
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

import pexpect


class Shell:
    def __init__(self, binary, home, bash=True, config='', private=False, startup=False, login=False,
                 wait_prompt=True, dialect=False):
        self.home = Path(home)
        (self.home / 'cache/fish/generated_completions').mkdir(parents=True, exist_ok=True)
        self.dump = self.home / 'buffer'
        self.cursor = self.home / 'cursor'
        self.env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME=str(home),
                        XDG_CONFIG_HOME=str(self.home / 'config'),
                        XDG_DATA_HOME=str(self.home / 'data'),
                        XDG_CACHE_HOME=str(self.home / 'cache'),
                        TERM='xterm-256color', LANG='en_US.UTF-8')
        init = ("set -g fish_greeting; function fish_prompt; printf 'PROBE> '; end; "
                f"bind ctrl-g 'commandline --current-buffer > {self.dump}; commandline --cursor > {self.cursor}'; "
                "bind ctrl-x 'commandline --replace \"\"'; " + config)
        args = ['--features=mark-prompt', '-C', init]
        if startup:
            args.insert(0, '--bash-startup')
        if login:
            args.insert(0, '--login')
        if private:
            args.insert(0, '--private')
        if dialect:
            args.insert(0, '--interactive-language=bash')
        elif bash:
            args.insert(0, '--rash-shell')
        self.child = pexpect.spawn(str(binary), args, env=self.env, timeout=10)
        self.child.delaybeforesend = 0
        self.log = io.BytesIO()
        self.child.logfile_read = self.log
        if wait_prompt:
            self.marker(b'\x1b]133;B')

    def marker(self, value):
        while True:
            i = self.child.expect_exact([value, b'\x1b[0c', b'\x1b[6n'])
            if i == 0:
                return
            self.child.send(b'\x1b[?1;2c' if i == 1 else b'\x1b[1;1R')

    def finished(self, status=None):
        self.marker(b'\x1b]133;D;')
        self.child.expect(rb'(\d+)\x1b\\')
        actual = int(self.child.match[1])
        if status is not None:
            assert actual == status, (actual, status)
        self.marker(b'\x1b]133;B')

    def run(self, source, expected=None, status=0):
        self.child.sendline(source)
        if expected is not None:
            self.child.expect_exact(expected.encode())
        self.finished(status)

    def signal_job(self, source, key, status):
        self.child.sendline(source)
        self.marker(b'\x1b]133;C;')
        # Drain output while Fish restores a stopped job's terminal with TCSADRAIN.
        deadline = time.monotonic() + .15
        while time.monotonic() < deadline:
            try:
                self.child.read_nonblocking(65536, timeout=.01)
            except pexpect.TIMEOUT:
                pass
        self.child.sendcontrol(key)
        self.finished(status)
        assert os.tcgetpgrp(self.child.child_fd) == self.child.pid

    def buffer(self):
        self.cursor.unlink(missing_ok=True)
        self.child.sendcontrol('g')
        end = time.monotonic() + 5
        while not self.cursor.exists() and time.monotonic() < end:
            try:
                self.child.read_nonblocking(65536, timeout=.003)
            except pexpect.TIMEOUT:
                pass
        assert self.cursor.exists(), 'buffer observer did not run'
        return self.dump.read_text()[:-1], int(self.cursor.read_text())

    def clear(self):
        self.child.sendcontrol('x')
        assert self.buffer() == ('', 0)

    def close(self):
        self.child.close(force=True)


def main(binary):
    with tempfile.TemporaryDirectory(prefix='fish-bash-acceptance-') as tmp:
        root = Path(tmp)
        home = root / 'home'
        home.mkdir()
        repo = root / 'repo ü space'
        repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), '-c', 'commit.gpgsign=false', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-q', '--allow-empty', '-m', 'fixture'], check=True)
        subprocess.run(['git', '-C', str(repo), 'branch', 'topic-completion'], check=True)
        state_file = home / 'state'
        config = (f"function observe --on-event fish_postexec; printf '%s\\n' $status $PWD $shared > {state_file}; end; "
                  "function fish_only; echo FISH_FUNCTION; end; set -g fish_only_value wrong; "
                  "function update_shared --on-event fish_postexec; if test \"$argv[1]\" = hook_trigger; set -gx shared from_fish; end; end; "
                  "complete -c mutate_probe -f -a '(commandline -r overwritten; echo candidate)'; "
                  "complete -c ordered_probe -f -k -a 'zeta alpha' -d description; "
                  "bind ctrl-o 'history save'; ")
        s = Shell(binary, home, config=config)
        try:
            s.run("value=retained; greeting() { printf 'VALUE:%s\\n' \"$value\"; }; alias hi=greeting")
            s.run('hi', 'VALUE:retained\r\n')
            s.run('false', status=1)
            s.run('echo PREVIOUS:$?', 'PREVIOUS:1\r\n')
            s.run("count=0; for i in 1 2 3; do ((count+=i)); done; echo COUNT:$count", 'COUNT:6\r\n')
            s.run("observed=0; trap '((observed+=1))' DEBUG")
            s.run("trap - DEBUG; echo OBSERVED:$observed", 'OBSERVED:1\r\n')
            s.run('export shared=from_bash')
            assert state_file.read_text().splitlines()[-1] == 'from_bash'
            s.run('unset shared')
            assert 'from_bash' not in state_file.read_text()
            s.run('exec 9>/dev/null; echo FD9:ok', 'FD9:ok\r\n')
            s.run('hook_trigger() { :; }')
            s.run('hook_trigger')
            s.run('echo SHARED:$shared', 'SHARED:from_fish\r\n')
            print('PASS persistent state, aliases, functions, status, and trap isolation', flush=True)

            s.child.sendline('for i in a b; do')
            time.sleep(.1)
            s.child.sendline('echo LOOP:$i')
            s.child.sendline('done')
            s.child.expect_exact(b'LOOP:a\r\nLOOP:b\r\n')
            s.finished(0)
            s.signal_job('sleep 30', 'z', 128 + 18 if sys.platform == 'darwin' else 128 + 20)
            s.signal_job('fg', 'c', 130)
            s.signal_job('while true; do :; done', 'c', 130)
            print('PASS multiline input, Ctrl-Z, fg, Ctrl-C, and terminal ownership', flush=True)

            s.run(f"repo='{repo}'")
            source = 'git -C "${repo}" switch topic-'
            s.child.send(source)
            s.child.send('\t')
            text, cursor = s.buffer()
            assert text == source + 'completion ', (text, cursor)
            assert cursor == len(text)
            s.clear()
            s.child.send('git --ver')
            s.child.send('\t')
            text, _ = s.buffer()
            assert text == 'git --version ', text
            s.clear()
            s.child.send('git --ver suffix')
            s.child.send('\x1b[D' * len(' suffix'))
            s.child.send('\t')
            text, cursor = s.buffer()
            assert text == 'git --version suffix', (text, cursor)
            assert cursor == len('git --version '), cursor
            s.clear()
            for prefix, expected in [('gree', 'greeting '), ('fish_on', 'fish_on')]:
                s.child.send(prefix + '\t')
                assert s.buffer()[0] == expected, s.buffer()
                s.clear()
            s.child.send('hi argument\t')
            assert s.buffer()[0] == 'hi argument'
            s.clear()
            s.run(f"cd '{root}'")
            filename = 'fixture-ü ;$()` file'
            (root / filename).write_text('LITERAL_FILE_OK\n')
            s.child.send('cat fixture-\t')
            text, _ = s.buffer()
            assert text.startswith('cat fixture-') and text.endswith(' '), text
            s.child.sendline('')
            s.child.expect_exact(b'LITERAL_FILE_OK\r\n')
            s.finished(0)
            (root / 'directory-completion').mkdir()
            s.child.send('cd directory-\t')
            assert s.buffer()[0] == 'cd directory-completion/'
            s.clear()
            for source in ['mutate_probe ', 'echo "$(touch sentinel)', 'echo "${side:=changed}"', 'git -C "$fish_only_value" switch topic-']:
                s.child.send(source + '\t')
                assert s.buffer()[0] == source, s.buffer()
                s.clear()
            assert not (root / 'sentinel').exists()
            s.child.send('ordered_probe \t')
            time.sleep(.1)
            s.buffer()
            assert b'description' in s.log.getvalue()
            s.clear()
            print('PASS unchanged Git provider, quoted variable provenance, options, and Tab insertion', flush=True)

            s.run('echo unique_history_value', 'unique_history_value\r\n')
            s.child.send('echo unique_hist')
            time.sleep(.2)
            s.child.send('\x1b[C')
            text, _ = s.buffer()
            assert text == 'echo unique_history_value', text
            s.clear()
            s.child.send('\x1b[A')
            text, _ = s.buffer()
            assert 'unique_history_value' in text, text
            s.clear()
            print('PASS Bash history suggestions and recall', flush=True)

            s.run('sleep 30 &')
            s.run('fish_input_mode fish', status=127)
            s.run('jobs', 'running\tsleep 30')
            s.run('fish_only', status=127)
            s.signal_job('fg', 'c', 130)
            s.run('cd /')
            assert b'\x1b]7;file://' in s.log.getvalue()
            assert re.search(rb'\x1b\]7;file://[^/\x07]*/\x07', s.log.getvalue())
            s.child.sendline('exit')
            s.child.expect(pexpect.EOF)
            print('PASS fixed Rash language, retained job, fg, OSC 133, and OSC 7', flush=True)
        finally:
            if sys.exc_info()[0]:
                Path("/private/tmp/fish-bash-integration.log").write_bytes(s.log.getvalue())
            s.close()
        assert (home / 'data/fish/bash-history/fish_history').exists()
        bash_history = (home / 'data/fish/bash-history/fish_history').read_text()
        assert 'value=retained' in bash_history
        fish_history = home / 'data/fish/fish_history'
        assert not fish_history.exists() or 'value=retained' not in fish_history.read_text()
        print('PASS Rash history stays separate from shared Fish history', flush=True)

        one, two = Shell(binary, home), Shell(binary, home)
        try:
            one.run('echo concurrent_one', 'concurrent_one\r\n')
            two.run('echo concurrent_two', 'concurrent_two\r\n')
            for shell in [one, two]:
                shell.child.sendline('exit')
                shell.child.expect(pexpect.EOF)
        finally:
            one.close()
            two.close()
        history = (home / 'data/fish/bash-history/fish_history').read_text()
        assert 'concurrent_one' in history and 'concurrent_two' in history
        private = Shell(binary, home, private=True)
        try:
            private.child.send('echo concurrent_')
            time.sleep(.15)
            private.child.send('\x1b[C')
            assert private.buffer()[0] == 'echo concurrent_'
            private.clear()
            private.run('echo private_value', 'private_value\r\n')
            private.child.sendline('exit')
            private.child.expect(pexpect.EOF)
        finally:
            private.close()
        assert 'private_value' not in (home / 'data/fish/bash-history/fish_history').read_text()
        print('PASS concurrent and private Bash history', flush=True)

        replacing = Shell(binary, home)
        try:
            replacing.child.sendline('exec sleep 30')
            replacing.marker(b'\x1b]133;C;')
            time.sleep(.15)
            replacing.child.sendcontrol('c')
            replacing.child.expect(pexpect.EOF)
            replacing.child.close()
            assert replacing.child.signalstatus == signal.SIGINT, (
                replacing.child.exitstatus, replacing.child.signalstatus
            )
        finally:
            replacing.close()
        print('PASS exec replacement terminal ownership and exit status', flush=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
