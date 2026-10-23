#!/usr/bin/env python3
"""Exercise the read-only storage reporter's CLI and metadata boundaries."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(sys.argv.pop(1)).resolve()
LIVE = '/nix/store/' + '0' * 32 + '-live'
DEAD = '/nix/store/' + '1' * 32 + '-dead'


class StorageReport(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        tools = self.root / 'tools'
        tools.mkdir()
        program = f'#!{sys.executable}\n' + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as out:
    out.write(json.dumps(args) + '\\n')
if 'config' in args:
    print(json.dumps({'keep-failed': {'value': True}, 'build-dir': {'value': os.environ['BUILD_BASE']}}))
elif 'path-info' in args:
    print(os.environ['METADATA'])
elif args == ['--gc', '--print-dead']:
    print(os.environ['DEAD_PATH'])
elif args == ['--gc', '--print-roots']:
    print('operator-root -> ' + os.environ['LIVE_PATH'])
else:
    raise SystemExit('unexpected or destructive Nix operation: ' + repr(args))
'''
        for name in ['nix', 'nix-store']:
            path = tools / name
            path.write_text(program)
            path.chmod(0o755)
        self.build_base = self.root / 'builds'
        self.failed_build = self.build_base / 'nix-build-example.drv-0'
        self.failed_build.mkdir(parents=True)
        (self.failed_build / 'diagnostic').write_text('retained')
        self.calls = self.root / 'calls'
        self.env = os.environ | {
            'PATH': str(tools), 'CALLS': str(self.calls),
            'METADATA': json.dumps({LIVE: {'narSize': 2 * 1024 ** 3}, DEAD: {'narSize': 1024 ** 3}}),
            'DEAD_PATH': DEAD, 'LIVE_PATH': LIVE, 'BUILD_BASE': str(self.build_base),
        }

    def run_report(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=self.repo,
                              env=self.env, capture_output=True, text=True)

    def test_default_reads_database_without_gc_or_tree_walk(self):
        result = self.run_report()
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report['store_NAR_GiB'], 3)
        self.assertEqual(report['largest_store_paths'][0]['path'], LIVE)
        self.assertNotIn('dead_paths', report)
        self.assertIn('NOT physical', report['sizes'])
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        self.assertIn('config', calls[0])
        self.assertIn('--all', calls[1])
        self.assertNotIn('--recursive', calls[1])
        self.assertTrue(report['keep_failed_builds'])
        self.assertIn(str(self.failed_build), report['temporary_build_directories'])
        self.assertEqual((self.failed_build / 'diagnostic').read_text(), 'retained')
        self.assertFalse((self.repo / 'target').exists())

    def test_dead_mode_only_requests_native_read_only_gc_queries(self):
        result = self.run_report('--dead')
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report['dead_NAR_GiB'], 1)
        self.assertEqual(report['dead_paths_missing_from_metadata'], [])
        self.assertEqual(report['largest_dead_paths'][0]['path'], DEAD)
        self.assertTrue((Path(report['logs']) / 'gc-roots').is_file())
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls[2:], [['--gc', '--print-dead'], ['--gc', '--print-roots']])

    def test_malformed_sizes_fail_instead_of_reporting_zero(self):
        for value in [True, -1, 'large', None]:
            with self.subTest(value=value):
                self.env['METADATA'] = json.dumps({LIVE: {'narSize': value}})
                result = self.run_report()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('no zero-size fallback', result.stderr)

    def test_unknown_dead_paths_are_not_reported_as_complete_reclaimable_size(self):
        self.env['DEAD_PATH'] = '/nix/store/' + '2' * 32 + '-changed-during-audit'
        result = self.run_report('--dead')
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(len(report['dead_paths_missing_from_metadata']), 1)
        self.assertIsNone(report['dead_NAR_GiB'])


if __name__ == '__main__':
    unittest.main()
