#!/usr/bin/env python3
"""Independent shell comparison; matching these fixtures is not full Bash conformance.

Example: python3 tests/rash_conformance.py --oracle /path/to/bash \
  --candidate target/debug/fish --candidate-arg=-N \
  --candidate-arg=--script-language=bash --report target/rash-conformance.json

GNU fixtures stay outside this repository. Add --gnu-tests PATH --gnu-helpers PATH
and one or more --gnu-case run-ifs. GNU cases require a Bash-compatible candidate
CLI, including recursive script and -c invocations; arguments are never translated.
"""
import argparse
import base64
from collections import Counter, defaultdict
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time


LIMIT = 8 * 1024 * 1024


def decode(value):
    return base64.b64decode(value['base64'], validate=True) if isinstance(value, dict) else value.encode()


def blob(value):
    return {'base64': base64.b64encode(value).decode(),
            'text': value.decode('utf-8', errors='backslashreplace')}


def run(argv, work, env, stdin=b'', timeout=10, limit=LIMIT):
    """Use files for stdio so a blocked reader or inherited pipe cannot hang cleanup."""
    start = time.monotonic()
    outcome = 'completed'
    with tempfile.TemporaryFile() as inp, tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        inp.write(stdin)
        inp.seek(0)
        process = subprocess.Popen(argv, cwd=work, env=env, stdin=inp, stdout=out,
                                   stderr=err, start_new_session=True)
        try:
            while process.poll() is None:
                if time.monotonic() - start >= timeout:
                    outcome = 'timeout'
                    break
                if os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size > limit:
                    outcome = 'output-limit'
                    break
                time.sleep(.005)
        finally:
            # Also remove same-group children left behind by an already-exited shell.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=3)
        size = os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size
        if size > limit:
            outcome = 'output-limit'
        out.seek(0)
        err.seek(0)
        return {'outcome': outcome, 'returncode': process.returncode,
                'stdout': blob(out.read(limit)), 'stderr': blob(err.read(limit)),
                'elapsed_seconds': round(time.monotonic() - start, 6)}


def artifacts(work):
    result, total = {}, 0
    for directory, dirs, files in os.walk(work, followlinks=False):
        if Path(directory) == work:
            dirs[:] = [name for name in dirs if name != '.harness']
        for name in sorted(dirs + files):
            path = Path(directory) / name
            info = path.lstat()
            entry = {'mode': stat.S_IMODE(info.st_mode)}
            if stat.S_ISLNK(info.st_mode):
                entry.update(kind='symlink', target=os.readlink(path))
            elif stat.S_ISREG(info.st_mode):
                total += info.st_size
                if total > LIMIT or len(result) >= 4096:
                    raise ValueError('filesystem observation limit exceeded')
                entry.update(kind='file', base64=base64.b64encode(path.read_bytes()).decode())
            else:
                entry['kind'] = 'directory' if stat.S_ISDIR(info.st_mode) else stat.S_IFMT(info.st_mode)
            result[str(path.relative_to(work))] = entry
    return result


def fixture_path(work, name):
    path = Path(name)
    if path.is_absolute() or '..' in path.parts or path.parts[:1] == ('.harness',):
        raise ValueError('fixture path must remain inside its workspace: ' + name)
    return work / path


def gnu_driver_path(tests, name):
    if not name.startswith('run-') or Path(name).name != name or not (tests / name).is_file():
        raise ValueError('invalid GNU fixture driver: ' + name)
    if name in ('run-all', 'run-gprof', 'run-minimal'):
        raise ValueError('select individual fixture drivers, not ' + name)
    path = tests / name
    if b'/tmp/' in path.read_bytes():
        raise ValueError('GNU fixture driver uses a fixed absolute temporary path: ' + name
                         + '; use an isolated OS environment for this legacy suite')
    return path


def execute(case, command, work, timeout, gnu_tests=None, gnu_helpers=None):
    if 'gnu_driver' in case:
        gnu_driver_path(gnu_tests, case['gnu_driver'])
    if work.exists():
        shutil.rmtree(work)
    if 'gnu_driver' in case:
        shutil.copytree(gnu_tests, work, symlinks=True)
    else:
        work.mkdir()
    support = work / '.harness'
    support.mkdir()
    (work / 'home').mkdir(exist_ok=True)
    # Fish creates its normal config/cache layout even with -N. Seed the same
    # empty layout on both sides so the diff measures script effects; do not
    # exclude these paths from artifact comparison (scripts may change them).
    for directory in ('config/fish/completions', 'config/fish/functions',
                      'config/fish/conf.d', 'data/fish', 'cache/fish'):
        (work / 'home' / directory).mkdir(parents=True, exist_ok=True)
    (work / 'home/config/fish/config.fish').touch()

    env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(work / 'home'),
           'XDG_CONFIG_HOME': str(work / 'home/config'), 'XDG_DATA_HOME': str(work / 'home/data'),
           'XDG_CACHE_HOME': str(work / 'home/cache'), 'BASH_ENV': '', 'ENV': '',
           'LC_ALL': 'C', 'LANG': 'C', 'TERM': 'dumb'}
    for name in case.get('directories', []):
        fixture_path(work, name).mkdir(parents=True, exist_ok=True)
    for name, content in case.get('files', {}).items():
        path = fixture_path(work, name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(decode(content))

    if 'gnu_driver' in case:
        launcher = support / 'shell'
        launcher.write_text('#!' + sys.executable + '\nimport os, sys\ncommand = ' + repr(command)
                            + '\nos.execv(command[0], command + sys.argv[1:])\n')
        launcher.chmod(0o700)
        log = support / 'diffs.jsonl'
        diff = support / 'diff'
        diff.write_text('#!' + sys.executable + '\nimport json, subprocess, sys\n'
                        + 'result = subprocess.run(' + repr([shutil.which('diff')]) + ' + sys.argv[1:])\n'
                        + 'with open(' + repr(str(log)) + ', "a") as out:\n'
                        + '    out.write(json.dumps({"args": sys.argv[1:], "returncode": result.returncode}) + "\\n")\n'
                        + 'raise SystemExit(result.returncode)\n')
        diff.chmod(0o700)
        env.update(THIS_SH=str(launcher), BUILD_DIR=str(gnu_helpers), TMPDIR=str(support),
                   BASH_TSTOUT=str(support / 'test-output'))
        env['PATH'] = str(support) + ':' + str(gnu_helpers) + ':' + env['PATH']
        argv = ['/bin/sh', './' + case['gnu_driver']]
    else:
        script = support / 'script ü;$.bash'
        script.write_text(case['source'])
        argv = command + ['--', str(script)] + [decode(arg) for arg in case.get('args', [])]
    before = artifacts(work)
    result = run(argv, work, env, decode(case.get('stdin', '')), timeout)
    after = artifacts(work)
    result['artifacts'] = {name: after.get(name, {'kind': 'removed'})
                           for name in sorted(before.keys() | after.keys())
                           if before.get(name) != after.get(name)}
    if 'gnu_driver' in case:
        result['diffs'] = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        output = base64.b64decode(result['stdout']['base64']) + base64.b64decode(result['stderr']['base64'])
        result['emitted_diff'] = bool(re.search(rb'(?m)^\d+(?:,\d+)?[acd]\d+(?:,\d+)?\r?$', output))
    return result


def compare(case, oracle, candidate, work, timeout=10, **kwargs):
    if case.get('skip_reason'):
        return {'name': case['name'], 'category': case['category'], 'status': 'skip',
                'reason': case['skip_reason']}
    expected = execute(case, oracle, work, timeout, **kwargs)
    actual = execute(case, candidate, work, timeout, **kwargs)
    different = [key for key in ('returncode', 'stdout', 'stderr', 'artifacts') if expected[key] != actual[key]]
    oracle_anomaly = any(d['returncode'] != 0 for d in expected.get('diffs', [])) or expected.get('emitted_diff')
    candidate_anomaly = any(d['returncode'] != 0 for d in actual.get('diffs', [])) or actual.get('emitted_diff')
    if expected['outcome'] != 'completed' or oracle_anomaly or ('gnu_driver' in case and expected['returncode']):
        status = 'oracle-fail'
    elif actual['outcome'] != 'completed':
        status = actual['outcome']
    else:
        status = 'fail' if different or candidate_anomaly else 'pass'
    return {'name': case['name'], 'category': case['category'], 'status': status,
            'different': different, 'oracle': expected, 'candidate': actual}


def identity(path):
    return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def gnu_identity(tests, helpers, drivers):
    # Hash the whole suite because selected drivers can recursively read fixtures.
    entries = []
    for path in sorted(tests.rglob('*')):
        if path.is_symlink():
            entry = {'symlink': os.readlink(path)}
        elif path.is_file():
            entry = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        else:
            continue
        entries.append((str(path.relative_to(tests)), entry, stat.S_IMODE(path.lstat().st_mode)))
    return {'path': str(tests),
            'tree_sha256': hashlib.sha256(json.dumps(entries, sort_keys=True).encode()).hexdigest(),
            'drivers': [identity(tests / name) for name in drivers],
            'helpers': [identity(helpers / name) for name in ('recho', 'zecho', 'printenv', 'xcase')]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--oracle', required=True, type=Path)
    parser.add_argument('--candidate', required=True, type=Path)
    parser.add_argument('--candidate-arg', action='append', default=[])
    parser.add_argument('--cases', type=Path, default=Path(__file__).with_name('rash_cases.json'))
    parser.add_argument('--case', action='append', help='Exact corpus case name; repeat to select cases')
    parser.add_argument('--gnu-tests', type=Path)
    parser.add_argument('--gnu-helpers', type=Path)
    parser.add_argument('--gnu-case', action='append', default=[])
    parser.add_argument('--timeout', type=float, default=10)
    parser.add_argument('--report', required=True, type=Path)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error('--timeout must be positive')
    if (args.gnu_tests or args.gnu_helpers) and not args.gnu_case:
        parser.error('GNU fixture paths require at least one --gnu-case')
    oracle_path, candidate_path = args.oracle.resolve(), args.candidate.resolve()
    oracle = [str(oracle_path), '--noprofile', '--norc']
    candidate = [str(candidate_path)] + args.candidate_arg
    cases = json.loads(args.cases.read_text())
    if args.case:
        unknown = set(args.case) - {case['name'] for case in cases}
        if unknown:
            parser.error('unknown cases: ' + ', '.join(sorted(unknown)))
        cases = [case for case in cases if case['name'] in args.case]
    if args.gnu_case:
        if not args.gnu_tests or not args.gnu_helpers:
            parser.error('--gnu-case requires --gnu-tests and --gnu-helpers')
        args.gnu_tests, args.gnu_helpers = args.gnu_tests.resolve(), args.gnu_helpers.resolve()
        for name in ('recho', 'zecho', 'printenv', 'xcase'):
            if not os.access(args.gnu_helpers / name, os.X_OK):
                parser.error('missing GNU test helper: ' + str(args.gnu_helpers / name))
        for driver in args.gnu_case:
            try:
                gnu_driver_path(args.gnu_tests, driver)
            except ValueError as error:
                parser.error(str(error))
            cases.append({'name': driver, 'category': 'gnu/' + driver[4:], 'gnu_driver': driver})
    gnu_source = gnu_identity(args.gnu_tests, args.gnu_helpers, args.gnu_case) if args.gnu_case else None
    with tempfile.TemporaryDirectory(prefix='rash-conformance-') as tmp:
        work = Path(tmp) / 'work'
        version = run([str(oracle_path), '--version'], Path(tmp), {'PATH': '/usr/bin:/bin'}, timeout=args.timeout)
        match = re.search(r'\bversion (\d+)\.(\d+)', version['stdout']['text'])
        oracle_version = tuple(map(int, match.groups())) if match else None
        results = []
        for case in cases:
            minimum = case.get('oracle_min_version')
            if minimum and (oracle_version is None or oracle_version < tuple(minimum)):
                case = dict(case, skip_reason='fixture requires a Bash oracle >= ' + '.'.join(map(str, minimum)))
            try:
                result = compare(case, oracle, candidate, work, args.timeout,
                                 gnu_tests=args.gnu_tests, gnu_helpers=args.gnu_helpers)
            except (OSError, ValueError) as error:
                result = {'name': case['name'], 'category': case['category'], 'status': 'error',
                          'reason': str(error)}
            results.append(result)
            print(result['status'].upper(), case['category'], case['name'], flush=True)
    categories = defaultdict(Counter)
    for result in results:
        categories[result['category']][result['status']] += 1
    report = {'oracle': identity(oracle_path), 'oracle_version': version,
              'candidate': identity(candidate_path), 'candidate_argv': candidate,
              'corpus': identity(args.cases.resolve()), 'gnu_tests': gnu_source,
              'categories': dict(categories), 'results': results,
              'limitations': ['Matches cover only the selected fixtures, not full language conformance.',
                              'No engine identity or absence of a hidden backend is inferred from matching output.',
                              'Process-group cleanup does not contain programs that deliberately create another session.',
                              'External GNU fixtures are trusted code; driver temporary-path checks are not a filesystem sandbox.',
                              'Interactive state, terminal ownership and Fish integration require the separate PTY suites.']}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=True, indent=2) + '\n')
    return int(any(result['status'] not in ('pass', 'skip') for result in results))


if __name__ == '__main__':
    raise SystemExit(main())
