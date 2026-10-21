#!/usr/bin/env python3
"""Headless native Tmuxx transport trial; does not certify the Rustty GUI.

Usage: python3 tests/bash_input_tmuxx.py FISH TMUXX [--rash-shell [--bash-startup]]
"""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('fish', type=Path)
parser.add_argument('tmuxx', type=Path)
parser.add_argument('--bash-startup', action='store_true')
parser.add_argument('--rash-shell', action='store_true')
args = parser.parse_args()
if args.bash_startup and not args.rash_shell:
    parser.error("--bash-startup requires --rash-shell")
fish, tmuxx = str(args.fish.resolve()), str(args.tmuxx.resolve())
with tempfile.TemporaryDirectory(prefix='fish-tmuxx-', dir='/tmp') as tmp:
    root = Path(tmp)
    (root / 'cache/fish/generated_completions').mkdir(parents=True)
    env = dict(os.environ, HOME=tmp, XDG_CONFIG_HOME=tmp + '/config',
               XDG_DATA_HOME=tmp + '/data', XDG_CACHE_HOME=tmp + '/cache')
    env.pop('PHASE_TWO_NATIVE_RC', None)
    (root / '.bashrc').write_text('export PHASE_TWO_NATIVE_RC=loaded\n')
    socket = tmp + '/s'
    raw = root / 'raw'
    base = [tmuxx, '-S', socket, '-f', '/dev/null']
    native = [tmuxx, '--native-endpoint', socket + '.native']
    def call(*args, endpoint=native):
        return subprocess.check_output(endpoint + list(args), env=env, timeout=10)
    def until(needle):
        end = time.monotonic() + 6
        while time.monotonic() < end:
            text = raw.read_bytes() if raw.exists() else b''
            if needle in text:
                return text
            time.sleep(.02)
        raise AssertionError((needle, text[-3000:]))
    def send(source):
        call('send-keys', '-t', 'trial', '-l', source)
        call('send-keys', '-t', 'trial', 'Enter')
    try:
        startup = ['--bash-startup'] if args.bash_startup else []
        mode = '--rash-shell' if args.rash_shell else '--interactive-language=bash'
        call('new-session', '-d', '-s', 'trial', fish, mode, *startup,
             '--features=mark-prompt', '-C',
             "set -g fish_greeting; function fish_prompt; printf 'NATIVE> '; end",
             endpoint=base)
        call('pipe-pane', '-t', 'trial', '-o', 'cat > ' + str(raw))
        send("printf 'NATIVE_RC:%s\\n' \"$PHASE_TWO_NATIVE_RC\"")
        until(b'NATIVE_RC:loaded\r\n' if args.bash_startup else b'NATIVE_RC:\r\n')
        send("value=persistent; cd /; printf 'NATIVE:%s\\n' \"$value\"")
        output = until(b'NATIVE:persistent\r\n')
        until(b'\x1b]133;D;0')
        until(b'\x1b]7;file://')
        send('sleep 30')
        time.sleep(.15)
        call('send-keys', '-t', 'trial', 'C-z')
        until(b'has stopped')
        if not args.rash_shell:
            send('fish_input_mode fish')
            until(b'cmdline_url=fish_input_mode%20fish')
        send('fg')
        time.sleep(.15)
        call('send-keys', '-t', 'trial', 'C-c')
        until(b'\x1b]133;D;130' if args.rash_shell else b'\x1b]133;D;0')
        if args.rash_shell:
            send('echo RASH:$value')
            until(b'RASH:persistent\r\n')
        else:
            send('echo FISH:$version')
            until(b'FISH:4.')
        if args.bash_startup:
            send('echo RC_FISH:$PHASE_TWO_NATIVE_RC')
            until(b'RC_FISH:loaded\r\n')
        path = call('display-message', '-p', '-t', 'trial', '#{pane_current_path}').strip()
        assert path == b'/', path
        call('resize-window', '-t', 'trial', '-x', '100', '-y', '32')
        snapshot = call('--snapshot')
        assert snapshot, 'empty native snapshot'
        raw_output = raw.read_bytes()
        for marker in [b'\x1b]133;A', b'\x1b]133;B', b'\x1b]133;C', b'\x1b]133;D', b'\x1b]7;file://']:
            assert marker in raw_output, marker
        print('PASS native Tmuxx direct argv, ' + ('startup export, ' if args.bash_startup else 'startup isolation, ')
              + ('Rash state' if args.rash_shell else 'Bash-style Fish state/switch')
              + ', Ctrl-Z/C, cwd, resize, snapshot, OSC 133/7')
    finally:
        subprocess.run(base + ['kill-server'], env=env, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
