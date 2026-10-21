#!/usr/bin/env python3
"""Compare Rash script behavior with an explicitly supplied GNU Bash oracle."""
import argparse
import os
from pathlib import Path
import signal
import shutil
import select
import subprocess
import tempfile
import time


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("fish", type=Path)
parser.add_argument("oracle", type=Path, help="GNU Bash executable used only as the reference")
options = parser.parse_args()
fish = options.fish.resolve()
oracle = options.oracle.resolve()
with tempfile.TemporaryDirectory(prefix="fish-bash-scripts-") as tmp:
    home = Path(tmp)
    env = os.environ.copy()
    env.update(HOME=tmp, XDG_CONFIG_HOME=str(home / "config"),
               XDG_DATA_HOME=str(home / "data"), BASH_ENV="", ENV="")
    script = home / "script ü;$.bash"
    workloads = [
        # An explicit script language overrides a file's shebang.
        '#!/usr/bin/env fish\na=(one "two three"); printf "%s:%s:%s\\n" "${a[1]}" "$0" "$#"; printf "<%s>\\n" "$@"',
        'IFS= read -r line; printf "stdin:%s\\n" "$line"; printf "stderr\\n" >&2; exit 37',
        'exec /bin/sh -c \'printf "exec:%s\\n" "$1"; exit 23\' child "$1"',
        'trap \'printf "exiting:%s\\n" "$?"\' EXIT; false',
    ]
    args = ["", 'a "quote"; $(touch INJECTED)', "ü * ?", b"raw-\xff"]
    for source in workloads:
        script.write_text(source)
        reference = subprocess.run([oracle, "--noprofile", "--norc", "--", script, *args],
                                   env=env, input=b"from stdin\n", capture_output=True)
        actual = subprocess.run([fish, "-N", "--script-language=bash", "--", script, *args],
                                env=env, input=b"from stdin\n", capture_output=True)
        assert (actual.returncode, actual.stdout, actual.stderr) == (
            reference.returncode, reference.stdout, reference.stderr
        ), (source, reference, actual)
    print("PASS isolated Bash scripts: shebang, argv/$0/raw bytes, stdio, exec, traps and exit status")

    for sig in (signal.SIGTERM, signal.SIGINT):
        script.write_text(f"kill -{sig.value} $$\n")
        reference = subprocess.run([oracle, "--noprofile", "--norc", "--", script],
                                   env=env, capture_output=True)
        actual = subprocess.run([fish, "-N", "--script-language=bash", "--", script],
                                env=env, capture_output=True)
        assert actual.returncode == reference.returncode == -sig, (sig, reference, actual)
    print("PASS isolated Bash scripts: SIGTERM and SIGINT retain native signal termination")

    # Configuration stays Fish; Bash script dispatch does not run Fish exit hooks.
    (home / "config/fish").mkdir(parents=True, exist_ok=True)
    (home / "config/fish/config.fish").write_text(
        "set -gx SCRIPT_STATE fish\n"
        "function script_exit --on-event fish_exit\n"
        "    printf 'fish-exit:%s:%s\\n' $argv[1] $SCRIPT_STATE\n"
        "end\n"
    )
    script.write_text('printf "CONFIG:%s\\n" "$SCRIPT_STATE"; export SCRIPT_STATE=bash; exit 37\n')
    actual = subprocess.run([fish, "--script-language=bash", "--", script],
                            env=env, capture_output=True)
    assert actual.returncode == 37 and actual.stdout == b"CONFIG:fish\n", actual
    print("PASS isolated Bash scripts: Fish configuration precedes evaluation; Fish exit hooks do not run")

    without_backend = home / "without-backend"
    without_backend.mkdir()
    copied_fish = without_backend / "fish"
    shutil.copy2(fish, copied_fish)
    script.write_text('value=retained\nf() { printf "INPROCESS:%s\\n" "$value"; }\n'
                      'f\ndeclare -f f\ndeclare -F f missing\nprintf "QUERY:%s\\n" "$?"\nexit 37\n')
    standalone_env = dict(env, PATH=str(home / "no-executables"))
    actual = subprocess.run([copied_fish, "-N", "--script-language=bash", "--", script],
                            env=standalone_env, capture_output=True)
    expected = (b'INPROCESS:retained\nf () \n{ \n    printf "INPROCESS:%s\\n" "$value"\n}\n'
                b'f\nQUERY:1\n')
    assert (actual.returncode, actual.stdout, actual.stderr) == (37, expected, b""), actual
    print("PASS isolated Bash scripts: copied Fish evaluates Bash without a sibling executable or PATH tools")

    def line_before_deadline(stream, timeout=3):
        deadline = time.monotonic() + timeout
        line = bytearray()
        while not line.endswith(b"\n"):
            remaining = deadline - time.monotonic()
            assert remaining > 0 and select.select([stream], [], [], remaining)[0], "timed out waiting for output"
            byte = os.read(stream.fileno(), 1)
            assert byte, ("unexpected EOF", line)
            line.extend(byte)
        return bytes(line)

    def read_signal_trial(program, flags, exit_from_trap):
        script.write_text(
            "trap 'printf \"TRAP\\n\"" + ("; exit 17" if exit_from_trap else "") + "' USR1\n"
            "printf 'READY\\n'\nIFS= read -r value\nprintf 'READ:%s:<%s>\\n' \"$?\" \"$value\"\n"
        )
        process = subprocess.Popen([program, *flags, "--", script], env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout = line_before_deadline(process.stdout)
            assert stdout == b"READY\n", stdout
            process.stdin.write(b"prefix")
            process.stdin.flush()
            time.sleep(.05)
            process.send_signal(signal.SIGUSR1)
            stdout += line_before_deadline(process.stdout)
            assert stdout == b"READY\nTRAP\n", stdout
            if exit_from_trap:
                # Keep stdin open: exiting must not depend on EOF waking read.
                process.wait(timeout=3)
            else:
                assert process.poll() is None, "ordinary trap should resume Bash 3.2 read"
                process.stdin.write(b"-rest\n")
                process.stdin.flush()
            tail, stderr = process.communicate(timeout=3)
            return process.returncode, stdout + tail, stderr
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=3)

    for exit_from_trap in (False, True):
        expected = read_signal_trial(oracle, ["--noprofile", "--norc"], exit_from_trap)
        actual = read_signal_trial(fish, ["-N", "--script-language=bash"], exit_from_trap)
        assert actual == expected, (exit_from_trap, expected, actual)
        assert actual == ((17, b"READY\nTRAP\n", b"") if exit_from_trap else
                          (0, b"READY\nTRAP\nREAD:0:<prefix-rest>\n", b"")), actual
    print("PASS isolated Bash scripts: trapped read resumes partial input or exits while stdin remains open")

    for command in ("(kill -USR1 $$)", "value=$(kill -USR1 $$)"):
        script.write_text("trap 'printf \"PARENT\\n\"' USR1\n" + command + "\nprintf 'AFTER\\n'\n")
        expected = subprocess.run([oracle, "--noprofile", "--norc", "--", script],
                                  env=env, capture_output=True, timeout=3)
        actual = subprocess.run([fish, "-N", "--script-language=bash", "--", script],
                                env=env, capture_output=True, timeout=3)
        assert (actual.returncode, actual.stdout, actual.stderr) == (
            expected.returncode, expected.stdout, expected.stderr
        ) == (0, b"PARENT\nAFTER\n", b""), (command, expected, actual)
    print("PASS isolated Bash scripts: subshell and substitution signals preserve the parent trap")

    def signal_trial(program, flags, sig, delivery, disposition):
        with tempfile.TemporaryDirectory(dir=home) as trial:
            trial = Path(trial)
            pidfile, release, source = trial / "pid", trial / "release", trial / "script"
            trap = ""
            if disposition == "trap":
                trap = f"trap 'count=$((count+1))' {sig.value}\n"
            elif disposition == "ignore":
                trap = f"trap '' {sig.value}\n"
            source.write_text(
                f"count=0\n{trap}printf '%s\\n' \"$$\" > {pidfile}\n"
                f"while [[ ! -e {release} ]]; do :; done\nprintf 'count:%s\\n' \"$count\"\n"
            )
            process = subprocess.Popen([program, *flags, "--", source], env=env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       start_new_session=True)
            try:
                deadline = time.monotonic() + 3
                while not pidfile.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                assert int(pidfile.read_text()) == process.pid, "The evaluator must retain the invoked PID"
                if delivery == "group":
                    os.killpg(process.pid, sig)
                else:
                    process.send_signal(sig)
                if disposition != "default":
                    time.sleep(.05)
                    assert process.poll() is None, (sig, delivery, disposition)
                    release.touch()
                stdout, stderr = process.communicate(timeout=3)
                return process.returncode, stdout, stderr
            finally:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=3)

    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        for delivery in ("pid", "group"):
            for disposition in ("default", "trap", "ignore"):
                expected = signal_trial(oracle, ["--noprofile", "--norc"], sig, delivery, disposition)
                actual = signal_trial(fish, ["-N", "--script-language=bash"], sig, delivery, disposition)
                assert actual == expected, (sig, delivery, disposition, expected, actual)
                if disposition == "default":
                    assert actual[0] == -sig, actual
                else:
                    assert actual[:2] == (0, b"count:1\n" if disposition == "trap" else b"count:0\n"), actual
    print("PASS isolated Bash scripts: PID/group HUP, TERM and INT match native default, trap and ignore behavior")
