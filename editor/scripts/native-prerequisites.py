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
MIRROR_FILES = ("/etc/apt/apt-mirrors.txt", "/etc/apt/apt-mirrors-security.txt")


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


def official_archive(url):
    source = urlsplit(url)
    official = source.hostname in ("archive.ubuntu.com", "security.ubuntu.com",
                                    "ports.ubuntu.com") or (
        source.hostname is not None and source.hostname.endswith(".archive.ubuntu.com"))
    if (not official or source.scheme not in ("http", "https") or source.username
            or source.password or source.port or source.query or source.fragment
            or not source.path.startswith(("/ubuntu/pool/", "/ubuntu-ports/pool/"))
            or not source.path.endswith(".deb")
            or any(part in (".", "..") for part in unquote(source.path).split("/"))):
        raise ValueError("APT proposed a package outside official Ubuntu archives")
    return source


def resolve_uri(uri, architecture, cache):
    source = urlsplit(uri)
    if source.scheme != "mirror+file":
        return official_archive(uri), None
    if source.netloc or source.query or source.fragment:
        raise ValueError("Unexpected APT mirror alias")
    filename = next((name for name in MIRROR_FILES if source.path.startswith(name + "/")), None)
    if filename is None:
        raise ValueError("APT mirror alias names an unapproved local file")
    suffix = source.path[len(filename) + 1:]
    if not suffix.startswith("pool/"):
        raise ValueError("APT mirror alias is not a package pool path")
    if filename not in cache:
        cache[filename] = Path(filename).read_text(encoding="utf-8")
    choices = []
    for position, line in enumerate(cache[filename].splitlines()):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split()
        tags = {}
        for item in fields[1:]:
            key, separator, value = item.partition(":")
            tags.setdefault(key, set()).add(value if separator else "")
        # Release/language restrictions cannot be inferred from a .deb name.
        # Do not turn a partial mirror into an asserted download location.
        if (tags.keys() - {"priority", "arch", "type", "component"}
                or ("arch" in tags and architecture not in tags["arch"])
                or ("type" in tags and "deb" not in tags["type"])
                or ("component" in tags and suffix.split("/")[1] not in tags["component"])):
            continue
        priority = tags.get("priority", {str(2**31)})
        if len(priority) != 1 or not next(iter(priority)).isdecimal():
            continue
        try:
            candidate = official_archive(fields[0].rstrip("/") + "/" + suffix)
        except ValueError:
            continue
        choices.append((int(next(iter(priority))), position, candidate))
    if not choices:
        raise ValueError("APT mirror list has no eligible official Ubuntu package mirror")
    # Pin one URL for human review, rather than claim APT's random tie choice.
    return min(choices, key=lambda choice: choice[:2])[2], filename


def parse_uris(output):
    packages = []
    filenames = set()
    mirror_cache = {}
    for line in output.splitlines():
        if not line.startswith("'"):
            continue
        fields = shlex.split(line)
        if len(fields) != 4:
            raise ValueError("Unexpected APT URI record")
        uri, filename, size, digest = fields
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
        source, mirror_file = resolve_uri(uri, architecture, mirror_cache)
        packages.append({"package": package, "version": version, "architecture": architecture,
                         "requested": package in TARGETS, "filename": filename,
                         "source_filename": unquote(source.path.rsplit("/", 1)[-1]),
                         "url": source.geturl(), "apt_uri": uri, "mirror_file": mirror_file,
                         "size_bytes": int(size), "checksum": digest})
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
        "mirror_policy": "Mirror aliases use only existing allowlisted APT lists and resolve "
                         "to one eligible official Ubuntu URL pinned for review. This is not "
                         "a claim about APT's random selection or permission for other mirrors.",
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
