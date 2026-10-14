#!/usr/bin/env python3
"""Use only the runner's existing tools; retain unavailable native prerequisites.

On Linux, an existing Xvfb and dbus-run-session can provide a virtual X11
display and session bus. Solder still creates an actual X11 OS window and
receives XTEST input; GPUI's headless/test backends are never used.
An offline build prevents implicit provisioning on a fresh runner. A parent
workflow may instead supply --binary after its separately approved build.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    scripts = Path(__file__).resolve().parent
    with (out / "harness-tests.log").open("w", encoding="utf-8") as log:
        tests = subprocess.run([sys.executable, str(scripts / "verify-native-harness-test.py")],
                               stdout=log, stderr=subprocess.STDOUT, timeout=30)
    command = [sys.executable, str(scripts / "verify-native-notebook.py"), "--out", str(out)]
    command += ["--binary", str(args.binary.resolve())] if args.binary else ["--build"]
    env = os.environ.copy()
    env["SOLDER_NATIVE_HARNESS_TEST_EXIT_CODE"] = str(tests.returncode)
    server, server_log, display_file = None, None, None
    try:
        if sys.platform.startswith("linux"):
            # Explicitly choose this harness's X11 path. Never invoke HeadlessClient.
            env.pop("WAYLAND_DISPLAY", None)
            env.pop("ZED_HEADLESS", None)
            if not env.get("DISPLAY") and shutil.which("Xvfb"):
                display_path = out / "xvfb-display.txt"
                display_file = display_path.open("wb")
                server_log = (out / "xvfb.log").open("wb")
                server = subprocess.Popen([
                    "Xvfb", "-displayfd", str(display_file.fileno()), "-screen", "0", "1440x1000x24",
                    "-nolisten", "tcp"], pass_fds=(display_file.fileno(),),
                    stdout=server_log, stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline and server.poll() is None:
                    number = display_path.read_text(encoding="ascii").strip()
                    if number.isdecimal():
                        env["DISPLAY"] = ":" + number
                        env["SOLDER_NATIVE_DISPLAY_KIND"] = "xvfb-virtual-display"
                        break
                    time.sleep(0.1)
            if not env.get("DBUS_SESSION_BUS_ADDRESS") and shutil.which("dbus-run-session"):
                command = ["dbus-run-session", "--", *command]
        result = subprocess.run(command, env=env, timeout=4500)
        return result.returncode if tests.returncode == 0 else 1
    finally:
        if server and server.poll() is None:
            server.terminate()
            server.wait(timeout=10)
        for stream in (server_log, display_file):
            if stream:
                stream.close()


if __name__ == "__main__":
    sys.exit(main())
