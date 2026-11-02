#!/usr/bin/env python3
"""Offline archive contracts using fake binaries; only macOS signing is mocked."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("package-release.py")
spec = importlib.util.spec_from_file_location("package_release", SCRIPT)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

TAG = "v0.1.0-preview.20261102.1"
SOURCE = "315410d7190a181c5aa92db742f70d00f57f4648"
BINARY = b"Fake Solder executable, deliberately not a runnable program.\x00\xff\r\n"
REPORT = b"Dynamic libraries from the native build:\r\n  system library (1.0)\r\n"


class ReleasePackagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="solder-release-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        inputs = self.root / "inputs with spaces"
        inputs.mkdir()
        self.binary = inputs / "untrusted $() [binary].bin"
        self.binary.write_bytes(BINARY)
        self.binary.chmod(0o755)
        self.report = inputs / "report with spaces.txt"
        self.report.write_bytes(REPORT)
        self.out = self.root / "artifacts with spaces"

    def arguments(self, **overrides):
        options = {
            "binary": self.binary, "platform": "linux", "arch": "x86_64",
            "tag": TAG, "source": SOURCE, "out": self.out,
            "runtime-report": self.report,
        }
        options.update(overrides)
        return [item for key, value in options.items() if value is not None
                for item in ("--" + key, str(value))]

    def cli(self, **overrides):
        return subprocess.run(
            [sys.executable, "-B", str(SCRIPT), *self.arguments(**overrides)],
            capture_output=True, text=True, timeout=20,
        )

    def archive_path(self, platform="linux", arch="x86_64", tag=TAG):
        suffix = ".tar.gz" if platform == "linux" else ".zip"
        return self.out / f"solder-{tag}-{platform}-{arch}{suffix}"

    def packaged(self, platform="linux", arch="x86_64", **overrides):
        result = self.cli(platform=platform, arch=arch, **overrides)
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.archive_path(platform, arch, overrides.get("tag", TAG))
        self.assertTrue(path.is_file())
        self.assertEqual(Path(result.stdout.strip()), path)
        return path

    def contents(self, path):
        if path.name.endswith(".tar.gz"):
            with tarfile.open(path, "r:gz") as archive:
                members = archive.getmembers()
                self.assertTrue(all(item.isfile() or item.isdir() for item in members))
                names = [item.name for item in members]
                data = {}
                for item in members:
                    if item.isfile():
                        with archive.extractfile(item) as stream:
                            data[item.name] = stream.read()
                modes = {item.name: item.mode for item in members}
        else:
            with zipfile.ZipFile(path) as archive:
                self.assertIsNone(archive.testzip())
                members = archive.infolist()
                names = [item.filename for item in members]
                data = {item.filename: archive.read(item) for item in members if not item.is_dir()}
                modes = {}
                for item in members:
                    self.assertEqual(item.create_system, 3)
                    mode = item.external_attr >> 16
                    self.assertEqual(stat.S_IFMT(mode), stat.S_IFDIR if item.is_dir() else stat.S_IFREG)
                    modes[item.filename.rstrip("/")] = stat.S_IMODE(mode)
        self.assertEqual(len(names), len(set(names)))
        for name in names:
            self.assertFalse(PurePosixPath(name).is_absolute())
            self.assertNotIn("..", PurePosixPath(name).parts)
            self.assertNotIn("\\", name)
            self.assertNotIn(":", name)
        return data, modes

    def assert_metadata(self, path, data, platform, arch, executable, expected_binary=BINARY, source=SOURCE, tag=TAG):
        self.assertEqual(data[executable], expected_binary)
        metadata = json.loads(data["BUILD-INFO.json"])
        for key, expected in (("source", source), ("tag", tag), ("platform", platform), ("arch", arch)):
            self.assertEqual(metadata[key], expected)
        self.assertEqual(metadata["binary"], executable)
        self.assertEqual(metadata["binary_sha256"], hashlib.sha256(data[executable]).hexdigest())
        checksum = path.with_name(path.name + ".sha256").read_text(encoding="ascii")
        self.assertEqual(checksum, f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n")
        self.assertEqual(self.binary.read_bytes(), BINARY, "packaging must not modify the build input")
        readme = data["README.txt"].decode("utf-8")
        self.assertIn("Prerelease", readme)
        self.assertIn("Packaging does not verify GUI startup", readme)
        self.assertNotIn("--version", readme)

    def test_linux_tar_has_launchable_binary_provenance_and_runtime_requirements(self):
        for arch in ("x86_64", "arm64"):
            with self.subTest(arch=arch):
                path = self.packaged(arch=arch)
                data, modes = self.contents(path)
                self.assertEqual(set(data), {"solder", "BUILD-INFO.json", "README.txt", "RUNTIME-LIBRARIES.txt"})
                self.assert_metadata(path, data, "linux", arch, "solder")
                self.assertEqual(modes["solder"], 0o755)
                self.assertEqual(modes["README.txt"], 0o644)
                self.assertEqual(data["RUNTIME-LIBRARIES.txt"], REPORT)
                readme = data["README.txt"].decode("utf-8")
                for requirement in ("Ubuntu 24.04", arch, "./solder", "X11", "Wayland", "Vulkan",
                                    "WebKitGTK 4.1", "libwebkit2gtk-4.1-0", "unsigned"):
                    self.assertIn(requirement, readme)
                self.assertIn("not supported under Wayland", readme)

    def test_windows_zip_has_exe_and_webview_requirement(self):
        for arch in ("x86_64", "arm64"):
            with self.subTest(arch=arch):
                path = self.packaged(platform="windows", arch=arch)
                data, _ = self.contents(path)
                self.assertEqual(set(data), {"solder.exe", "BUILD-INFO.json", "README.txt", "RUNTIME-LIBRARIES.txt"})
                self.assert_metadata(path, data, "windows", arch, "solder.exe")
                self.assertEqual(data["RUNTIME-LIBRARIES.txt"], REPORT)
                readme = data["README.txt"].decode("utf-8")
                for requirement in ("Extract", "double-click solder.exe", "Windows", "WebView2 Runtime",
                                    "extension pages", "unsigned"):
                    self.assertIn(requirement, readme)

    def mac_package(self, signer, **overrides):
        output = io.StringIO()
        with patch.object(release.sys, "platform", "darwin"), \
                patch.object(release.subprocess, "run", side_effect=signer) as signing, \
                contextlib.redirect_stdout(output):
            status = release.main(self.arguments(platform="macos", **overrides))
        self.assertEqual(status, 0)
        self.assertEqual(signing.call_count, 2)
        return self.archive_path("macos", overrides.get("arch", "x86_64"))

    def fake_signer(self, argv, **options):
        self.assertEqual(argv[0], "/usr/bin/codesign")
        self.assertTrue(options["check"])
        self.assertFalse(options.get("shell", False))
        bundle = Path(argv[-1])
        self.assertEqual(bundle.name, "Solder.app")
        executable = bundle / "Contents/MacOS/solder"
        plist = bundle / "Contents/Info.plist"
        self.assertTrue(plist.is_file(), "the bundle must be complete before signing")
        if "--sign" in argv:
            self.assertEqual(argv[argv.index("--sign") + 1], "-")
            self.assertIn("--timestamp=none", argv)
            executable.write_bytes(BINARY + b" ad hoc signature")
            signature = bundle / "Contents/_CodeSignature"
            signature.mkdir()
            (signature / "CodeResources").write_bytes(b"signed resource seal")
        else:
            self.assertIn("--verify", argv)
            self.assertIn("--strict", argv)
            self.assertEqual(executable.read_bytes(), BINARY + b" ad hoc signature")
        return subprocess.CompletedProcess(argv, 0, "", "")

    def test_macos_bundle_plist_permissions_and_hash_after_signing(self):
        for arch in ("arm64", "x86_64"):
            with self.subTest(arch=arch):
                path = self.mac_package(self.fake_signer, arch=arch)
                data, modes = self.contents(path)
                executable = "Solder.app/Contents/MacOS/solder"
                self.assert_metadata(path, data, "macos", arch, executable, BINARY + b" ad hoc signature")
                self.assertEqual(data["Solder.app/Contents/_CodeSignature/CodeResources"], b"signed resource seal")
                self.assertEqual(data["RUNTIME-LIBRARIES.txt"], REPORT)
                self.assertEqual(modes[executable], 0o755)
                for directory in ("Solder.app", "Solder.app/Contents", "Solder.app/Contents/MacOS"):
                    self.assertEqual(modes[directory], 0o755)
                plist = plistlib.loads(data["Solder.app/Contents/Info.plist"])
                for key, expected in (
                    ("CFBundleIdentifier", "dev.solder.ide"), ("CFBundleExecutable", "solder"),
                    ("CFBundleName", "Solder"), ("CFBundleDisplayName", "Solder"),
                    ("CFBundlePackageType", "APPL"), ("CFBundleShortVersionString", "0.1.0"),
                    ("CFBundleVersion", "0.1.0"), ("LSMinimumSystemVersion", "15.0"),
                ):
                    self.assertEqual(plist[key], expected)
                self.assertIs(plist["NSHighResolutionCapable"], True)
                readme = data["README.txt"].decode("utf-8")
                for requirement in ("macOS 15.0", "Solder.app", "Applications", "ad hoc signed", "not notarized"):
                    self.assertIn(requirement, readme)

    def test_macos_signing_or_verification_failure_leaves_no_release(self):
        for failed_step in ("--sign", "--verify"):
            with self.subTest(step=failed_step):
                def signer(argv, **options):
                    if failed_step in argv:
                        raise subprocess.CalledProcessError(1, argv, stderr="fixture signing failure")
                    return self.fake_signer(argv, **options)

                errors = io.StringIO()
                with patch.object(release.sys, "platform", "darwin"), \
                        patch.object(release.subprocess, "run", side_effect=signer), \
                        contextlib.redirect_stderr(errors), self.assertRaises(SystemExit) as failure:
                    release.main(self.arguments(platform="macos"))
                self.assertNotEqual(failure.exception.code, 0)
                self.assertIn("fixture signing failure", errors.getvalue())
                self.assertEqual(list(self.out.iterdir()), [])
                self.assertEqual(self.binary.read_bytes(), BINARY)

    def test_macos_requires_native_signing_instead_of_silently_emitting_unsigned_bundle(self):
        with patch.object(release.sys, "platform", "linux"), \
                patch.object(release.subprocess, "run") as signing, \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            release.main(self.arguments(platform="macos"))
        signing.assert_not_called()
        self.assertFalse(self.out.exists())

    def test_optional_report_can_be_absent_or_empty(self):
        path = self.packaged(**{"runtime-report": None})
        data, _ = self.contents(path)
        self.assertNotIn("RUNTIME-LIBRARIES.txt", data)
        self.report.write_bytes(b"")
        path = self.packaged(platform="windows")
        data, _ = self.contents(path)
        self.assertEqual(data["RUNTIME-LIBRARIES.txt"], b"")

    def test_rejects_malformed_and_path_injecting_identifiers_before_creating_output(self):
        cases = {
            "tag": ("", "0.1.0", "v01.0.0", "v0.1", "../escape", "v0.1.0/../../escape",
                    "v0.1.0\\escape", "v0.1.0:stream", "v0.1.0\n", "v0.1.0;echo bad",
                    "v0.1.0$(echo bad)", "v0.1.0%2fescape", "v0.1.0-", "v0.1.0-" + "a" * 150),
            "source": ("", "HEAD", "a" * 39, "a" * 41, "g" * 40, "../" + "a" * 37,
                       "a" * 39 + "\n"),
            "platform": ("../linux", "Linux", "linux/escape", "freebsd"),
            "arch": ("amd64", "ARM64", "../x86_64", "x86_64\\escape"),
        }
        for option, values in cases.items():
            for value in values:
                with self.subTest(option=option, value=value):
                    result = self.cli(**{option: value})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertTrue(result.stderr)
                    self.assertNotIn("Traceback", result.stderr)
                    self.assertFalse(self.out.exists())

    def test_accepts_complete_hex_source_without_rewriting_provenance(self):
        source = SOURCE.upper()
        tag = "v0.1.0-preview.20261102.2+build.7"
        path = self.packaged(source=source, tag=tag)
        data, _ = self.contents(path)
        self.assert_metadata(path, data, "linux", "x86_64", "solder", source=source, tag=tag)

    def test_missing_empty_or_directory_binary_and_missing_report_are_rejected(self):
        empty = self.root / "empty binary"
        empty.touch()
        for binary in (self.root / "missing binary", empty, self.root):
            with self.subTest(binary=binary):
                result = self.cli(binary=binary)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("nonempty regular file", result.stderr)
                self.assertFalse(self.out.exists())
        for report in (self.root / "missing report", self.root):
            with self.subTest(report=report):
                result = self.cli(**{"runtime-report": report})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("runtime report", result.stderr)
                self.assertFalse(self.out.exists())

    def test_existing_archive_is_preserved_and_other_platforms_can_share_output(self):
        linux = self.packaged()
        original = {path.name: path.read_bytes() for path in self.out.iterdir()}
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to overwrite", result.stderr)
        self.assertEqual({path.name: path.read_bytes() for path in self.out.iterdir()}, original)
        windows = self.packaged(platform="windows")
        mac_arm = self.mac_package(self.fake_signer, arch="arm64")
        mac_intel = self.mac_package(self.fake_signer, arch="x86_64")
        archives = (linux, windows, mac_arm, mac_intel)
        expected = {path.name for archive in archives for path in (archive, archive.with_name(archive.name + ".sha256"))}
        self.assertEqual({path.name for path in self.out.iterdir()}, expected)

    def test_existing_checksum_or_directory_is_not_overwritten(self):
        self.out.mkdir()
        archive = self.archive_path()
        checksum = archive.with_name(archive.name + ".sha256")
        checksum.write_bytes(b"existing checksum")
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(checksum.read_bytes(), b"existing checksum")
        self.assertFalse(archive.exists())
        checksum.unlink()
        archive.mkdir()
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(archive.is_dir())
        self.assertFalse(checksum.exists())

    def test_nonexecutable_input_is_packaged_as_executable(self):
        self.binary.chmod(0o644)
        path = self.packaged()
        _, modes = self.contents(path)
        self.assertEqual(modes["solder"], 0o755)
        path = self.mac_package(self.fake_signer)
        _, modes = self.contents(path)
        self.assertEqual(modes["Solder.app/Contents/MacOS/solder"], 0o755)

    @unittest.skipIf(os.name == "nt", "Windows source files do not carry Unix execute permissions")
    def test_unix_binary_permissions_survive_tar_and_zip(self):
        self.binary.chmod(0o751)
        linux = self.packaged()
        _, modes = self.contents(linux)
        self.assertEqual(modes["solder"], 0o751)
        mac = self.mac_package(self.fake_signer)
        _, modes = self.contents(mac)
        self.assertEqual(modes["Solder.app/Contents/MacOS/solder"], 0o751)


if __name__ == "__main__":
    unittest.main()
