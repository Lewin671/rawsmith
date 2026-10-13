"""Offline installer integration tests; no network or real user directories."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


INSTALLER = Path(
    os.environ.get(
        "RAWSMITH_INSTALL_SCRIPT", Path(__file__).resolve().parents[1] / "install.sh"
    )
).resolve()
TARGETS = {
    ("Linux", "x86_64"): "x86_64-unknown-linux-musl",
    ("Linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Darwin", "arm64"): "aarch64-apple-darwin",
}
PLATFORMS = {
    ("Linux", "x86_64"): "linux-x86_64",
    ("Linux", "aarch64"): "linux-arm64",
    ("Darwin", "x86_64"): "macos-x86_64",
    ("Darwin", "arm64"): "macos-arm64",
}


class InstallerTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="rawsmith install tests ")
        self.addCleanup(scratch.cleanup)
        self.base = Path(scratch.name)
        self.home = self.base / "isolated home"
        self.home.mkdir()
        self.downloads = self.base / "release fixtures"
        self.downloads.mkdir()
        self.tools = self.base / "fake tools"
        self.tools.mkdir()
        self.log = self.base / "requests.jsonl"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": str(self.tools) + os.pathsep + os.environ.get("PATH", ""),
            "FAKE_RELEASES": str(self.downloads),
            "FAKE_CURL_LOG": str(self.log),
            "FAKE_OS": "Linux",
            "FAKE_ARCH": "x86_64",
            "FAKE_LATEST": "v0.1.0",
            "TMPDIR": str(self.base),
        }
        self.env.pop("CLAUDE_CONFIG_DIR", None)
        self.env.pop("CODEX_HOME", None)
        self.write_tool(
            "uname",
            "import os, sys\n"
            "print(os.environ['FAKE_OS' if sys.argv[1] == '-s' else 'FAKE_ARCH'])\n",
        )
        self.write_tool(
            "curl",
            """import json, os, pathlib, shutil, sys
args = sys.argv[1:]
output = None
write_out = ''
urls = []
i = 0
while i < len(args):
    arg = args[i]
    if arg in ('-o', '--output', '-w', '--write-out'):
        i += 1
        if arg in ('-o', '--output'):
            output = args[i]
        else:
            write_out = args[i]
    elif arg.startswith('https://'):
        urls.append(arg)
    elif arg in ('--retry', '--connect-timeout', '--max-time', '--proto', '--tlsv1.2'):
        if arg != '--tlsv1.2':
            i += 1
    i += 1
if len(urls) != 1:
    sys.exit('fake curl expected one HTTPS URL')
url = urls[0]
with open(os.environ['FAKE_CURL_LOG'], 'a') as stream:
    stream.write(json.dumps(url) + '\\n')
if url.endswith('/releases/latest'):
    effective = url[:-len('latest')] + 'tag/' + os.environ['FAKE_LATEST']
    if output and output != '/dev/null':
        pathlib.Path(output).write_bytes(b'')
    sys.stdout.write(write_out.replace('%{url_effective}', effective))
    sys.exit(0)
parts = url.split('/releases/download/')
if len(parts) != 2:
    sys.exit('unexpected release URL: ' + url)
source = pathlib.Path(os.environ['FAKE_RELEASES']) / parts[1]
if not source.is_file():
    sys.exit('missing fixture: ' + str(source))
if output:
    shutil.copyfile(source, output)
else:
    sys.stdout.buffer.write(source.read_bytes())
""",
        )
        self.release("0.1.0")
        self.release("0.1.1")
        self.release("0.2.0")

    def write_tool(self, name, source):
        path = self.tools / name
        path.write_text(f"#!{sys.executable}\n" + source)
        path.chmod(0o755)

    @staticmethod
    def skill_contents(version):
        return (
            "---\nname: rawsmith\ndescription: Develop RAW photos locally.\n---\n"
            f"Use rawsmith {version} to inspect, preview, and export.\n"
        ).encode()

    def release(
        self, version, missing=None, binary_version=None, extra=None,
        manifest_version=None,
    ):
        folder = self.downloads / ("v" + version)
        folder.mkdir(exist_ok=True)
        files = {
            "bin/rawsmith": (
                f"#!/bin/sh\nprintf '%s\\n' 'rawsmith {binary_version or version}'\n"
            ).encode(),
            "skill/rawsmith/SKILL.md": self.skill_contents(version),
            "VERSION": ((manifest_version or version) + "\n").encode(),
            "LICENSE-MIT": b"Synthetic MIT licence fixture\n",
            "LICENSE-APACHE": b"Synthetic Apache licence fixture\n",
            "README.md": b"Synthetic release readme\n",
        }
        files.pop(missing, None)
        names = TARGETS.values() if version == "0.1.0" else PLATFORMS.values()
        for name in names:
            archive = folder / f"rawsmith-{name}.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                for name, contents in files.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(contents)
                    member.mode = 0o755 if name == "bin/rawsmith" else 0o644
                    bundle.addfile(member, io.BytesIO(contents))
                if extra:
                    member, contents = extra
                    bundle.addfile(member, io.BytesIO(contents) if contents else None)
        self.checksums(version)

    def checksums(self, version):
        folder = self.downloads / ("v" + version)
        (folder / "SHA256SUMS").write_text(
            "".join(
                f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
                for path in sorted(folder.glob("*.tar.gz"))
            )
        )

    def install(self, *args, success=True):
        result = subprocess.run(
            ["sh", str(INSTALLER), *args],
            env=self.env,
            cwd=self.base,
            capture_output=True,
            text=True,
            timeout=30,
        )
        message = f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        if success:
            self.assertEqual(result.returncode, 0, message)
        else:
            self.assertNotEqual(result.returncode, 0, message)
        return result

    @property
    def binary(self):
        return self.home / ".local/bin/rawsmith"

    @property
    def claude(self):
        return self.home / ".claude/skills/rawsmith"

    @property
    def codex(self):
        return self.home / ".agents/skills/rawsmith"

    def assert_binary(self, version="0.1.0", path=None):
        binary = path or self.binary
        self.assertTrue(os.access(binary, os.X_OK))
        result = subprocess.run(
            [str(binary), "--version"], capture_output=True, text=True, check=True
        )
        self.assertEqual(result.stdout.strip(), "rawsmith " + version)

    def assert_skill(self, path, version="0.1.0"):
        contents = self.skill_contents(version)
        self.assertEqual((path / "SKILL.md").read_bytes(), contents)
        self.assertEqual(
            (path / ".rawsmith-source.sha256").read_text().strip(),
            hashlib.sha256(contents).hexdigest(),
        )

    def seed_binary(self):
        self.binary.parent.mkdir(parents=True, exist_ok=True)
        self.binary.write_bytes(b"existing binary must survive\n")
        return self.binary.read_bytes()

    def requests(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_default_install_resolves_latest_and_installs_both_skills(self):
        self.install()
        self.assert_binary()
        self.assert_skill(self.claude)
        self.assert_skill(self.codex)
        self.assertFalse((self.home / ".codex").exists())
        requests = self.requests()
        self.assertEqual(sum(url.endswith("/releases/latest") for url in requests), 1)
        downloads = [url for url in requests if "/releases/download/" in url]
        self.assertEqual(len(downloads), 2)
        self.assertTrue(all("/releases/download/v0.1.0/" in url for url in downloads))

    def test_all_supported_platforms_download_the_matching_archive(self):
        for (system, arch), target in TARGETS.items():
            with self.subTest(system=system, arch=arch):
                self.env.update(FAKE_OS=system, FAKE_ARCH=arch)
                self.install("--version", "v0.1.0", "--skills", "none")
                self.assert_binary()
                self.assertTrue(
                    any(url.endswith(f"/rawsmith-{target}.tar.gz") for url in self.requests())
                )
        self.assertFalse(self.claude.exists())
        self.assertFalse(self.codex.exists())
        self.assertFalse(any(url.endswith("/releases/latest") for url in self.requests()))

    def test_new_releases_use_friendly_names_on_every_platform(self):
        for (system, arch), platform in PLATFORMS.items():
            with self.subTest(system=system, arch=arch):
                self.env.update(FAKE_OS=system, FAKE_ARCH=arch)
                result = self.install("--version", "0.1.1", "--skills", "none")
                self.assert_binary("0.1.1")
                self.assertIn(f"for {platform}", result.stdout)
                self.assertTrue(any(
                    url.endswith(f"/v0.1.1/rawsmith-{platform}.tar.gz")
                    for url in self.requests()
                ))
        self.assertFalse(any("unknown" in url or "apple-darwin" in url for url in self.requests()))

    def test_latest_installs_the_renamed_release_and_both_skills(self):
        self.env["FAKE_LATEST"] = "v0.1.1"
        self.install()
        self.assert_binary("0.1.1")
        self.assert_skill(self.claude, "0.1.1")
        self.assert_skill(self.codex, "0.1.1")
        self.assertTrue(any(url.endswith("/v0.1.1/rawsmith-linux-x86_64.tar.gz") for url in self.requests()))

    def test_unprefixed_version_and_custom_directories_with_spaces(self):
        binary_dir = self.base / "custom bin directory"
        claude_dir = self.base / "custom Claude skills"
        codex_dir = self.base / "custom Codex skills"
        self.install(
            "--version", "0.1.0", "--bin-dir", str(binary_dir),
            "--claude-skills-dir", str(claude_dir),
            "--codex-skills-dir", str(codex_dir),
        )
        self.assert_binary(path=binary_dir / "rawsmith")
        self.assert_skill(claude_dir / "rawsmith")
        self.assert_skill(codex_dir / "rawsmith")
        self.assertFalse(self.binary.exists())
        self.assertFalse(self.claude.exists())
        self.assertFalse(self.codex.exists())

    def test_claude_config_dir_is_respected(self):
        config = self.base / "alternate Claude configuration"
        self.env["CLAUDE_CONFIG_DIR"] = str(config)
        self.install("--version", "0.1.0", "--skills", "claude")
        self.assert_skill(config / "skills/rawsmith")
        self.assertFalse(self.claude.exists())
        self.assertFalse(self.codex.exists())

    def test_codex_only_does_not_install_claude_skill(self):
        result = self.install("--version", "0.1.0", "--skills", "codex")
        self.assert_skill(self.codex)
        self.assertFalse(self.claude.exists())
        self.assertIn("reload skills or restart the agent", result.stdout)

    def test_no_skills_prints_binary_guidance_and_stages_under_tmpdir(self):
        staging = self.base / "download staging"
        staging.mkdir()
        self.env["TMPDIR"] = str(staging)
        # Record the actual output paths passed to the offline curl fixture.
        curl = self.tools / "curl"
        source = curl.read_text().replace(
            "if output:\n    shutil.copyfile(source, output)",
            "if output:\n    with open(os.environ['FAKE_STAGE_LOG'], 'a') as log:\n"
            "        log.write(output + '\\n')\n    shutil.copyfile(source, output)",
        )
        curl.write_text(source)
        log = self.base / "stage paths"
        self.env["FAKE_STAGE_LOG"] = str(log)
        result = self.install("--version", "0.1.1", "--skills", "none")
        self.assert_binary("0.1.1")
        self.assertFalse(self.claude.exists() or self.codex.exists())
        self.assertIn("No agent skill was installed", result.stdout)
        self.assertIn("rawsmith --help", result.stdout)
        self.assertNotIn("reload skills", result.stdout)
        self.assertNotIn("restart the agent", result.stdout)
        paths = log.read_text().splitlines()
        self.assertEqual(len(paths), 2)
        self.assertTrue(all(Path(path).is_relative_to(staging) for path in paths))
        self.assertEqual(list(staging.iterdir()), [])

    def test_help_does_not_download_or_modify_home(self):
        result = self.install("--help")
        self.assertIn("--skills", result.stdout + result.stderr)
        self.assertIn("TMPDIR (default: /tmp)", result.stdout)
        self.assertFalse(self.log.exists())
        self.assertEqual(list(self.home.iterdir()), [])

    def test_invalid_options_do_not_download_or_modify_home(self):
        for args in (
            ("--unknown",), ("--version",), ("--skills", "invalid"),
            ("--version", "../../unsafe"),
        ):
            with self.subTest(args=args):
                self.install(*args, success=False)
                self.assertFalse(self.log.exists())
                self.assertEqual(list(self.home.iterdir()), [])

    def test_unsupported_platform_does_not_modify_home(self):
        for system, arch in (("FreeBSD", "x86_64"), ("Linux", "riscv64")):
            with self.subTest(system=system, arch=arch):
                self.env.update(FAKE_OS=system, FAKE_ARCH=arch)
                self.install("--version", "0.1.0", success=False)
                self.assertEqual(list(self.home.iterdir()), [])

    def test_bad_checksum_preserves_existing_binary_and_skills(self):
        original = self.seed_binary()
        self.claude.mkdir(parents=True)
        (self.claude / "SKILL.md").write_bytes(b"my custom instructions\n")
        sums = self.downloads / "v0.1.0/SHA256SUMS"
        sums.write_text(
            "".join(
                "0" * 64 + "  " + path.name + "\n"
                for path in sums.parent.glob("*.tar.gz")
            )
        )
        self.install("--version", "0.1.0", "--force", success=False)
        self.assertEqual(self.binary.read_bytes(), original)
        self.assertEqual(
            (self.claude / "SKILL.md").read_bytes(), b"my custom instructions\n"
        )
        self.assertEqual(list(self.claude.iterdir()), [self.claude / "SKILL.md"])
        self.assertFalse(self.codex.exists())

    def test_missing_required_files_preserve_existing_binary(self):
        original = self.seed_binary()
        for missing in ("bin/rawsmith", "skill/rawsmith/SKILL.md", "VERSION"):
            with self.subTest(missing=missing):
                self.release("0.1.0", missing=missing)
                self.install("--version", "0.1.0", success=False)
                self.assertEqual(self.binary.read_bytes(), original)
                self.assertFalse(self.claude.exists())
                self.assertFalse(self.codex.exists())

    def test_binary_version_mismatch_preserves_existing_binary(self):
        original = self.seed_binary()
        self.release("0.1.0", binary_version="9.9.9")
        self.install("--version", "0.1.0", success=False)
        self.assertEqual(self.binary.read_bytes(), original)
        self.assertFalse(self.claude.exists())

    def test_manifest_version_mismatch_preserves_existing_binary(self):
        original = self.seed_binary()
        self.release("0.1.0", manifest_version="9.9.9")
        self.install("--version", "0.1.0", success=False)
        self.assertEqual(self.binary.read_bytes(), original)
        self.assertFalse(self.claude.exists())

    def test_existing_custom_skill_blocks_all_installation(self):
        original = self.seed_binary()
        self.codex.mkdir(parents=True)
        custom = b"My independently maintained skill\n"
        (self.codex / "SKILL.md").write_bytes(custom)
        self.install("--version", "0.1.0", success=False)
        self.assertEqual(self.binary.read_bytes(), original)
        self.assertEqual((self.codex / "SKILL.md").read_bytes(), custom)
        self.assertFalse(self.claude.exists())
        self.assertFalse((self.codex / ".rawsmith-source.sha256").exists())

    def test_repeat_install_and_managed_upgrade(self):
        self.install("--version", "0.1.0")
        self.install("--version", "0.1.0")
        self.assert_binary()
        self.assert_skill(self.claude)
        self.assert_skill(self.codex)
        self.install("--version", "0.2.0")
        self.assert_binary("0.2.0")
        self.assert_skill(self.claude, "0.2.0")
        self.assert_skill(self.codex, "0.2.0")

    def test_identical_unmarked_skill_can_be_adopted(self):
        self.claude.mkdir(parents=True)
        (self.claude / "SKILL.md").write_bytes(self.skill_contents("0.1.0"))
        self.install("--version", "0.1.0")
        self.assert_skill(self.claude)

    def test_manually_modified_managed_skill_blocks_upgrade(self):
        self.install("--version", "0.1.0")
        original_binary = self.binary.read_bytes()
        custom = self.skill_contents("0.1.0") + b"Keep this local instruction.\n"
        (self.codex / "SKILL.md").write_bytes(custom)
        self.install("--version", "0.2.0", success=False)
        self.assertEqual(self.binary.read_bytes(), original_binary)
        self.assertEqual((self.codex / "SKILL.md").read_bytes(), custom)
        self.assert_skill(self.claude)

    def test_force_backs_up_custom_skill_before_installing(self):
        self.codex.mkdir(parents=True)
        custom = b"Keep my custom skill in a backup.\n"
        (self.codex / "SKILL.md").write_bytes(custom)
        self.install("--version", "0.1.0", "--force")
        self.assert_binary()
        self.assert_skill(self.codex)
        backups = [
            path for path in (self.home / ".agents/skills").rglob("*")
            if path.is_file() and path.read_bytes() == custom
        ]
        self.assertTrue(backups, "--force must retain the user's original SKILL.md")

    def test_skill_directory_symlink_never_writes_through(self):
        outside = self.base / "outside skill"
        outside.mkdir()
        (outside / "SKILL.md").write_bytes(b"outside skill must survive\n")
        self.codex.parent.mkdir(parents=True)
        self.codex.symlink_to(outside, target_is_directory=True)
        original_binary = self.seed_binary()
        self.install("--version", "0.1.0", "--force", success=False)
        self.assertEqual(list(outside.iterdir()), [outside / "SKILL.md"])
        self.assertEqual(
            (outside / "SKILL.md").read_bytes(), b"outside skill must survive\n"
        )
        self.assertEqual(self.binary.read_bytes(), original_binary)

    def test_skill_file_symlink_never_writes_through(self):
        outside = self.base / "outside skill.md"
        outside.write_bytes(b"outside file must survive\n")
        self.codex.mkdir(parents=True)
        (self.codex / "SKILL.md").symlink_to(outside)
        original_binary = self.seed_binary()
        self.install("--version", "0.1.0", "--force", success=False)
        self.assertEqual(outside.read_bytes(), b"outside file must survive\n")
        self.assertEqual(self.binary.read_bytes(), original_binary)

    def test_binary_symlink_never_writes_through(self):
        outside = self.base / "outside binary"
        outside.write_bytes(b"outside binary must survive\n")
        self.binary.parent.mkdir(parents=True)
        self.binary.symlink_to(outside)
        self.install("--version", "0.1.0", "--force", success=False)
        self.assertEqual(outside.read_bytes(), b"outside binary must survive\n")
        self.assertFalse(self.claude.exists())

    def test_archive_symlinks_and_hardlinks_are_rejected(self):
        original = self.seed_binary()
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
            with self.subTest(kind=kind):
                member = tarfile.TarInfo("skill/rawsmith/unsafe-link")
                member.type = kind
                member.linkname = "../../bin/rawsmith"
                self.release("0.1.0", extra=(member, b""))
                self.install("--version", "0.1.0", success=False)
                self.assertEqual(self.binary.read_bytes(), original)
                self.assertFalse(self.claude.exists())

    def test_archive_absolute_paths_and_traversal_are_rejected(self):
        original = self.seed_binary()
        escaped = self.base / "must not be extracted"
        for name in (
            str(escaped), "../must not be extracted", "safe/../../must not be extracted",
        ):
            with self.subTest(name=name):
                member = tarfile.TarInfo(name)
                contents = b"unsafe archive payload\n"
                member.size = len(contents)
                self.release("0.1.0", extra=(member, contents))
                self.install("--version", "0.1.0", success=False)
                self.assertEqual(self.binary.read_bytes(), original)
                self.assertFalse(escaped.exists())
                self.assertFalse(self.claude.exists())


if __name__ == "__main__":
    unittest.main()
