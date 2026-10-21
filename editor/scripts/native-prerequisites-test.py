#!/usr/bin/env python3
"""Offline contracts for the package planner, not a live Ubuntu package list."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("prerequisites", Path(__file__).with_name("native-prerequisites.py"))
planner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(planner)

# Synthetic records exercise the APT protocol, not current package versions.
URI = "http://azure.archive.ubuntu.com/ubuntu/pool/universe/o/openbox/openbox_1.0-1_amd64.deb"
RECORD = f"'{URI}' openbox_1.0-1_amd64.deb 1234 SHA256:{'a' * 64}"
DEPENDENCY = ("'https://security.ubuntu.com/ubuntu/pool/main/libx/libx11/libx11-6_1.0_all.deb' "
              f"libx11-6_1%3a1.0_all.deb 99 MD5Sum:{'b' * 32}")


class PlannerTests(unittest.TestCase):
    def test_resolver_is_print_only_without_root_or_recommendations(self):
        command = planner.apt_command(Path("/tmp/empty-archives"))
        self.assertEqual(command[0], "apt-get")
        for option in ("--print-uris", "--download-only", "--no-install-recommends", "--no-remove"):
            self.assertIn(option, command)
        self.assertNotIn("--simulate", command)
        self.assertNotIn("sudo", command)
        self.assertEqual(command[-4:], ["install", "openbox", "xdg-desktop-portal", "xdg-desktop-portal-gtk"])
        self.assertIn("Dir::Cache::archives=/tmp/empty-archives/", command)
        self.assertIn("Dir::Cache::pkgcache=", command)
        self.assertIn("Dir::Cache::srcpkgcache=", command)

    def test_records_include_dependencies_exact_sizes_hashes_and_epoch_names(self):
        packages = planner.parse_uris("Reading package lists... Done\n" + RECORD + "\n" + DEPENDENCY)
        self.assertEqual(len(packages), 2)
        self.assertEqual(packages[0]["url"], URI)
        self.assertEqual(packages[0]["filename"], "openbox_1.0-1_amd64.deb")
        self.assertEqual(packages[0]["size_bytes"], 1234)
        self.assertTrue(packages[0]["requested"])
        self.assertFalse(packages[1]["requested"])
        self.assertEqual(packages[1]["version"], "1:1.0")
        self.assertEqual(packages[1]["filename"], "libx11-6_1%3a1.0_all.deb")
        self.assertEqual(packages[1]["source_filename"], "libx11-6_1.0_all.deb")
        self.assertEqual(packages[1]["checksum"], "MD5Sum:" + "b" * 32)

    def test_rejects_nonofficial_or_ambiguous_sources(self):
        for url in (
            URI.replace("azure.archive.ubuntu.com", "archive.ubuntu.com.attacker.invalid"),
            URI.replace("azure.archive.ubuntu.com", "packages.microsoft.com"),
            URI.replace("azure.archive.ubuntu.com", "ppa.launchpad.net"),
            URI.replace("azure.archive.ubuntu.com", "user:pass@archive.ubuntu.com"),
            URI.replace("azure.archive.ubuntu.com", "archive.ubuntu.com:8080"),
            URI.replace("http:", "ftp:"), URI + "?token=private", URI + "#fragment",
            URI.replace("/ubuntu/pool/", "/other/"), URI.replace(".deb", ".tar.gz"),
        ):
            with self.subTest(url=url), self.assertRaises(ValueError):
                planner.parse_uris(RECORD.replace(URI, url))

    def test_rejects_partial_or_malformed_records(self):
        for record in (
            RECORD.rsplit(" ", 1)[0], RECORD + " extra", RECORD.replace("1234", "-1"),
            RECORD.replace("1234", "0"), RECORD.replace("1234", "unknown"),
            RECORD.replace("SHA256:" + "a" * 64, "SHA256:bad"),
            RECORD.replace("' openbox_", "' ../openbox_"),
            RECORD.replace("' openbox_", "' open_box_"), RECORD + "\n" + RECORD,
        ):
            with self.subTest(record=record), self.assertRaises(ValueError):
                planner.parse_uris(record)

    def test_local_mirror_alias_pins_an_eligible_official_url_and_reads_once(self):
        alias = "mirror+file:/etc/apt/apt-mirrors.txt/pool/universe/o/openbox/openbox_1.0-1_amd64.deb"
        listing = ("# Existing image configuration\n\n"
                   "https://attacker.invalid/ubuntu/\tpriority:0\n"
                   "https://security.ubuntu.com/ubuntu/\tpriority:0 type:index\n"
                   "https://archive.ubuntu.com/ubuntu/\tpriority:0 arch:arm64\n"
                   "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
                   "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1 arch:amd64 arch:all type:deb\n")
        second = alias.replace("openbox_1.0-1_amd64.deb", "openbox_1.0-2_amd64.deb")
        records = RECORD.replace(URI, alias) + "\n" + RECORD.replace(URI, second).replace(
            "' openbox_1.0-1_amd64.deb ", "' openbox_1.0-2_amd64.deb ")
        with patch.object(planner.Path, "read_text", return_value=listing) as read:
            packages = planner.parse_uris(records)
        read.assert_called_once_with(encoding="utf-8")
        self.assertEqual(packages[0]["url"], URI)
        self.assertEqual(packages[0]["apt_uri"], alias)
        self.assertEqual(packages[0]["mirror_file"], "/etc/apt/apt-mirrors.txt")
        self.assertEqual(packages[1]["url"], URI.replace("1.0-1", "1.0-2"))

    def test_mirror_alias_cannot_read_an_arbitrary_file_or_accept_a_partial_mirror(self):
        alias = "mirror+file:/etc/apt/apt-mirrors.txt/pool/universe/o/openbox/openbox_1.0-1_amd64.deb"
        for uri in (alias.replace("apt-mirrors.txt", "credentials.txt"),
                    alias.replace("mirror+file:", "mirror+file://server"),
                    alias + "?other=true", alias + "#fragment",
                    alias.replace("/pool/", "/outside/")):
            with self.subTest(uri=uri), patch.object(planner.Path, "read_text") as read:
                with self.assertRaises(ValueError):
                    planner.parse_uris(RECORD.replace(URI, uri))
                read.assert_not_called()
        for metadata in ("type:index", "arch:arm64", "component:restricted", "suite:noble"):
            listing = "https://archive.ubuntu.com/ubuntu/\t" + metadata
            with self.subTest(metadata=metadata), patch.object(planner.Path, "read_text", return_value=listing):
                with self.assertRaises(ValueError):
                    planner.parse_uris(RECORD.replace(URI, alias))
        with patch.object(planner.Path, "read_text", return_value="https://attacker.invalid/ubuntu/"):
            with self.assertRaises(ValueError):
                planner.parse_uris(RECORD.replace(URI, alias))

    def test_real_runner_mirror_records_preserve_all_filenames_sizes_and_hashes(self):
        output = Path(__file__).with_name("fixtures").joinpath("apt-print-uris-mirrors.txt").read_text()
        with patch.object(planner.Path, "read_text", return_value="https://archive.ubuntu.com/ubuntu/"):
            packages = planner.parse_uris(output)
        self.assertEqual(len(packages), 32)
        self.assertEqual(sum(package["size_bytes"] for package in packages), 24529196)
        self.assertTrue(all(package["apt_uri"].startswith("mirror+file:") for package in packages))
        self.assertTrue(all(package["url"].startswith("https://archive.ubuntu.com/ubuntu/pool/")
                            for package in packages))
        self.assertTrue(any("%3a" in package["filename"] for package in packages))

    def test_official_host_does_not_allow_parent_pool_traversal(self):
        for path in ("/ubuntu/pool/../other/", "/ubuntu/pool/%2e%2e/other/"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                planner.parse_uris(RECORD.replace("/ubuntu/pool/", path))

    def collect(self, result, statuses=(b"installed state", b"installed state")):
        with patch.object(planner.Path, "read_bytes", side_effect=statuses), \
                patch.object(planner.Path, "read_text", return_value="ID=ubuntu\nVERSION_ID=24.04\n"), \
                patch.object(planner.subprocess, "run", side_effect=result) as run, \
                contextlib.redirect_stderr(io.StringIO()):
            plan = planner.collect_plan()
        return plan, run

    def test_collect_emits_full_plan_and_checks_no_package_or_database_writes(self):
        def apt(command, **kwargs):
            self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
            self.assertEqual(kwargs["env"]["LC_ALL"], "C")
            self.assertNotIn("shell", kwargs)
            cache = next(value.split("=", 1)[1] for value in command if value.startswith("Dir::Cache::archives="))
            self.assertEqual(list(Path(cache).iterdir()), [])
            return SimpleNamespace(returncode=0, stdout=RECORD + "\n" + DEPENDENCY, stderr="")
        plan, run = self.collect(apt)
        run.assert_called_once()
        self.assertEqual(plan["requested_packages"], ["openbox", "xdg-desktop-portal", "xdg-desktop-portal-gtk"])
        self.assertEqual(plan["package_count"], 2)
        self.assertEqual(plan["total_size_bytes"], 1333)
        self.assertEqual(plan["proof"]["downloaded_deb_files"], 0)
        self.assertTrue(plan["proof"]["dpkg_status_unchanged"])
        self.assertEqual(len(plan["proof"]["dpkg_status_sha256"]), 64)

    def test_already_satisfied_packages_need_no_archives(self):
        plan, _ = self.collect(lambda *args, **kwargs: SimpleNamespace(
            returncode=0, stdout="0 upgraded, 0 newly installed, 0 to remove.\n", stderr=""))
        self.assertEqual(plan["packages"], [])
        self.assertEqual(plan["total_size_bytes"], 0)

    def test_resolver_error_or_changed_database_cannot_produce_success(self):
        for result, statuses in (
            (SimpleNamespace(returncode=100, stdout="", stderr="unmet dependencies"), (b"old", b"old")),
            (SimpleNamespace(returncode=0, stdout=RECORD, stderr=""), (b"old", b"new")),
        ):
            with self.subTest(result=result, statuses=statuses), self.assertRaises(ValueError):
                self.collect(lambda *args, **kwargs: result, statuses)

    def test_unexpected_archive_is_a_failure(self):
        def apt(command, **kwargs):
            cache = next(value.split("=", 1)[1] for value in command if value.startswith("Dir::Cache::archives="))
            (Path(cache) / "unexpected.deb").touch()
            return SimpleNamespace(returncode=0, stdout=RECORD, stderr="")
        with self.assertRaisesRegex(ValueError, "unexpectedly wrote"):
            self.collect(apt)

    def test_timeout_cannot_produce_success(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            self.collect(lambda *args, **kwargs: (_ for _ in ()).throw(subprocess.TimeoutExpired("apt-get", 180)))

    def test_json_in_log_and_summary_is_identical(self):
        plan = {"packages": planner.parse_uris(RECORD), "total_size_bytes": 1234}
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()) as log:
            summary = Path(directory) / "summary.md"
            summary.write_text("Earlier summary\n", encoding="utf-8")
            planner.publish(plan, summary)
            rendered = summary.read_text(encoding="utf-8")
        self.assertEqual(json.loads(log.getvalue()), plan)
        self.assertIn("```json\n" + log.getvalue() + "```", rendered)
        self.assertTrue(rendered.startswith("Earlier summary\n"))

    def test_workflow_has_manual_only_plan_and_keeps_native_and_census_guards(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/extensions.yml").read_text()
        job = workflow.split("\n  native-prerequisites:\n", 1)[1].split("\n  census:\n", 1)[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch' && inputs.mode == 'native-prerequisites'", job)
        self.assertIn("runs-on: ubuntu-24.04", job)
        self.assertIn("persist-credentials: false", job)
        self.assertIn('native-prerequisites.py --summary "$GITHUB_STEP_SUMMARY"', job)
        self.assertIn("APT::Update::Error-Mode=any update", job)
        for forbidden in ("apt-get install", "cargo ", "setup-node", "rust-toolchain", "rust-cache",
                          "extension-census", "upload-artifact", "secrets.", "token:"):
            self.assertNotIn(forbidden, job)
        self.assertIn("if: github.event_name == 'release' || inputs.mode == 'census'", workflow)
        self.assertIn("if: github.event_name == 'workflow_dispatch' && inputs.mode == 'native-verification'", workflow)


if __name__ == "__main__":
    unittest.main()
