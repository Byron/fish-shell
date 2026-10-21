#!/usr/bin/env python3
"""Test the comparison machinery with deliberately broken stand-in programs."""
from pathlib import Path
import sys
import tempfile
import unittest

from rash_conformance import compare, gnu_identity, run


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='rash-harness-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.case = {'name': 'fixture', 'category': 'harness', 'source': ':'}

    def program(self, source):
        return [sys.executable, '-c', source]

    def test_exact_match(self):
        program = self.program('from pathlib import Path; print("output"); Path("file").write_text("data")')
        result = compare(self.case, program, program, self.root / 'work')
        self.assertEqual(result['status'], 'pass')

    def test_wrong_candidates_fail(self):
        oracle = self.program('print("output")')
        for field, source in [
            ('stdout', 'print("wrong")'),
            ('stderr', 'import sys; print("output"); print("wrong", file=sys.stderr)'),
            ('returncode', 'print("output"); raise SystemExit(7)'),
            ('artifacts', 'from pathlib import Path; print("output"); Path("extra").write_text("wrong")'),
        ]:
            with self.subTest(field=field):
                result = compare(self.case, oracle, self.program(source), self.root / 'work')
                self.assertEqual(result['status'], 'fail')
                self.assertIn(field, result['different'])

    def test_timeout_and_output_limit(self):
        result = run(self.program('import time; time.sleep(30)'), self.root, {}, timeout=.05)
        self.assertEqual(result['outcome'], 'timeout')
        result = run(self.program('import sys\nwhile True: sys.stdout.write("x" * 65536)\n'),
                     self.root, {}, timeout=1, limit=65536)
        self.assertEqual(result['outcome'], 'output-limit')

    def test_oracle_failure_is_not_a_match(self):
        hanging = self.program('import time; time.sleep(30)')
        result = compare(self.case, hanging, hanging, self.root / 'work', timeout=.05)
        self.assertEqual(result['status'], 'oracle-fail')

    def test_gnu_driver_cannot_hide_diff_failure_with_exit_zero(self):
        suite = self.root / 'external-suite'
        suite.mkdir()
        (suite / 'run-fixture').write_text(
            '"$THIS_SH" unused > "$BASH_TSTOUT"\ndiff "$BASH_TSTOUT" expected\nexit 0\n')
        (suite / 'expected').write_text('expected\n')
        case = {'name': 'run-fixture', 'category': 'harness', 'gnu_driver': 'run-fixture'}
        result = compare(case, self.program('print("expected")'), self.program('print("wrong")'),
                         self.root / 'work', gnu_tests=suite, gnu_helpers=suite)
        self.assertEqual(result['candidate']['returncode'], 0)
        self.assertEqual(result['status'], 'fail')
        self.assertTrue(result['candidate']['emitted_diff'])
        self.assertEqual(result['candidate']['diffs'][0]['returncode'], 1)

    def test_legacy_fixed_temporary_path_is_rejected_before_execution(self):
        suite = self.root / 'external-suite'
        suite.mkdir()
        (suite / 'run-fixture').write_text('echo unsafe > /tmp/xx\n')
        case = {'name': 'run-fixture', 'category': 'harness', 'gnu_driver': 'run-fixture'}
        with self.assertRaisesRegex(ValueError, 'fixed absolute temporary path'):
            compare(case, [], [], self.root / 'work', gnu_tests=suite, gnu_helpers=suite)
        self.assertFalse((self.root / 'work').exists())

    def test_external_provenance_covers_recursive_fixtures(self):
        suite = self.root / 'external-suite'
        suite.mkdir()
        for name in ('run-fixture', 'fixture.tests', 'recho', 'zecho', 'printenv', 'xcase'):
            (suite / name).write_text('first')
        before = gnu_identity(suite, suite, ['run-fixture'])
        (suite / 'fixture.tests').write_text('changed')
        after = gnu_identity(suite, suite, ['run-fixture'])
        self.assertNotEqual(before['tree_sha256'], after['tree_sha256'])
        self.assertEqual(before['drivers'], after['drivers'])
        self.assertEqual(before['helpers'], after['helpers'])


if __name__ == '__main__':
    unittest.main()
