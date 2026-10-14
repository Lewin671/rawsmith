"""Selector behavior with fake local tools; no photos or cloud calls."""
import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'skill/rawsmith/scripts/select_candidate.py'
spec = importlib.util.spec_from_file_location('rawsmith_select', SCRIPT)
selector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(selector)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.raw = self.root / 'input.ARW'
        self.baseline = self.root / 'base.json'
        self.candidate = self.root / 'candidate.json'
        for path, data in [(self.raw, b'RAW'), (self.baseline, b'{ "exposure": 1 }\n'),
                           (self.candidate, b'{ "exposure": 2 }\n')]:
            path.write_bytes(data)
        self.original = {p: p.read_bytes() for p in (self.raw, self.baseline, self.candidate)}
        self.output = self.root / 'final.jpg'
        self.recipe = self.root / 'final.json'
        self.engine = self.root / 'rawsmith'
        self.engine.write_text('''#!/usr/bin/env python3
import pathlib, sys
args = sys.argv
recipe = pathlib.Path(args[args.index('--recipe') + 1]).read_bytes()
pathlib.Path(args[args.index('-o') + 1]).write_bytes(b'JPEG:' + recipe)
''')
        self.engine.chmod(0o755)
        self.codex = self.root / 'codex'
        self.codex.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
assert sys.argv[sys.argv.index('-m') + 1] == 'gpt-6.1-sol'
assert sys.argv[sys.argv.index('-s') + 1] == 'read-only'
assert '--ignore-user-config' in sys.argv and '--ephemeral' in sys.argv
assert sys.argv[sys.argv.index('-c') + 1] == 'model_reasoning_effort="low"'
assert [sys.argv[i + 1] for i, arg in enumerate(sys.argv) if arg == '--disable'] == ['hooks', 'apps', 'plugins', 'memories']
assert [pathlib.Path(sys.argv[i + 1]).name for i, arg in enumerate(sys.argv) if arg == '-i'] == ['image-A.jpg', 'image-B.jpg']
state = pathlib.Path(os.environ['FAKE_VOTES'])
queue = json.loads(state.read_text())
vote = queue.pop(0)
state.write_text(json.dumps(queue))
if vote == 'fail': sys.exit(3)
pathlib.Path(sys.argv[sys.argv.index('-o') + 1]).write_text(json.dumps(vote))
''')
        self.codex.chmod(0o755)
        self.queue = self.root / 'votes.json'
        self.env = patch.dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH'], FAKE_VOTES=str(self.queue))
        self.env.start()
        self.addCleanup(self.env.stop)

    def run_select(self, votes, extra=None):
        self.queue.write_text(json.dumps(votes))
        args = [str(self.raw), '--baseline', str(self.baseline), '--candidate', str(self.candidate),
                '--output', str(self.output), '--recipe', str(self.recipe), '--rawsmith', str(self.engine)]
        args += ['--allow-cloud'] if extra is None else extra
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = selector.main(args)
        for p, data in self.original.items():
            self.assertEqual(p.read_bytes(), data)
        return code, json.loads(out.getvalue()) if out.getvalue() else None

    @staticmethod
    def vote(winner, confidence=2):
        return {'winner': winner, 'confidence': confidence, 'reason': 'Comparison result.'}

    def passing(self):
        return [self.vote(x) for x in ['A', 'B', 'A', 'B', 'tie', 'tie', 'tie', 'tie', 'A', 'B', 'A', 'B', 'A', 'B', 'A', 'B']]

    def test_direct_script_entry_does_not_shadow_stdlib(self):
        result = subprocess.run([sys.executable, str(SCRIPT), '--help'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--allow-cloud', result.stdout)

    def test_candidate_requires_all_gates_and_preserves_bytes(self):
        code, report = self.run_select(self.passing())
        self.assertEqual(code, 0)
        self.assertEqual(report['choice'], 'candidate')
        self.assertEqual(self.recipe.read_bytes(), self.original[self.candidate])
        self.assertEqual(self.output.read_bytes(), b'JPEG:' + self.original[self.candidate])
        self.assertEqual(len(report['selection']), 2)
        self.assertEqual(len(report['acceptance']), 2)

    def test_selection_tie_disagreement_low_confidence_failure_invalid(self):
        for first in [self.vote('tie'), self.vote('B'), self.vote('A', 1), 'fail', {'winner': 'A'}]:
            with self.subTest(first=first):
                code, report = self.run_select([first, self.vote('B')])
                self.assertEqual(code, 0)
                self.assertEqual(report['choice'], 'baseline')
                self.assertEqual(report['acceptance'], [])
                self.assertEqual(self.recipe.read_bytes(), self.original[self.baseline])
                self.output.unlink()
                self.recipe.unlink()

    def test_acceptance_and_controls_can_veto(self):
        for index in range(2, 16):
            with self.subTest(index=index):
                votes = self.passing()
                votes[index] = self.vote('tie' if index in [2, 3] or index >= 8 else 'A')
                code, report = self.run_select(votes)
                self.assertEqual(code, 0)
                self.assertEqual(report['choice'], 'baseline')
                self.output.unlink()
                self.recipe.unlink()

    def test_no_cloud_permission_has_no_side_effects(self):
        with self.assertRaises(SystemExit):
            self.run_select([], [])
        self.assertFalse(self.output.exists())
        self.assertFalse(self.recipe.exists())
        self.assertEqual(self.queue.read_text(), '[]')

    def test_existing_outputs_and_hardlinked_inputs_rejected(self):
        for target in [self.output, self.recipe]:
            target.write_bytes(b'keep')
            code, _ = self.run_select([])
            self.assertEqual(code, 1)
            self.assertEqual(target.read_bytes(), b'keep')
            target.unlink()
        os.link(self.raw, self.output)
        code, _ = self.run_select([])
        self.assertEqual(code, 1)
        self.assertEqual(self.raw.read_bytes(), b'RAW')

    def test_same_destinations_rejected(self):
        self.recipe = self.output
        code, _ = self.run_select([])
        self.assertEqual(code, 1)
        self.assertFalse(self.output.exists())

    def test_failed_final_render_writes_nothing(self):
        actual = selector.render
        def render(*args, **kwargs):
            if not kwargs.get('preview'):
                raise ValueError('export failed')
            actual(*args, **kwargs)
        with patch.object(selector, 'render', render):
            code, _ = self.run_select([self.vote('tie'), self.vote('tie')])
        self.assertEqual(code, 1)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.recipe.exists())

    def test_non_jpeg_extension_rejected_before_judging(self):
        self.output = self.root / 'final.png'
        votes = self.passing()
        code, _ = self.run_select(votes)
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(self.queue.read_text()), votes)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.recipe.exists())

    def test_baseline_preview_failure_is_fatal(self):
        with patch.object(selector, 'render', side_effect=ValueError('baseline invalid')):
            code, report = self.run_select([])
        self.assertEqual(code, 1)
        self.assertIsNone(report)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.recipe.exists())

    def test_candidate_preview_failure_delivers_baseline(self):
        actual = selector.render
        def render(engine, raw, recipe, output, **kwargs):
            if recipe.name == 'candidate.json':
                raise ValueError('invalid candidate recipe')
            actual(engine, raw, recipe, output, **kwargs)
        with patch.object(selector, 'render', render):
            code, report = self.run_select([])
        self.assertEqual(code, 0)
        self.assertEqual(report['choice'], 'baseline')
        self.assertIn('candidate_render_error', report)
        self.assertEqual(self.recipe.read_bytes(), self.original[self.baseline])

    def test_control_render_failure_delivers_baseline(self):
        actual = selector.render
        def render(*args, **kwargs):
            if kwargs.get('settings') == selector.CONTROLS['flat']:
                raise ValueError('bad control render')
            actual(*args, **kwargs)
        with patch.object(selector, 'render', render):
            code, report = self.run_select(self.passing()[:-2])
        self.assertEqual(code, 0)
        self.assertEqual(report['choice'], 'baseline')
        self.assertFalse(report['controls_passed'])
        self.assertIn('render_error', report['controls']['flat'])
        self.assertEqual(self.recipe.read_bytes(), self.original[self.baseline])

    def test_second_publish_failure_rolls_back_first(self):
        actual = os.link
        def link(source, target):
            if target == self.recipe:
                raise OSError('simulated disk error')
            actual(source, target)
        with patch.object(selector.os, 'link', link):
            code, _ = self.run_select([self.vote('tie'), self.vote('tie')])
        self.assertEqual(code, 1)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.recipe.exists())


if __name__ == '__main__':
    unittest.main()
