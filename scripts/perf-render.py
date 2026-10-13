#!/usr/bin/env python3
"""Serial reference-host measurements; no host scheduling or locking logic.

Use only on the designated measurement machine, with an idle host. Build the
render-profile feature first. Files and raw paths are never written to reports.
"""
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys

label, root = sys.argv[1:]
if not re.fullmatch(r"[A-Za-z0-9_-]+", label):
    raise SystemExit("label must contain only letters, digits, underscores or hyphens")
out = pathlib.Path('/tmp/rawsmith-perf-' + label)
out.mkdir(exist_ok=True)
results = []
for name in ('h43', 'h14'):
    raw = pathlib.Path(root) / (name + '.ARW')
    # The first three develop the photo every time. The reuse workloads are a
    # later render of the same photo in an editing session: its developed
    # scene is already stored, as after the first `auto` or preview.
    for workload in ('export', 'preview', 'auto', 'reuse-export', 'reuse-preview'):
        reuse = workload.startswith('reuse-')
        scenes = {'RAWSMITH_CACHE_DIR': str(out / 'scenes')} if reuse else {'RAWSMITH_CACHE_MB': '0'}
        env = {**{k: v for k, v in os.environ.items() if not k.startswith('RAWSMITH_CACHE_')}, 'RAWSMITH_PROFILE': '1', **scenes}
        if reuse:
            (out / 'prime.json').write_text('{}\n')
            subprocess.run(['./target/release/rawsmith', 'render', str(raw), '--recipe', str(out / 'prime.json'), '-o', str(out / 'prime.jpg'), '--size', '1'], env=env, capture_output=True, check=True)
        runs = []
        for repeat in range(3):
            tag = f'{name}-{workload}-{repeat}'
            (out / (tag + '.json')).write_text('{}\n')
            args = ['./target/release/rawsmith', 'auto' if workload == 'auto' else 'render', str(raw), '--recipe', str(out / (tag + '.json')), '-o', str(out / (tag + '.jpg'))]
            if not workload.endswith('export'):
                args += ['--size', '1400']
            result = subprocess.run(['/usr/bin/time', '-l'] + args, env=env, capture_output=True, text=True, check=True)
            (out / (tag + '.stdout')).write_text(result.stdout)
            (out / (tag + '.stderr')).write_text(result.stderr)
            stages = {}
            for line in result.stderr.splitlines():
                if line.startswith('profile\t'):
                    _, stage, ms, allocated, live, peak = line.split('\t')
                    entry = stages.setdefault(stage, [0., 0, 0, 0])
                    entry[0] += float(ms)
                    entry[1] += int(allocated)
                    entry[2] = max(entry[2], int(live))
                    entry[3] = max(entry[3], int(peak))
                elif ' real ' in line:
                    real = float(line.split()[0])
                elif 'maximum resident set size' in line:
                    rss = int(line.split()[0])
            runs.append(dict(real=real, rss=rss, stages=stages))
        row = dict(scene=name, workload=workload, seconds=statistics.median(r['real'] for r in runs), rss_bytes=statistics.median(r['rss'] for r in runs), stages={stage: [statistics.median(r['stages'][stage][i] for r in runs) for i in range(4)] for stage in runs[0]['stages']})
        results.append(row)
        print(json.dumps(row), flush=True)
(out / 'summary.json').write_text(json.dumps(results, indent=2))
