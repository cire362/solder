#!/usr/bin/env python3
"""Contract checks for the evidence harness; these are NOT native UI results."""

import copy
import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True


spec = importlib.util.spec_from_file_location("native_suite", Path(__file__).with_name("verify-native-notebook.py"))
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)
spec = importlib.util.spec_from_file_location("native_ci", Path(__file__).with_name("verify-native-ci.py"))
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.book = suite.read_json(suite.ROOT / "verification/fixtures/native-window.ipynb")

    def test_preservation_oracle_accepts_source_only_edits(self):
        saved = copy.deepcopy(self.book)
        saved["cells"][0]["source"] = "edited"
        suite.assert_book(saved, self.book, "edited")

    def test_preservation_oracle_rejects_loss_and_wrong_input(self):
        for location in ("metadata", "outputs", "execution_count", "id"):
            with self.subTest(location=location):
                damaged = copy.deepcopy(self.book)
                damaged["cells"][0]["source"] = "edited"
                damaged["cells"][0].pop(location)
                with self.assertRaises(AssertionError):
                    suite.assert_book(damaged, self.book, "edited")
        for mutate in (
            lambda book: book.pop("custom_top_level"),
            lambda book: book["cells"][1].pop("attachments"),
            lambda book: book["cells"].reverse(),
            lambda book: book["cells"].pop(),
        ):
            damaged = copy.deepcopy(self.book)
            mutate(damaged)
            with self.assertRaises(AssertionError):
                suite.assert_book(damaged, self.book, "edited")
        with self.assertRaises(AssertionError):
            suite.assert_book(self.book, self.book, "wrong input")

    def test_empty_session_matches_the_current_rust_serialization(self):
        with tempfile.TemporaryDirectory() as directory:
            config, root = Path(directory) / "config", Path(directory) / "project"
            suite.seed_empty_session(config, root)
            windows = suite.read_json(config / "sessions/windows.json")
            self.assertEqual(windows, {"version": 1, "open": [], "recent": []})
            session = suite.read_json(config / f"sessions/{suite.session_name(root)}.json")
            self.assertEqual(session["root"], str(root))
            self.assertEqual(session["panes"], [[]])
            self.assertEqual(session["books"], [])
            self.assertEqual(session["front_book"], None)
            self.assertEqual(suite.session_name(Path("a")), "af63dc4c8601ec8c")

    def test_recovery_evidence_requires_this_path_and_exact_source(self):
        with tempfile.TemporaryDirectory() as directory:
            config, path = Path(directory), Path(directory) / "book.ipynb"
            book = copy.deepcopy(self.book)
            book["cells"][0]["source"] = "recovered"
            copy_path = config / "sessions/recovery/project/native.book.json"
            suite.write_json(copy_path, {"path": str(path), "snapshot": {"content": book}})
            self.assertEqual(len(suite.recovery_files(config, path, "recovered")), 1)
            self.assertEqual(len(suite.recovery_files(config, path)), 1)
            self.assertEqual(suite.recovery_files(config, path, "wrong"), [])
            self.assertEqual(suite.recovery_files(config, path.with_name("other.ipynb"), "recovered"), [])
            copy_path.write_text("{broken", encoding="utf-8")
            self.assertEqual(suite.recovery_files(config, path, "recovered"), [])

    def test_canonical_paths_match_native_files_and_resolve_aliases(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "native.txt"
            file.write_text("fixture", encoding="utf-8")
            canonical = suite.INPUT.canonical_path(file)
            self.assertEqual(canonical.read_text(encoding="utf-8"), "fixture")
            if suite.os.name == "nt":
                self.assertTrue(str(canonical).startswith("\\\\?\\"))
            else:
                alias = file.with_name("alias.txt")
                alias.symlink_to(file)
                self.assertEqual(suite.INPUT.canonical_path(alias), canonical)

    def test_wait_handles_a_session_file_not_yet_published(self):
        instance = suite.Suite(None, None, None, None)
        calls = []
        def published():
            calls.append(True)
            if len(calls) == 1:
                raise FileNotFoundError("session writer has not published yet")
            return "ready"
        self.assertEqual(instance.wait("published session", published, seconds=1), "ready")
        self.assertEqual(len(calls), 2)

    def test_partial_and_failed_results_cannot_be_reported_as_passed(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = suite.Evidence(Path(directory))
            self.assertEqual(evidence.exit_code(), 2)
            evidence.result(suite.CHECKS[0], "passed", "fixture test only")
            evidence.block_remaining("platform unavailable")
            self.assertEqual(evidence.exit_code(), 2)
            self.assertEqual(evidence.report["checks"][suite.CHECKS[0]]["status"], "passed")
            evidence.result(suite.CHECKS[1], "failed", "wrong file")
            self.assertEqual(evidence.exit_code(), 1)
            for name in suite.CHECKS:
                evidence.result(name, "passed", "fixture test only")
            self.assertEqual(evidence.exit_code(), 0)

    def test_software_driver_selection_resolves_library_beside_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            broken, mesa, chrome = [root / name / "driver.json" for name in ("broken", "mesa", "chrome")]
            suite.write_json(broken, {"ICD": {"library_path": "./missing.so"}})
            suite.write_json(mesa, {"ICD": {"library_path": "libvulkan_lvp.so"}})
            suite.write_json(chrome, {"ICD": {"library_path": "./libvk_swiftshader.so"}})
            (chrome.parent / "libvk_swiftshader.so").touch()
            env = {"VK_DRIVER_FILES": "old.json", "VK_ICD_FILENAMES": "older.json", "ZED_DEVICE_ID": "1234"}
            result = ci.configure_software_vulkan(env, [root / "absent.json", broken, mesa, chrome])
            self.assertEqual(result["selected"], str(mesa.resolve()))
            self.assertIn("missing", result["candidates"][1]["error"])
            self.assertEqual(env["VK_DRIVER_FILES"], str(mesa.resolve()))
            self.assertEqual(env["VK_ICD_FILENAMES"], str(mesa.resolve()))
            self.assertNotIn("ZED_DEVICE_ID", env)
            result = ci.configure_software_vulkan({}, [broken, chrome])
            self.assertEqual(result["selected"], str(chrome.resolve()))
            self.assertEqual(result["candidates"][1]["library"], str((chrome.parent / "libvk_swiftshader.so").resolve()))

    def test_missing_or_malformed_driver_keeps_unavailable_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            broken = root / "broken.json"
            broken.write_text("{broken", encoding="utf-8")
            env = {"DISPLAY": ":17", "ZED_DEVICE_ID": "1234"}
            with patch.object(ci, "software_vulkan_manifests", return_value=[broken]), \
                    patch.object(ci, "diagnostic_command", return_value=None):
                ci.gpu_diagnostics(root, env, True)
            report = suite.read_json(root / "gpu-diagnostics.json")
            self.assertIsNone(report["selected"])
            self.assertIn("error", report["candidates"][0])
            self.assertIn("No preinstalled", env["SOLDER_NATIVE_GPU_UNAVAILABLE"])
            self.assertNotIn("VK_DRIVER_FILES", env)
            self.assertEqual(report["incoming_environment"]["DISPLAY"], ":17")
            self.assertEqual(report["effective_environment"]["VK_LOADER_DEBUG"], "error,warn,driver")

    def test_existing_desktop_does_not_force_a_software_driver(self):
        with tempfile.TemporaryDirectory() as directory:
            env = {"VK_DRIVER_FILES": "/existing.json", "VK_LOADER_DEBUG": "all"}
            with patch.object(ci, "configure_software_vulkan") as select, \
                    patch.object(ci, "diagnostic_command", return_value=None):
                ci.gpu_diagnostics(Path(directory), env, False)
            select.assert_not_called()
            self.assertEqual(env["VK_DRIVER_FILES"], "/existing.json")
            self.assertEqual(env["VK_LOADER_DEBUG"], "all")

    def test_selected_driver_retains_default_and_selected_probe_results(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "driver.json"
            suite.write_json(manifest, {"ICD": {"library_path": "libvulkan_lvp.so"}})
            env = {"DISPLAY": ":17", "VK_DRIVER_FILES": "previous.json"}
            calls = []
            def diagnostic(out, name, command, effective, timeout=20):
                calls.append((name, effective.copy()))
                return 0
            with patch.object(ci, "software_vulkan_manifests", return_value=[manifest]), \
                    patch.object(ci, "diagnostic_command", side_effect=diagnostic):
                ci.gpu_diagnostics(root, env, True)
            probes = dict(calls)
            self.assertEqual(probes["vulkan-probe-default.log"]["VK_DRIVER_FILES"], "previous.json")
            self.assertEqual(probes["vulkan-probe.log"]["VK_DRIVER_FILES"], str(manifest.resolve()))
            self.assertEqual(suite.read_json(root / "gpu-diagnostics.json")["probe_exit_code"], 0)
            self.assertFalse((root / "report.json").exists())

    def test_missing_gpu_stops_before_build_and_blocks_all_native_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            argv = ["verify-native-notebook.py", "--out", str(root), "--build"]
            env = {"GITHUB_ACTIONS": "true", "RUNNER_TEMP": str(root),
                   "SOLDER_NATIVE_GPU_UNAVAILABLE": "No preinstalled software driver"}
            with patch.object(suite.sys, "platform", "linux"), patch.object(suite.sys, "argv", argv), \
                    patch.dict(suite.os.environ, env), patch.object(suite, "inventory"), \
                    patch.object(suite, "build") as build, patch.object(suite.INPUT, "X11") as desktop, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(suite.main(), 2)
            build.assert_not_called()
            desktop.assert_not_called()
            report = suite.read_json(root / "report.json")
            self.assertTrue(all(check["status"] == "blocked" for check in report["checks"].values()))

    def test_failed_diagnostics_preserve_logs_without_claiming_ui_success(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            with patch.object(ci.subprocess, "run", side_effect=FileNotFoundError("no compiler")):
                self.assertIsNone(ci.diagnostic_command(out, "probe.log", ["cc"], {}))
            self.assertIn("Diagnostic unavailable: no compiler", (out / "probe.log").read_text())
            self.assertFalse((out / "report.json").exists())

    def test_only_initial_unsupported_gpu_is_unavailable_not_other_process_deaths(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            instance = suite.Suite(None, suite.Evidence(out), None, None)
            instance.launch_number = 1
            instance.process = SimpleNamespace(poll=lambda: 101, returncode=101)
            log = out / "process-1.log"
            gpu_error = ("Failed to initialize X11 client.\nUnable to init GPU context\nNoSupportedDeviceFound")
            with patch.object(suite.sys, "platform", "linux"):
                for output in ("unrelated application panic", "NoSupportedDeviceFound"):
                    log.write_text(output, encoding="utf-8")
                    with self.assertRaises(AssertionError):
                        instance.wait("an OS window", lambda: None, seconds=1)
                log.write_text(gpu_error, encoding="utf-8")
                with self.assertRaises(suite.INPUT.Unavailable):
                    instance.wait("an OS window", lambda: None, seconds=1)
                instance.window = 42
                with self.assertRaises(AssertionError):
                    instance.wait("saved input", lambda: None, seconds=1)
                instance.window, instance.launch_number = None, 2
                with self.assertRaises(AssertionError):
                    instance.wait("an OS window", lambda: None, seconds=1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
