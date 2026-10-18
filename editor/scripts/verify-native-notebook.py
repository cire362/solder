#!/usr/bin/env python3
"""First-party, disposable-runner native notebook/CLI verification.

Example (an existing platform binary; never uses a parent's target directory):
  python scripts/verify-native-notebook.py --binary /path/to/solder --out /runner/temp/evidence

--build attempts one offline Cargo ci-profile build in the evidence directory.
There is no installation, download, third-party extension, or headless fallback.
Exit 0 means all native checks passed, 1 means failed, 2 means unavailable/partial.
JSON, screenshots, original/saved/exported bytes, process logs and settings/session
snapshots are retained on every path. Abrupt process death is not power loss.
"""

import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time
import traceback
import uuid


# Imported harness modules must not leave bytecode in the source checkout.
sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("native_input", ROOT / "scripts/verify-native-input.py")
INPUT = importlib.util.module_from_spec(spec)
spec.loader.exec_module(INPUT)
CHECKS = ("cli-empty-session-foreground", "notebook-edit-save", "notebook-recovery-written",
          "notebook-recovery-restored", "notebook-export-copy", "notebook-recovery-save")


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write_json(path, value):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def source(book):
    value = book["cells"][0]["source"]
    return "".join(value) if isinstance(value, list) else value


def assert_book(book, original, expected_source):
    expected = copy.deepcopy(original)
    expected["cells"][0]["source"] = expected_source
    actual = copy.deepcopy(book)
    actual["cells"][0]["source"] = source(book)
    if actual != expected:
        raise AssertionError("Notebook content differs: edited source, cell count/order, IDs, "
                             "execution count, metadata, attachments, outputs or unknown fields")


def session_name(root):
    # session.rs::name_of, over the canonical platform path's UTF-8 bytes.
    value = 0xCBF29CE484222325
    for byte in str(root).encode("utf-8"):
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def seed_empty_session(config, root):
    write_json(config / "sessions/windows.json", {"version": 1, "open": [], "recent": []})
    write_json(config / f"sessions/{session_name(root)}.json", {
        "version": 1, "root": str(root), "panes": [[]], "active": [None],
        "active_pane": 0, "books": [], "pages": [], "front_book": None,
        "front_page": None, "viewers": [[]]})


class Evidence:
    def __init__(self, out):
        self.out = out
        self.report = {
            "schema": 1, "platform": platform.platform(), "python": sys.version,
            "workflow_sha": os.environ.get("GITHUB_SHA"), "run_id": os.environ.get("GITHUB_RUN_ID"),
            "display_kind": os.environ.get("SOLDER_NATIVE_DISPLAY_KIND", "existing-platform-desktop"),
            "harness_contract_exit_code": os.environ.get("SOLDER_NATIVE_HARNESS_TEST_EXIT_CODE"),
            "checks": {name: {"status": "not-run", "kind": "native-window"} for name in CHECKS},
            "limitations": ["No physical display/GPU or power-loss durability claim",
                            "No published extension, kernel execution or independent notebook views",
                            "X11 only on Linux; Wayland is not exercised"],
            "events": [],
        }

    def event(self, name, **details):
        self.report["events"].append({"at": time.time(), "event": name, **details})
        self.flush()

    def result(self, name, status, detail):
        self.report["checks"][name].update(status=status, detail=detail)
        self.flush()

    def block_remaining(self, reason):
        for check in self.report["checks"].values():
            if check["status"] == "not-run":
                check.update(status="blocked", detail=reason)

    def flush(self):
        write_json(self.out / "report.json", self.report)
        lines = [f"Native verification: {self.report['platform']}", ""]
        for name, check in self.report["checks"].items():
            lines.append(f"- {name}: **{check['status']}**. {check.get('detail', '')}")
        lines += ["", "Limitations:"] + [f"- {item}" for item in self.report["limitations"]]
        if self.report["harness_contract_exit_code"] is not None:
            lines += ["", "Harness fixture contracts exit code: " + self.report["harness_contract_exit_code"]
                      + ". These are not native UI checks."]
        lines += ["", "See report.json, OS screenshots, process logs and retained state for evidence."]
        (self.out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")

    def exit_code(self):
        if any(name in self.report for name in ("cleanup_error", "desktop_cleanup_error", "profile_cleanup_error")):
            return 1
        states = [check["status"] for check in self.report["checks"].values()]
        return 1 if "failed" in states else 0 if all(state == "passed" for state in states) else 2


def inventory(evidence):
    tools = ("cargo", "rustc", "pkg-config", "Xvfb", "dbus-run-session", "gdbus", "powershell")
    evidence.report["tools"] = {name: shutil.which(name) for name in tools}
    if shutil.which("git"):
        revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                  capture_output=True, text=True, timeout=20)
        evidence.report["checkout_commit"] = revision.stdout.strip()
    evidence.report["source_hashes"] = {
        str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (ROOT / "Cargo.lock", ROOT / "crates/solder/src/main.rs",
                     ROOT / "crates/solder/src/session.rs", ROOT / "crates/solder/src/notebook.rs")
    }
    for tool in ("cargo", "rustc"):
        if shutil.which(tool):
            result = subprocess.run([tool, "--version"], capture_output=True, text=True, timeout=20)
            evidence.report[tool] = result.stdout.strip() or result.stderr.strip()
    if sys.platform.startswith("linux"):
        gpu = evidence.out / "gpu-diagnostics.json"
        if gpu.is_file():
            evidence.report["linux_gpu"] = read_json(gpu)
        desktop = evidence.out / "x11-desktop.json"
        if desktop.is_file():
            evidence.report["linux_desktop"] = read_json(desktop)
        modules = ("xkbcommon", "xkbcommon-x11", "wayland-client", "vulkan", "fontconfig",
                   "freetype2", "alsa", "libzstd", "webkit2gtk-4.1")
        evidence.report["linux_system_libraries"] = {}
        for module in modules:
            found = bool(shutil.which("pkg-config")) and subprocess.run(
                ["pkg-config", "--exists", module], capture_output=True).returncode == 0
            evidence.report["linux_system_libraries"][module] = found
    evidence.event("runner-inventory")


def build(evidence):
    target = evidence.out / "cargo-target"
    command = ["cargo", "build", "--offline", "--locked", "--profile", "ci", "-p", "solder",
               "--bin", "solder", "--target-dir", str(target)]
    evidence.event("offline-build-start", command=command)
    if not shutil.which("cargo"):
        raise INPUT.Unavailable("No preinstalled Cargo; a prebuilt platform binary is required")
    if sys.platform.startswith("linux"):
        missing = [name for name, found in evidence.report["linux_system_libraries"].items() if not found]
        if missing:
            raise INPUT.Unavailable("Build prerequisites absent (no packages installed): " + ", ".join(missing))
    with (evidence.out / "build.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, timeout=3600)
    evidence.report["build_exit_code"] = result.returncode
    evidence.event("offline-build-finished", exit_code=result.returncode)
    if result.returncode:
        raise RuntimeError("Platform build failed; inspect build.log. "
                           "Workflow provisioning must populate the locked Cargo dependencies first")
    return target / "ci" / ("solder.exe" if os.name == "nt" else "solder")


def recovery_files(config, path, wanted=None):
    found = []
    for file in (config / "sessions/recovery").glob("**/*.json"):
        try:
            kept = read_json(file)
            if (kept.get("path") == str(path) and "snapshot" in kept
                    and (wanted is None or source(kept["snapshot"]["content"]) == wanted)):
                found.append((file, kept))
        except (OSError, ValueError, KeyError, TypeError, IndexError):
            continue
    return found


class Suite:
    def __init__(self, binary, evidence, config, ui):
        self.binary, self.evidence, self.config, self.ui = binary, evidence, config, ui
        self.process = None
        self.window = None
        self.logs = []
        self.launch_number = 0

    def wait(self, what, predicate, seconds=30):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.process is not None and self.process.poll() is not None:
                detail = (f"Solder exited with {self.process.returncode} while waiting for {what}; "
                          "inspect process logs and retained Solder journal")
                if sys.platform.startswith("linux") and self.launch_number == 1 and self.window is None:
                    log = self.evidence.out / "process-1.log"
                    output = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""
                    if all(message in output for message in (
                            "Failed to initialize X11 client.", "Unable to init GPU context", "NoSupportedDeviceFound")):
                        raise INPUT.Unavailable(detail + "; no supported Vulkan device at startup; "
                                                "inspect gpu-diagnostics.json and vulkan-probe.log")
                raise AssertionError(detail)
            try:
                result = predicate()
            except FileNotFoundError:
                # Session writers may not have published the first file yet.
                result = False
            if result:
                return result
            time.sleep(0.1)
        raise AssertionError(f"Timed out waiting for {what}")

    def launch(self, argument=None):
        self.launch_number += 1
        log = (self.evidence.out / f"process-{self.launch_number}.log").open("wb")
        self.logs.append(log)
        command = [str(self.binary)] + ([str(argument)] if argument else [])
        self.process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        self.evidence.event("launch", command=command, pid=self.process.pid)
        self.window = self.wait("an OS window", lambda: next(iter(self.ui.windows(self.process.pid)), None))
        # Never set foreground/focus ourselves: CLI selection must stand on its own.
        self.wait("Solder to select the foreground", lambda: self.ui.owns_focus(self.window, self.process.pid))
        self.evidence.event("native-window", handle=int(self.window), pid=self.process.pid,
                            foreground=int(self.ui.foreground()), backend=self.ui.backend)
        time.sleep(1)  # Let the first OS frames and asynchronous notebook reader finish.
        self.capture(f"launch-{self.launch_number}")

    def capture(self, name):
        path = self.ui.screenshot(self.evidence.out / name, self.window)
        self.evidence.event("os-screenshot", path=Path(path).name)

    def keys(self, *names):
        if not self.ui.owns_focus(self.window, self.process.pid):
            raise AssertionError("Solder lost the OS foreground before input")
        self.ui.chord(*names)
        self.evidence.event("os-key", chord=list(names))

    def replace(self, value):
        self.keys("CTRL", "a")
        self.ui.text(value)
        self.evidence.event("os-text", text=value)

    def kill(self):
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=15)
            self.evidence.event("abrupt-process-termination", pid=self.process.pid,
                                exit_code=self.process.returncode)
        self.process = None

    def copy_state(self, name):
        destination = self.evidence.out / name
        destination.mkdir()
        # Copy published state, not staged sibling files which may disappear
        # between enumeration and opening during an atomic write.
        for file in self.config.rglob("*"):
            if file.is_file() and file.suffix in (".json", ".log"):
                target = destination / file.relative_to(self.config)
                target.parent.mkdir(parents=True, exist_ok=True)
                try:
                    target.write_bytes(file.read_bytes())
                except FileNotFoundError:
                    continue
        self.evidence.event("state-snapshot", path=name)

    def cli(self):
        root = self.evidence.out / "cli-project"
        root.mkdir()
        path = root / "foreground.txt"
        path.write_text("initial", encoding="utf-8")
        seed_empty_session(self.config, INPUT.canonical_path(root))
        self.copy_state("cli-empty-session-before")
        self.launch(path)
        # No click or focus operation precedes the edit.
        self.replace("native_cli_foreground")
        self.keys("CTRL", "s")
        self.wait("CLI argument's saved input", lambda: path.read_text(encoding="utf-8") == "native_cli_foreground")
        self.capture("cli-saved")
        self.evidence.result(CHECKS[0], "passed", "OS foreground and real input selected the CLI file from an empty session; saved bytes match")
        self.keys("CTRL", "q")
        self.process.wait(timeout=30)
        if self.process.returncode != 0:
            raise AssertionError(f"Clean CLI quit exited with {self.process.returncode}")
        self.process = None

    def export(self, path, original, expected):
        destination = path.with_name("book.copy.ipynb")
        before = path.read_bytes()
        self.keys("CTRL", "ALT", "s")  # workspace::SaveAs -> notebook::ExportCopy
        try:
            dialog = self.wait("an OS save dialog", lambda: self.ui.save_dialog(self.process.pid), seconds=15)
        except AssertionError:
            raise INPUT.Unavailable("No foreground OS Save File dialog. Linux needs a running "
                                    "org.freedesktop.portal.FileChooser; no portal is installed by this harness")
        try:
            shot = self.ui.screenshot(self.evidence.out / "platform-save-dialog", dialog)
            self.evidence.event("platform-save-dialog", handle=int(dialog), screenshot=Path(shot).name)
            # The application supplies book.copy.ipynb in the source's directory.
            # Accept the actual platform dialog, without simulating a GPUI response.
            self.ui.chord("ENTER")
            self.wait("the exported copy", destination.exists)
            assert_book(read_json(destination), original, expected)
            if path.read_bytes() != before:
                raise AssertionError("Export copy modified its source")
            self.wait("export to return the foreground", lambda: self.ui.owns_focus(self.window, self.process.pid))
        finally:
            if self.ui.foreground() == dialog:
                self.ui.chord("ESC")
                self.wait("save dialog cancellation", lambda: self.ui.owns_focus(self.window, self.process.pid))
        self.capture("notebook-exported")
        if not recovery_files(self.config, path, expected):
            raise AssertionError("Export copy discarded the source notebook's recovery")
        self.evidence.result(CHECKS[4], "passed", "Platform Save File dialog produced the recovered copy; all fields preserved, source bytes unchanged and recovery retained")

    def notebook(self, output_bytes):
        root = self.evidence.out / "notebook-project"
        root.mkdir()
        path = root / "book.ipynb"
        original = read_json(ROOT / "verification/fixtures/native-window.ipynb")
        original["cells"][0]["outputs"][0]["text"] = ["first-party output\n" + "x" * output_bytes + "\n"]
        write_json(path, original)
        path = INPUT.canonical_path(path)
        root = path.parent
        write_json(self.evidence.out / "notebook-original.json", original)
        self.evidence.report["stream_output_bytes"] = output_bytes
        seed_empty_session(self.config, root)
        self.launch(path)
        saved = "# native_saved"
        self.replace(saved)
        self.keys("CTRL", "s")
        self.wait("native notebook save", lambda: source(read_json(path)) == saved)
        assert_book(read_json(path), original, saved)
        shutil.copyfile(path, self.evidence.out / "notebook-saved.ipynb")
        self.capture("notebook-saved")
        self.evidence.result(CHECKS[1], "passed", "OS input edited the first cell; native save retained outputs, attachments, metadata and unknown fields")
        recovered = "# native_recovered"
        self.replace(recovered)
        files = self.wait("durable notebook recovery bytes", lambda: recovery_files(self.config, path, recovered))
        assert_book(files[0][1]["snapshot"]["content"], original, recovered)
        if source(read_json(path)) != saved:
            raise AssertionError("Unsaved notebook edits reached the source file")
        self.copy_state("before-process-death")
        self.capture("notebook-unsaved")
        self.evidence.result(CHECKS[2], "passed", "Unsaved native edits and full outputs reached recovery storage while source stayed saved")
        # Ensure the real window/session writers have published the notebook tab.
        session = self.config / f"sessions/{session_name(root)}.json"
        self.wait("persisted notebook session", lambda: any(book.get("path") == str(path) for book in read_json(session).get("books", [])))
        self.wait("persisted project window", lambda: any(
            window.get("root") == str(root)
            for window in read_json(self.config / "sessions/windows.json").get("open", [])))
        self.kill()
        self.copy_state("after-process-death")
        self.launch()  # Restore actual windows/tabs/recovery, no CLI notebook argument.
        self.keys("CTRL", "END")
        self.ui.text("_after_restart")
        self.evidence.event("os-text", text="_after_restart")
        restored = recovered + "_after_restart"
        files = self.wait("input applied to restored cell", lambda: recovery_files(self.config, path, restored))
        assert_book(files[0][1]["snapshot"]["content"], original, restored)
        self.capture("notebook-restored")
        self.copy_state("after-recovery")
        self.evidence.result(CHECKS[3], "passed", "Restart with no CLI argument restored the unsaved cell; real OS input appended to it and full recovery snapshot matches")
        try:
            self.export(path, original, restored)
        except INPUT.Unavailable as error:
            self.evidence.result(CHECKS[4], "blocked", str(error))
        self.keys("CTRL", "s")
        self.wait("save of the recovered notebook", lambda: source(read_json(path)) == restored)
        assert_book(read_json(path), original, restored)
        self.wait("recovery cleanup", lambda: not recovery_files(self.config, path))
        shutil.copyfile(path, self.evidence.out / "notebook-recovered-saved.ipynb")
        self.capture("notebook-recovered-saved")
        self.evidence.result(CHECKS[5], "passed", "Native save wrote the restored cell and preserved every fixture field; matching recovery copies were removed")
        self.keys("CTRL", "q")
        self.process.wait(timeout=30)
        if self.process.returncode != 0:
            raise AssertionError(f"Clean notebook quit exited with {self.process.returncode}")
        self.process = None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--binary", type=Path)
    mode.add_argument("--build", action="store_true")
    parser.add_argument("--large-output-bytes", type=int, default=262144)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    evidence = Evidence(out)
    suite, ui, config, marker = None, None, None, None
    active = CHECKS[0]
    try:
        inventory(evidence)
        if not (os.name == "nt" or sys.platform.startswith("linux")):
            raise INPUT.Unavailable("Only Linux X11 and Windows are covered by this harness")
        if not 0 <= args.large_output_bytes <= 4 * 1024 * 1024:
            raise ValueError("Large output size must be between 0 and 4 MiB")
        # dirs 7 uses the Windows Known Folder profile, not a USERPROFILE override.
        # Use the disposable runner's real profile on both systems; never overwrite
        # an existing Solder profile, and never run this on a developer's machine.
        if os.environ.get("GITHUB_ACTIONS") != "true" or not os.environ.get("RUNNER_TEMP"):
            raise INPUT.Unavailable("Native execution requires a disposable GitHub Actions runner")
        if not out.is_relative_to(Path(os.environ["RUNNER_TEMP"]).resolve()):
            raise INPUT.Unavailable("Evidence/build output must be inside RUNNER_TEMP")
        if sys.platform.startswith("linux") and os.environ.get("SOLDER_NATIVE_GPU_UNAVAILABLE"):
            raise INPUT.Unavailable(os.environ["SOLDER_NATIVE_GPU_UNAVAILABLE"])
        if sys.platform.startswith("linux") and os.environ.get("SOLDER_NATIVE_DESKTOP_UNAVAILABLE"):
            raise INPUT.Unavailable(os.environ["SOLDER_NATIVE_DESKTOP_UNAVAILABLE"])
        binary = build(evidence) if args.build else args.binary.resolve()
        if not binary.is_file():
            raise INPUT.Unavailable(f"No platform binary at {binary}")
        evidence.report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        ui = INPUT.Windows() if os.name == "nt" else INPUT.X11()
        config = Path.home() / ".config/solder"
        try:
            config.mkdir(parents=True, exist_ok=False)
        except FileExistsError:
            raise INPUT.Unavailable("The runner already has a Solder profile; refusing to overwrite it")
        marker = uuid.uuid4().hex
        (config / ".native-verification-owner").write_text(marker, encoding="ascii")
        write_json(config / "settings.json", {"language": "en", "theme": "dark", "format_on_save": False,
                                              "show_performance_hud": False})
        suite = Suite(binary, evidence, config, ui)
        suite.cli()
        active = CHECKS[1]
        suite.notebook(args.large_output_bytes)
    except INPUT.Unavailable as error:
        evidence.block_remaining(str(error))
        evidence.event("unavailable", reason=str(error))
    except Exception as error:
        # Mark the first unfinished stage so partial native successes remain visible.
        active = next((name for name in CHECKS if evidence.report["checks"][name]["status"] == "not-run"), active)
        evidence.result(active, "failed", str(error))
        evidence.block_remaining("Earlier native stage failed; inspect harness-error.log")
        (out / "harness-error.log").write_text(traceback.format_exc(), encoding="utf-8")
    finally:
        if suite:
            try:
                suite.kill()
                suite.copy_state("final-state")
            except Exception as error:
                evidence.report["cleanup_error"] = str(error)
            for log in suite.logs:
                log.close()
        if ui:
            try:
                ui.finish()
            except Exception as error:
                evidence.report["desktop_cleanup_error"] = str(error)
        if config and marker:
            try:
                if (config / ".native-verification-owner").read_text(encoding="ascii") == marker:
                    shutil.rmtree(config)
            except OSError as error:
                evidence.report["profile_cleanup_error"] = str(error)
        evidence.block_remaining("Native verification did not reach this stage")
        evidence.flush()
        print((out / "summary.md").read_text(encoding="utf-8"))
    return evidence.exit_code()


if __name__ == "__main__":
    sys.exit(main())
