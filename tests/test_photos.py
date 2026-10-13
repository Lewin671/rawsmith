"""Filesystem safety checks for the photo benchmark; no photos or tools needed."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest


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


if __name__ == "__main__":
    unittest.main()
