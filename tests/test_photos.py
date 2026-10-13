"""Photo benchmark safety and provenance checks; synthetic files and judges only."""

import contextlib
import importlib.util
import io
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "photos", Path(__file__).resolve().parents[1] / "bench" / "photos.py"
)
photos = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(photos)


class WorkDirectoryTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.work = self.base / "work"

    def marked(self):
        photos.work_dir(str(self.work))
        return self.work / photos.WORK_MARKER

    def test_empty_directory_is_marked_and_can_be_reused(self):
        path = photos.work_dir(str(self.work))
        self.assertEqual(path, os.path.realpath(self.work))
        self.assertEqual(photos.load(str(self.work / photos.WORK_MARKER)), photos.WORK_FORMAT)
        (self.work / "images").mkdir()
        (self.work / "images" / "result.jpg").write_bytes(b"synthetic result")
        self.assertEqual(photos.work_dir(path), path)

    def test_nonempty_unmarked_directory_is_left_intact(self):
        self.work.mkdir()
        for name in ("images", "blind"):
            folder = self.work / name
            folder.mkdir()
            (folder / "keep.txt").write_text("unrelated files")
        (self.work / "verdicts-private.json").write_text("unrelated verdicts")
        with self.assertRaises(SystemExit):
            photos.work_dir(str(self.work))
        self.assertFalse((self.work / photos.WORK_MARKER).exists())
        self.assertEqual((self.work / "images" / "keep.txt").read_text(), "unrelated files")
        self.assertEqual((self.work / "blind" / "keep.txt").read_text(), "unrelated files")
        self.assertEqual((self.work / "verdicts-private.json").read_text(), "unrelated verdicts")

    def test_every_command_rejects_unowned_work_before_processing_it(self):
        self.work.mkdir()
        images = self.work / "images"
        images.mkdir()
        (images / "keep.txt").write_text("unrelated files")
        for command in (photos.cmd_render, photos.cmd_pairs, photos.cmd_judge, photos.cmd_report):
            with self.subTest(command=command.__name__):
                with self.assertRaises(SystemExit):
                    command(SimpleNamespace(work=str(self.work)))
                self.assertEqual((images / "keep.txt").read_text(), "unrelated files")

    def test_invalid_markers_are_not_adopted(self):
        marker = self.marked()
        invalid = [
            "not JSON", "null", "[]", "{}",
            json.dumps({"format": "rawsmith-photo-benchmark", "version": 2}),
            json.dumps({"format": "rawsmith-photo-benchmark", "version": True}),
            json.dumps({**photos.WORK_FORMAT, "extra": "unrecognised"}),
        ]
        for contents in invalid:
            with self.subTest(contents=contents):
                marker.write_text(contents)
                with self.assertRaises(SystemExit):
                    photos.work_dir(str(self.work))
                self.assertEqual(marker.read_text(), contents)
        marker.unlink()
        marker.mkdir()
        with self.assertRaises(SystemExit):
            photos.work_dir(str(self.work))

    @unittest.skipUnless(hasattr(os, "symlink"), "symbolic links unavailable")
    def test_marker_and_work_directory_links_are_rejected(self):
        marker = self.marked()
        outside = self.base / "owner.json"
        outside.write_text(json.dumps(photos.WORK_FORMAT))
        marker.unlink()
        marker.symlink_to(outside)
        with self.assertRaises(SystemExit):
            photos.work_dir(str(self.work))
        self.assertEqual(json.loads(outside.read_text()), photos.WORK_FORMAT)
        link = self.base / "work-link"
        link.symlink_to(self.work, target_is_directory=True)
        with self.assertRaises(SystemExit):
            photos.work_dir(str(link))
        with self.assertRaises(SystemExit):
            photos.work_dir(str(link) + os.sep)

    @unittest.skipUnless(hasattr(os, "symlink"), "symbolic links unavailable")
    def test_work_files_and_nested_links_cannot_redirect_operations(self):
        self.marked()
        outside = self.base / "outside.txt"
        outside.write_text("must survive")
        for name in ("scratch.json", "scratch.json.tmp", "manifest.json", "tasks.json", "verdicts-claude.json"):
            with self.subTest(name=name):
                link = self.work / name
                link.symlink_to(outside)
                with self.assertRaises(SystemExit):
                    photos.work_dir(str(self.work))
                link.unlink()
        images = self.work / "images"
        images.mkdir()
        (images / "p000-auto.jpg").symlink_to(outside)
        with self.assertRaises(SystemExit):
            photos.work_dir(str(self.work))
        self.assertEqual(outside.read_text(), "must survive")

    @unittest.skipUnless(hasattr(os, "symlink"), "symbolic links unavailable")
    def test_work_subdirectory_links_are_rejected(self):
        self.marked()
        outside = self.base / "outside"
        outside.mkdir()
        (outside / "keep.txt").write_text("must survive")
        for name in ("images", "blind"):
            with self.subTest(name=name):
                link = self.work / name
                link.symlink_to(outside, target_is_directory=True)
                with self.assertRaises(SystemExit):
                    photos.work_dir(str(self.work))
                link.unlink()
        self.assertEqual((outside / "keep.txt").read_text(), "must survive")

    def test_repository_work_directory_is_rejected(self):
        with self.assertRaises(SystemExit):
            photos.work_dir(photos.REPO)

    @unittest.skipUnless(hasattr(os, "symlink"), "symbolic links unavailable")
    def test_json_save_ignores_old_staging_link_and_leaves_no_temporary_files(self):
        outside = self.base / "outside.txt"
        outside.write_text("must survive")
        report = self.base / "report.json"
        (self.base / "report.json.tmp").symlink_to(outside)
        photos.save(str(report), {"photos": 1})
        self.assertEqual(photos.load(str(report)), {"photos": 1})
        self.assertEqual(outside.read_text(), "must survive")
        self.assertFalse(any(p.name.startswith(".rawsmith-") for p in self.base.iterdir()))


class CameraRenderTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.work = self.base / "work"
        self.raws = self.base / "raws"
        self.raws.mkdir()
        self.calls = self.base / "calls.jsonl"
        self.binary = self.base / "fake-rawsmith"
        self.binary.write_text("#!" + sys.executable + "\n" + f"CALLS = {str(self.calls)!r}\n" + r'''
import json, sys
from pathlib import Path
args = sys.argv[1:]
with open(CALLS, "a") as f:
    f.write(json.dumps(args) + "\n")
if args == ["--version"]:
    print("rawsmith fake-version")
    sys.exit(0)
if args[0] not in ("render", "auto", "compare"):
    sys.exit("unexpected command (extract-jpeg/inspect must not be used)")
output = Path(args[args.index("-o") + 1])
output.write_bytes(b"synthetic image")
if args[0] == "compare":
    assert args[2:6] == ["--panel", "camera", "--size", "1400"]
    space = Path(args[1]).read_text()
    if space == "failure":
        sys.exit("synthetic camera decode failure")
    converted = "Adobe RGB (1998) to sRGB" if space == "Adobe RGB (1998)" else None
    final = "sRGB" if converted else space
    print(json.dumps({"output_color_space": "sRGB" if final == "sRGB" else "mixed-unmanaged",
                      "panels": [{"color_space": final, "color_conversion": converted,
                                  "color_declaration": {"source": "raw-exif", "color_space": space}}]}))
else:
    if args[0] == "auto":
        Path(args[args.index("--recipe") + 1]).write_text("{}")
    print(json.dumps({"stats": {"clipped_highlights_pct": 0}, "recipe": {}}))
''')
        self.binary.chmod(0o755)

    def render(self, spaces, sips=None):
        for index, space in enumerate(spaces):
            (self.raws / f"private-photo-{index}.ARW").write_text(space)
        output = io.StringIO()
        with mock.patch.object(photos.shutil, "which", return_value=sips):
            with contextlib.redirect_stdout(output):
                photos.cmd_render(SimpleNamespace(work=str(self.work), raws=str(self.raws),
                                                 recipes=None, rawsmith=str(self.binary)))
        return photos.load(str(self.work / "manifest.json")), output.getvalue()

    def test_camera_without_macos_skips_unmanaged_and_failed_outputs(self):
        manifest, output = self.render(["sRGB", "Adobe RGB (1998)", "unknown", "uncalibrated",
                                       "ICC (unverified)", "failure"])
        counts = {"srgb": 1, "adobe_rgb_converted": 1, "skipped": 4}
        self.assertEqual(manifest["camera_color_space"], counts)
        for index, entry in enumerate(manifest["photos"].values()):
            self.assertEqual("camera" in entry["variants"], index < 2)
            self.assertEqual((self.work / "images" / f"p{index:03d}-camera.jpg").exists(), index < 2)
            self.assertNotIn("system", entry["variants"])
        self.assertEqual(output.count("camera skipped:"), 4)
        photos.cmd_pairs(SimpleNamespace(work=str(self.work), seed=1))
        tasks = photos.load(str(self.work / "tasks.json"))
        self.assertTrue(all(t["photo"] in ("p000", "p001") for t in tasks if "camera" in (t["A"], t["B"])))
        report_path = self.work / "report.json"
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            photos.cmd_report(SimpleNamespace(work=str(self.work), json=str(report_path)))
        report = photos.load(str(report_path))
        self.assertEqual(report["camera_color_space"], counts)
        self.assertIn("1 sRGB, 1 converted from Adobe RGB, 4 skipped", output.getvalue())
        self.assertNotIn("private-photo", json.dumps(report))
        self.assertNotIn(str(self.base), json.dumps(report))

    def test_sips_is_only_used_for_system(self):
        sips = self.base / "fake-sips"
        sips_calls = self.base / "sips.jsonl"
        sips.write_text("#!" + sys.executable + "\nimport json,sys\nfrom pathlib import Path\n"
                        + f"with open({str(sips_calls)!r}, 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                        + "Path(sys.argv[-1]).write_bytes(b'synthetic system')\n")
        sips.chmod(0o755)
        manifest, _ = self.render(["Adobe RGB (1998)"], str(sips))
        calls = [json.loads(line) for line in sips_calls.read_text().splitlines()]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][-3], str(self.raws / "private-photo-0.ARW"))
        self.assertIn("system", manifest["photos"]["p000"]["variants"])
        self.assertIn("camera", manifest["photos"]["p000"]["variants"])


class JudgeTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.work = self.base / "work"
        photos.work_dir(str(self.work))
        (self.work / "blind").mkdir()
        self.tasks = []
        for index in range(4):
            task = {"task": f"t{index:04d}", "photo": f"p{index // 2:03d}", "kind": "comparison",
                    "A": "auto" if index % 2 == 0 else "neutral",
                    "B": "neutral" if index % 2 == 0 else "auto"}
            self.tasks.append(task)
            for side in "AB":
                (self.work / "blind" / f"{task['task']}-{side}.jpg").write_bytes(f"synthetic {index} {side}".encode())
        photos.save(str(self.work / "tasks.json"), self.tasks)
        self.manifest = {"edge": photos.EDGE, "task_seed": 7,
                         "rendering": {"rawsmith_version": "rawsmith 0.1.2", "repository_commit": "a" * 40},
                         "photos": {f"p{i:03d}": {"variants": {}, "seconds": {}} for i in range(2)}}
        photos.save(str(self.work / "manifest.json"), self.manifest)
        self.calls = self.base / "calls.jsonl"
        script = self.base / "private-owner-judge.py"
        script.write_text("import json,sys\n"
                          "prompt=sys.stdin.read()\n"
                          "with open(sys.argv[1], 'a') as f: f.write(json.dumps(prompt)+'\\n')\n"
                          "print(json.dumps({'winner':'A','confidence':2,'reason':'synthetic'}))\n")
        self.command = " ".join(shlex.quote(str(p)) for p in (sys.executable, script, self.calls))
        self.args = SimpleNamespace(work=str(self.work), name="fake", judge_cmd=self.command, model="fake-model", jobs=1)
        self.verdict_file = self.work / "verdicts-fake.json"

    def judge(self):
        with contextlib.redirect_stdout(io.StringIO()):
            photos.cmd_judge(self.args)
        return photos.load(str(self.verdict_file))

    def call_count(self):
        return len(self.calls.read_text().splitlines()) if self.calls.exists() else 0

    def report(self):
        output = io.StringIO()
        path = self.work / "report.json"
        with contextlib.redirect_stdout(output):
            photos.cmd_report(SimpleNamespace(work=str(self.work), json=str(path)))
        return photos.load(str(path)), output.getvalue()

    def test_unchanged_resume_reuses_all_work_and_records_provenance(self):
        stored = self.judge()
        self.assertEqual(self.call_count(), 4)
        self.assertEqual(self.judge(), stored)
        self.assertEqual(self.call_count(), 4)
        config = stored["configuration"]
        self.assertEqual(config["judge_command"], self.command)
        self.assertEqual(config["model"], "fake-model")
        self.assertEqual(config["prompt_sha256"], photos.text_digest(photos.PROMPT))
        self.assertEqual(config["harness"], photos.JUDGE_FORMAT)
        self.assertEqual(stored["task_set"]["seed"], 7)
        self.assertEqual(stored["task_set"]["tasks"], 4)
        self.assertEqual(len(stored["task_set"]["blind_images_sha256"]), 64)
        self.assertIsNotNone(stored["runs"][0]["finished_at"])
        for verdict in stored["verdicts"].values():
            self.assertEqual(len(verdict["fingerprint"]), 64)
            self.assertRegex(verdict["judged_at"], r"^\d{4}-\d\d-\d\dT")
        report, _ = self.report()
        self.assertEqual(report["rendering"], self.manifest["rendering"])
        self.assertEqual(report["task_set"], stored["task_set"])
        summary = report["judges"]["fake"]
        self.assertEqual(summary["verdict_validation"]["valid"], 4)
        self.assertEqual(summary["judge_checks"]["pairs_judged"], 2)
        public = summary["evaluation"]
        self.assertEqual(public["judge_command"]["sha256"], photos.text_digest(self.command))
        self.assertEqual(public["model"], "fake-model")
        self.assertEqual(public["prompt_sha256"], config["prompt_sha256"])
        self.assertEqual(public["harness"], photos.JUDGE_FORMAT)
        self.assertEqual(public["runs"], stored["runs"])
        for private in (str(self.base), "private-owner", "t0000", "p000", ".jpg", "synthetic"):
            self.assertNotIn(private, json.dumps(report))

    def test_command_model_and_prompt_changes_refuse_without_mutating_file(self):
        self.judge()
        original = self.verdict_file.read_bytes()
        for field, value in (("judge_cmd", self.command + " changed"), ("model", "other-model")):
            old = getattr(self.args, field)
            with self.subTest(field=field):
                setattr(self.args, field, value)
                with self.assertRaisesRegex(SystemExit, "changed.*new --name.*left unchanged"):
                    self.judge()
                self.assertEqual(self.verdict_file.read_bytes(), original)
                setattr(self.args, field, old)
        with mock.patch.object(photos, "PROMPT", photos.PROMPT + " Changed prompt."):
            with self.assertRaisesRegex(SystemExit, "changed.*new --name"):
                self.judge()
            report, _ = self.report()
            self.assertEqual(report["judges"]["fake"]["verdict_validation"]["valid"], 0)
        self.assertEqual(self.verdict_file.read_bytes(), original)
        self.assertEqual(self.call_count(), 4)
        self.args.name = "other"
        self.args.model = "other-model"
        self.judge()
        self.assertTrue((self.work / "verdicts-other.json").exists())
        self.assertEqual(self.verdict_file.read_bytes(), original)

    def test_replacing_image_invalidates_exactly_one_verdict_and_report_pair(self):
        before = self.judge()
        old_identity = before["task_set"]
        image = self.work / "blind" / "t0000-A.jpg"
        replacement = image.with_suffix(".new")
        replacement.write_bytes(b"replaced image bytes")
        replacement.replace(image)
        report, _ = self.report()
        summary = report["judges"]["fake"]
        self.assertEqual(summary["verdict_validation"]["valid"], 3)
        self.assertEqual(summary["verdict_validation"]["stale"], 1)
        self.assertEqual(summary["judge_checks"]["pairs_judged"], 1)
        self.assertNotEqual(report["task_set"]["blind_images_sha256"], old_identity["blind_images_sha256"])
        after = self.judge()
        self.assertEqual(self.call_count(), 5)
        for key in ("t0001", "t0002", "t0003"):
            self.assertEqual(after["verdicts"][key], before["verdicts"][key])
        self.assertNotEqual(after["verdicts"]["t0000"]["fingerprint"], before["verdicts"]["t0000"]["fingerprint"])

    def test_task_assignment_and_exact_prompt_are_fingerprinted(self):
        before = self.judge()
        self.tasks[0]["A"], self.tasks[0]["B"] = self.tasks[0]["B"], self.tasks[0]["A"]
        photos.save(str(self.work / "tasks.json"), self.tasks)
        report, _ = self.report()
        self.assertEqual(report["judges"]["fake"]["verdict_validation"]["stale"], 1)
        after = self.judge()
        self.assertEqual(self.call_count(), 5)
        self.assertEqual(after["verdicts"]["t0001"], before["verdicts"]["t0001"])
        # Moving a work directory changes literal image paths in the prompt.
        moved = self.base / "moved-work"
        self.work.rename(moved)
        self.work = moved
        self.args.work = str(moved)
        self.verdict_file = moved / "verdicts-fake.json"
        report, _ = self.report()
        self.assertEqual(report["judges"]["fake"]["verdict_validation"]["stale"], 4)
        self.judge()
        self.assertEqual(self.call_count(), 9)

    def test_regenerating_pairs_retains_only_matching_verdicts(self):
        (self.work / "images").mkdir()
        for photo in self.manifest["photos"]:
            self.manifest["photos"][photo]["variants"] = {"auto": None, "neutral": None}
            for variant in ("auto", "neutral"):
                (self.work / "images" / f"{photo}-{variant}.jpg").write_bytes(f"{photo} {variant}".encode())
        photos.save(str(self.work / "manifest.json"), self.manifest)
        def pairs(seed):
            with contextlib.redirect_stdout(io.StringIO()):
                photos.cmd_pairs(SimpleNamespace(work=str(self.work), seed=seed))
        pairs(1)
        self.judge()
        original = self.verdict_file.read_bytes()
        count = self.call_count()
        pairs(2)
        self.assertEqual(self.verdict_file.read_bytes(), original)
        self.judge()
        self.assertEqual(self.call_count(), count)
        report, _ = self.report()
        self.assertEqual(report["task_set"]["seed"], 2)
        (self.work / "images" / "p001-neutral.jpg").write_bytes(b"new rendering")
        pairs(2)
        report, _ = self.report()
        self.assertEqual(report["judges"]["fake"]["verdict_validation"]["stale"], 2)
        self.judge()
        self.assertEqual(self.call_count(), count + 2)

    def test_interrupted_run_resumes_saved_valid_work(self):
        real_run = subprocess.run
        attempts = []
        def interrupted(*args, **kwargs):
            attempts.append(1)
            if len(attempts) > 1:
                raise KeyboardInterrupt()
            return real_run(*args, **kwargs)
        with mock.patch.object(photos.subprocess, "run", side_effect=interrupted):
            with self.assertRaises(KeyboardInterrupt):
                self.judge()
        partial = photos.load(str(self.verdict_file))
        self.assertEqual(len(partial["verdicts"]), 1)
        self.assertIsNone(partial["runs"][0]["finished_at"])
        first = partial["verdicts"]["t0000"]
        resumed = self.judge()
        self.assertEqual(self.call_count(), 4)
        self.assertEqual(resumed["verdicts"]["t0000"], first)
        self.assertEqual(len(resumed["runs"]), 2)

    def test_parallel_results_are_saved_before_a_slow_earlier_task(self):
        self.args.jobs = 2
        saved = threading.Event()
        real_save = photos.save
        def run(command, **kwargs):
            if "t0000-A.jpg" in kwargs["input"]:
                if not saved.wait(5):
                    raise RuntimeError("completed tasks were blocked behind a slow task")
            return subprocess.CompletedProcess(command, 0, '{"winner":"tie"}', '')
        def save(path, value):
            real_save(path, value)
            if value.get("verdicts", {}).get("t0001", {}).get("fingerprint"):
                saved.set()
        with mock.patch.object(photos.subprocess, "run", side_effect=run):
            with mock.patch.object(photos, "save", side_effect=save):
                stored = self.judge()
        self.assertEqual(len(stored["verdicts"]), 4)
        self.assertTrue(saved.is_set())

    def test_launch_errors_are_saved_as_failures_and_resume_retries_them(self):
        with mock.patch.object(photos.subprocess, "run", side_effect=OSError("synthetic launch failure")):
            with self.assertRaisesRegex(SystemExit, "some tasks failed"):
                self.judge()
        stored = photos.load(str(self.verdict_file))
        for verdict in stored["verdicts"].values():
            self.assertIsNone(verdict["winner"])
            self.assertIn("synthetic launch failure", verdict["error"])
        self.judge()
        self.assertEqual(self.call_count(), 4)

    def test_images_changed_or_removed_during_judging_are_not_evidence(self):
        real_run = subprocess.run
        calls = []
        def changed(*args, **kwargs):
            calls.append(1)
            reply = real_run(*args, **kwargs)
            if len(calls) == 1:
                (self.work / "blind" / "t0000-A.jpg").write_bytes(b"changed while judging")
                (self.work / "blind" / "t0001-A.jpg").unlink()
            return reply
        with mock.patch.object(photos.subprocess, "run", side_effect=changed):
            with self.assertRaisesRegex(SystemExit, "some tasks failed"):
                self.judge()
        report, _ = self.report()
        summary = report["judges"]["fake"]
        self.assertEqual(summary["verdict_validation"]["valid"], 2)
        self.assertEqual(summary["judge_checks"]["pairs_judged"], 1)

    def test_legacy_verdicts_are_reported_excluded_and_never_overwritten(self):
        legacy = {task["task"]: {"winner": "A"} for task in self.tasks}
        photos.save(str(self.verdict_file), legacy)
        original = self.verdict_file.read_bytes()
        with self.assertRaisesRegex(SystemExit, "legacy or unverifiable.*new --name"):
            self.judge()
        self.assertEqual(self.call_count(), 0)
        self.assertEqual(self.verdict_file.read_bytes(), original)
        report, text = self.report()
        summary = report["judges"]["fake"]
        self.assertEqual(summary["comparisons"], {})
        self.assertEqual(summary["verdict_validation"]["unverifiable"], 4)
        self.assertEqual(summary["judge_checks"]["pairs_judged"], 0)
        self.assertIsNone(summary["evaluation"])
        self.assertIn("unverifiable", text)

    def test_mixed_file_keeps_verified_entries_and_rejudges_unverifiable_ones(self):
        stored = self.judge()
        del stored["verdicts"]["t0000"]["fingerprint"]
        photos.save(str(self.verdict_file), stored)
        report, _ = self.report()
        counts = report["judges"]["fake"]["verdict_validation"]
        self.assertEqual(counts["valid"], 3)
        self.assertEqual(counts["unverifiable"], 1)
        after = self.judge()
        self.assertEqual(self.call_count(), 5)
        self.assertEqual(after["verdicts"]["t0001"], stored["verdicts"]["t0001"])

    def test_unreadable_cache_is_reported_and_left_unchanged(self):
        for contents in ("not JSON", "null", "[]"):
            with self.subTest(contents=contents):
                self.verdict_file.write_text(contents)
                with self.assertRaisesRegex(SystemExit, "unverifiable"):
                    self.judge()
                self.assertEqual(self.verdict_file.read_text(), contents)
                report, _ = self.report()
                self.assertEqual(report["judges"]["fake"]["verdict_validation"]["valid"], 0)
                self.assertEqual(report["judges"]["fake"]["verdict_validation"]["unverifiable"], 1)

    def test_failed_exit_even_with_json_is_not_evidence_and_is_retried(self):
        real_run = subprocess.run
        def failed(*args, **kwargs):
            reply = real_run(*args, **kwargs)
            reply.returncode = 9
            return reply
        with mock.patch.object(photos.subprocess, "run", side_effect=failed):
            with self.assertRaisesRegex(SystemExit, "some tasks failed"):
                self.judge()
        self.assertEqual(self.call_count(), 12)
        report, _ = self.report()
        self.assertEqual(report["judges"]["fake"]["verdict_validation"]["failed"], 4)
        self.assertEqual(report["judges"]["fake"]["judge_checks"]["pairs_judged"], 0)
        self.judge()
        self.assertEqual(self.call_count(), 16)

    def test_missing_images_and_removed_tasks_do_not_count_as_results(self):
        self.judge()
        (self.work / "blind" / "t0000-A.jpg").unlink()
        photos.save(str(self.work / "tasks.json"), self.tasks[:2])
        report, _ = self.report()
        summary = report["judges"]["fake"]
        self.assertEqual(summary["verdict_validation"]["valid"], 1)
        self.assertEqual(summary["verdict_validation"]["orphaned"], 2)
        self.assertEqual(summary["verdict_validation"]["stale"], 1)
        self.assertEqual(summary["judge_checks"]["pairs_judged"], 0)
        original = self.verdict_file.read_bytes()
        with self.assertRaisesRegex(SystemExit, "cannot fingerprint"):
            self.judge()
        self.assertEqual(self.verdict_file.read_bytes(), original)

    def test_custom_judges_require_an_explicit_model_and_safe_label(self):
        self.args.model = None
        with self.assertRaisesRegex(SystemExit, "requires --model"):
            self.judge()
        self.args.model = "fake-model"
        self.args.name = "../outside"
        with self.assertRaisesRegex(SystemExit, "--name"):
            self.judge()
        self.assertEqual(self.call_count(), 0)

    def test_default_adapter_model_is_explicit_and_environment_cannot_change_it(self):
        self.args.judge_cmd = photos.DEFAULT_JUDGE
        self.args.model = None
        reply = subprocess.CompletedProcess([], 0, '{"winner":"tie"}', '')
        with mock.patch.dict(os.environ, {"JUDGE_MODEL": "hidden-model"}):
            with mock.patch.object(photos.subprocess, "run", return_value=reply) as run:
                stored = self.judge()
        self.assertEqual(stored["configuration"]["model"], "gpt-6-luna")
        self.assertIn("--model gpt-6-luna", run.call_args.args[0])
        self.assertEqual(len(stored["configuration"]["adapter_sha256"]), 64)
        real_digest = photos.file_digest
        def changed_adapter(path):
            return "changed-adapter" if path.endswith("judge-codex.py") else real_digest(path)
        with mock.patch.object(photos, "file_digest", side_effect=changed_adapter):
            with self.assertRaisesRegex(SystemExit, "configuration changed"):
                self.judge()
            report, _ = self.report()
        self.assertEqual(report["judges"]["fake"]["verdict_validation"]["valid"], 0)

    def test_custom_shell_commands_and_claude_wrapped_replies_still_work(self):
        self.args.judge_cmd = self.command + " | cat"
        reply = subprocess.CompletedProcess([], 0, '{"result":"{\\"winner\\":\\"B\\"}"}', '')
        with mock.patch.object(photos.subprocess, "run", return_value=reply) as run:
            stored = self.judge()
        self.assertTrue(run.call_args.kwargs["shell"])
        self.assertEqual(run.call_args.args[0], self.args.judge_cmd)
        self.assertTrue(all(v["winner"] == "B" for v in stored["verdicts"].values()))

    def test_older_manifest_provenance_is_explicitly_unknown(self):
        del self.manifest["rendering"]
        del self.manifest["task_seed"]
        photos.save(str(self.work / "manifest.json"), self.manifest)
        self.judge()
        report, _ = self.report()
        self.assertEqual(report["rendering"], {"rawsmith_version": None, "repository_commit": None})
        self.assertIsNone(report["task_set"]["seed"])
        self.assertIsNone(report["camera_color_space"])

    def test_cli_defaults_to_codex_and_help_describes_resume(self):
        with mock.patch.object(sys, "argv", ["photos.py", "judge", "--work", str(self.work)]):
            with mock.patch.object(photos, "cmd_judge") as judge:
                photos.main()
        args = judge.call_args.args[0]
        self.assertEqual(args.name, "codex")
        self.assertEqual(args.judge_cmd, photos.DEFAULT_JUDGE)
        help_text = subprocess.run([sys.executable, str(Path(photos.REPO) / "bench" / "photos.py"), "judge", "--help"],
                                   capture_output=True, text=True, check=True).stdout
        self.assertIn("legacy verdict", help_text)
        self.assertIn("gpt-6-luna", help_text)

    def test_render_manifest_and_report_record_binary_version_and_commit(self):
        raws = self.base / "synthetic-raws"
        raws.mkdir()
        (raws / "private-name.ARW").write_bytes(b"synthetic RAW stand-in")
        binary = self.base / "fake-rawsmith"
        binary.write_text("not executed")
        def run(command, **kwargs):
            if command[-1] == "--version":
                return subprocess.CompletedProcess(command, 0, "rawsmith fake-version\n", "")
            return subprocess.CompletedProcess(command, 0, "b" * 40 + "\n", "")
        def rawsmith(exe, *args):
            if args[0] == "inspect":
                return {"camera": {"orientation": 1}}, 0
            if "-o" in args:
                Path(args[args.index("-o") + 1]).write_bytes(b"synthetic render")
            if args[0] == "compare":
                return {"output_color_space": "sRGB", "panels": [{"color_space": "sRGB",
                        "color_declaration": {"source": "raw-exif", "color_space": "sRGB"},
                        "color_conversion": None}]}, 0
            if args[0] == "auto":
                photos.save(args[args.index("--recipe") + 1], {})
            return {"stats": {"clipped_highlights_pct": 0}, "recipe": {}}, 0
        with mock.patch.object(photos.subprocess, "run", side_effect=run):
            with mock.patch.object(photos, "rawsmith", side_effect=rawsmith):
                with mock.patch.object(photos.shutil, "which", return_value=None):
                    with contextlib.redirect_stdout(io.StringIO()):
                        photos.cmd_render(SimpleNamespace(work=str(self.work), raws=str(raws), recipes=None, rawsmith=str(binary)))
        manifest = photos.load(str(self.work / "manifest.json"))
        self.assertEqual(manifest["rendering"], {"rawsmith_version": "rawsmith fake-version", "repository_commit": "b" * 40})
        report, _ = self.report()
        self.assertEqual(report["rendering"], manifest["rendering"])
        self.assertNotIn("private-name", json.dumps(report))

    def test_rendering_identity_queries_binary_and_handles_missing_git(self):
        def run(command, **kwargs):
            if command[-1] == "--version":
                return subprocess.CompletedProcess(command, 0, "rawsmith 0.2.0\n", "")
            raise FileNotFoundError("git absent")
        with mock.patch.object(photos.subprocess, "run", side_effect=run) as calls:
            rendering = photos.rendering_identity("synthetic-rawsmith")
        self.assertEqual(calls.call_args_list[0].args[0], ["synthetic-rawsmith", "--version"])
        self.assertEqual(rendering, {"rawsmith_version": "rawsmith 0.2.0", "repository_commit": None})


class CodexAdapterTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.base = Path(self.scratch.name)
        self.adapter = Path(photos.REPO) / "bench" / "judge-codex.py"
        # Provide only the launcher's dependencies, never the real Codex.
        (self.base / "python3").symlink_to(sys.executable)
        self.env = {**os.environ, "PATH": str(self.base), "JUDGE_MODEL": "hidden-model"}
        self.prompt = "Image A: /synthetic/a.jpg\nImage B: /synthetic/b.jpg\nJudge synthetic images."

    def run_adapter(self, *args):
        return subprocess.run([str(self.adapter), *args], input=self.prompt, capture_output=True, text=True, env=self.env)

    def fake_codex(self, body):
        path = self.base / "codex"
        path.write_text("#!" + sys.executable + "\n" + body)
        path.chmod(0o755)

    def test_missing_codex_fails_loudly(self):
        self.assertTrue(os.access(self.adapter, os.X_OK))
        result = self.run_adapter()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("codex not found on PATH", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_codex_failed_run_and_missing_or_empty_reply_fail_loudly(self):
        for body, message in (("import sys; sys.stderr.write('synthetic failure'); sys.exit(7)", "exit 7"),
                              ("pass", "failed"),
                              ("import sys; open(sys.argv[sys.argv.index('-o')+1], 'w').close()", "empty reply")):
            with self.subTest(message=message):
                self.fake_codex(body)
                result = self.run_adapter()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_adapter_attaches_images_pins_model_and_disables_integrations(self):
        capture = self.base / "argv.json"
        self.fake_codex("import json,sys\n"
                        + f"with open({str(capture)!r}, 'w') as f: json.dump(sys.argv[1:], f)\n"
                        + "with open(sys.argv[sys.argv.index('-o')+1], 'w') as f: f.write('{\"winner\":\"tie\"}')\n")
        for model, args in (("gpt-6-luna", ()), ("explicit-model", ("--model", "explicit-model"))):
            result = self.run_adapter(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), {"winner": "tie"})
            argv = json.loads(capture.read_text())
            self.assertEqual(argv[argv.index("-m") + 1], model)
            self.assertEqual(argv[argv.index("-s") + 1], "read-only")
            self.assertIn("--ephemeral", argv)
            self.assertIn("--ignore-user-config", argv)
            for feature in ("hooks", "apps", "plugins", "memories"):
                self.assertIn(["--disable", feature], [argv[i:i+2] for i in range(len(argv)-1)])
            self.assertEqual([argv[i+1] for i, value in enumerate(argv) if value == "-i"],
                             ["/synthetic/a.jpg", "/synthetic/b.jpg"])
            self.assertIn(self.prompt, argv[-1])
            self.assertNotIn("hidden-model", argv)


if __name__ == "__main__":
    unittest.main()
