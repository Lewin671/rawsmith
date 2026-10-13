"""Check release contents and rejection paths without building or using photos."""

import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"


class ReleasePackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name) / "source with spaces"
        scripts = self.root / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / "package-release.sh"
        shutil.copyfile(ROOT / "scripts/package-release.sh", self.script)
        (self.root / "Cargo.toml").write_text('[package]\nname = "rawsmith"\nversion = "1.2.3"\n')
        skill = self.root / "skill/rawsmith"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("assistant instructions\n")
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "README.md"):
            (self.root / name).write_text(name + "\n")
        # Files outside the explicit distribution list must never enter the tar.
        (self.root / "private-photo.ARW").write_text("private fixture\n")
        (self.root / "private-photo.jpg").write_text("private fixture\n")
        self.binary = self.root / "target" / TARGET / "release/rawsmith"
        self.binary.parent.mkdir(parents=True)
        self.write_binary()
        self.output = pathlib.Path(self.temporary.name) / "output with spaces"

    def write_binary(self, version="1.2.3", schema='{"exposure": {}}'):
        self.binary.write_text(
            '#!/bin/sh\ncase "$1" in\n'
            f'--version) printf "%s\\n" "rawsmith {version}" ;;\n'
            f"schema) printf '%s\\n' '{schema}' ;;\n"
            "*) exit 1 ;;\nesac\n"
        )
        self.binary.chmod(0o755)

    def package(self, tag="v1.2.3", target=TARGET):
        return subprocess.run(
            ["bash", str(self.script), target, tag, str(self.output)],
            text=True, capture_output=True,
        )

    def test_archive_contains_only_distribution_files(self):
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = self.output / f"rawsmith-{TARGET}.tar.gz"
        with tarfile.open(archive) as package:
            files = {entry.name for entry in package.getmembers() if entry.isfile()}
            self.assertEqual(files, {
                "bin/rawsmith", "skill/rawsmith/SKILL.md", "LICENSE-MIT",
                "LICENSE-APACHE", "README.md", "VERSION",
            })
            self.assertEqual(package.extractfile("VERSION").read(), b"1.2.3\n")
            self.assertEqual(package.getmember("bin/rawsmith").mode, 0o755)
        self.assertEqual(list(self.output.iterdir()), [archive])

    def test_existing_archive_is_preserved(self):
        self.output.mkdir()
        archive = self.output / f"rawsmith-{TARGET}.tar.gz"
        archive.write_bytes(b"keep existing archive")
        self.assertNotEqual(self.package().returncode, 0)
        self.assertEqual(archive.read_bytes(), b"keep existing archive")

    def test_rejects_unsupported_target_and_nonstable_tags(self):
        for tag in ("main", "v01.2.3", "v1.2.3-rc.1", "v1.2.3\nextra", "v1.2.4"):
            with self.subTest(tag=tag):
                self.assertNotEqual(self.package(tag=tag).returncode, 0)
        self.assertNotEqual(self.package(target="../../private").returncode, 0)
        self.assertFalse(self.output.exists())

    def test_rejects_wrong_binary_version_and_schema(self):
        for version, schema in (("1.2.4", '{}'), ("1.2.3", 'null'), ("1.2.3", '{}'), ("1.2.3", 'bad json')):
            with self.subTest(version=version, schema=schema):
                self.write_binary(version=version, schema=schema)
                self.assertNotEqual(self.package().returncode, 0)
                self.assertFalse(self.output.exists())

    def test_failed_copy_cleans_only_its_temporary_directory(self):
        self.output.mkdir()
        sentinel = self.output / "keep.txt"
        sentinel.write_text("keep\n")
        (self.root / "LICENSE-MIT").unlink()
        self.assertNotEqual(self.package().returncode, 0)
        self.assertEqual(list(self.output.iterdir()), [sentinel])
        self.assertEqual(sentinel.read_text(), "keep\n")


if __name__ == "__main__":
    unittest.main()
