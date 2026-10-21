#!/usr/bin/env python3
"""Plan missing Linux desktop packages with the runner's existing APT metadata.

No metadata refresh, package download or installation happens in this script.
The manual workflow may refresh metadata first. A later package acquisition
still needs human authorization; this report is not native verification.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
from urllib.parse import unquote, urlsplit


TARGETS = ("openbox", "xdg-desktop-portal", "xdg-desktop-portal-gtk")
APT_MANUAL = "https://manpages.ubuntu.com/manpages/noble/man8/apt-get.8.html"


def apt_command(archives):
    # --download-only alone would fetch packages. --print-uris replaces that
    # acquisition with a list. --simulate would bypass URI generation instead.
    # An empty private cache includes dependencies even if a .deb is cached in
    # /var/cache/apt/archives. Disable binary-cache writes and run without sudo.
    return ["apt-get", "--print-uris", "--download-only", "--no-install-recommends",
            "--no-remove", "--assume-yes", "-o", "Debug::NoLocking=1",
            "-o", f"Dir::Cache::archives={archives}/",
            "-o", "Dir::Cache::pkgcache=", "-o", "Dir::Cache::srcpkgcache=",
            "install", *TARGETS]


def parse_uris(output):
    packages = []
    filenames = set()
    for line in output.splitlines():
        if not line.startswith("'"):
            continue
        fields = shlex.split(line)
        if len(fields) != 4:
            raise ValueError("Unexpected APT URI record")
        url, filename, size, digest = fields
        source = urlsplit(url)
        official = source.hostname in ("archive.ubuntu.com", "security.ubuntu.com",
                                        "ports.ubuntu.com") or (
            source.hostname is not None and source.hostname.endswith(".archive.ubuntu.com"))
        if (not official or source.scheme not in ("http", "https") or source.username
                or source.password or source.port or source.query or source.fragment
                or not source.path.startswith(("/ubuntu/pool/", "/ubuntu-ports/pool/"))
                or not source.path.endswith(".deb")):
            raise ValueError("APT proposed a package outside official Ubuntu archives")
        if not re.fullmatch(r"[A-Za-z0-9.+~:%_-]+\.deb", filename):
            raise ValueError("Unexpected APT destination filename")
        if filename in filenames:
            raise ValueError("Duplicate APT destination filename")
        filenames.add(filename)
        parts = unquote(filename[:-4]).split("_")
        if len(parts) != 3 or not all(parts) or not size.isdecimal() or int(size) <= 0:
            raise ValueError("Unexpected APT filename or byte count")
        if not re.fullmatch(r"(?:MD5Sum:[0-9a-fA-F]{32}|SHA256:[0-9a-fA-F]{64}|SHA512:[0-9a-fA-F]{128})", digest):
            raise ValueError("Unexpected APT checksum")
        package, version, architecture = parts
        packages.append({"package": package, "version": version, "architecture": architecture,
                         "requested": package in TARGETS, "filename": filename,
                         "source_filename": unquote(source.path.rsplit("/", 1)[-1]),
                         "url": url, "size_bytes": int(size), "checksum": digest})
    return packages


def collect_plan():
    status = Path("/var/lib/dpkg/status")
    before = status.read_bytes()
    with tempfile.TemporaryDirectory(prefix="solder-package-plan-") as directory:
        archives = Path(directory)
        command = apt_command(archives)
        print("Metadata-only package plan: " + shlex.join(command), file=sys.stderr)
        result = subprocess.run(command, env={**os.environ, "LC_ALL": "C"},
                                stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                timeout=180, check=False)
        print(result.stdout, end="", file=sys.stderr)
        print(result.stderr, end="", file=sys.stderr)
        if any(archives.rglob("*.deb")):
            raise ValueError("APT unexpectedly wrote a package archive")
        if status.read_bytes() != before:
            raise ValueError("Installed package state changed during planning")
        if result.returncode:
            raise ValueError(f"APT package planning failed (exit {result.returncode})")
        packages = parse_uris(result.stdout)
    return {
        "schema_version": 1,
        "mode": "native-prerequisites",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "runner_image": {key: os.environ.get(key) for key in ("ImageOS", "ImageVersion")},
        "os_release": Path("/etc/os-release").read_text(encoding="utf-8"),
        "requested_packages": list(TARGETS),
        "scope": "Missing packages and solver-required upgrades, including dependencies, "
                 "against this runner's installed state before native build libraries. "
                 "Already satisfied dependencies and recommendations are excluded.",
        "apt_command": command,
        "apt_manual": APT_MANUAL,
        "proof": {"dpkg_status_sha256": hashlib.sha256(before).hexdigest(),
                  "dpkg_status_unchanged": True, "downloaded_deb_files": 0},
        "package_count": len(packages),
        "total_size_bytes": sum(package["size_bytes"] for package in packages),
        "packages": packages,
        "authorization": "Plan only. Human authorization is required before downloading "
                         "or installing these packages. Versions and runner images may change; "
                         "regenerate the plan before authorization. Native checks remain unverified.",
    }


def publish(plan, summary=None):
    payload = json.dumps(plan, indent=2) + "\n"
    print(payload, end="")
    if summary:
        with Path(summary).open("a", encoding="utf-8") as destination:
            destination.write("## Linux desktop package plan\n\n")
            destination.write("No .deb files downloaded or installed. This is not a native UI result.\n\n")
            destination.write("```json\n" + payload + "```\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--summary", type=Path, help="Append the JSON plan to the job summary")
    args = parser.parse_args()
    try:
        publish(collect_plan(), args.summary)
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"Package plan unavailable: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
