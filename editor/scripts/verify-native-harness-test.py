#!/usr/bin/env python3
"""Contract checks for the evidence harness; these are NOT native UI results."""

import copy
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True


spec = importlib.util.spec_from_file_location("native_suite", Path(__file__).with_name("verify-native-notebook.py"))
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)


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


if __name__ == "__main__":
    unittest.main(verbosity=2)
