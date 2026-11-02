#!/usr/bin/env python3
"""Package an already-built Solder executable without starting or building it.

BUILD-INFO.json hashes the executable as shipped, after macOS ad hoc signing.
The adjacent .sha256 file hashes the complete archive in sha256sum format.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile


TAG = re.compile(
    r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)


def release_tag(value):
    if len(value) > 128 or not TAG.fullmatch(value):
        raise argparse.ArgumentTypeError("tag must be a v-prefixed version, such as v0.1.0-preview.20261102.1")
    return value


def source_revision(value):
    if not re.fullmatch(r"[0-9a-fA-F]{40}", value):
        raise argparse.ArgumentTypeError("source must be a full 40-character hexadecimal commit")
    return value


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def readme(platform, arch):
    common = (
        "Solder preview\n\n"
        "Prerelease software. This is a portable archive, with no installer or updater.\n"
        "Choose the archive for your operating system and CPU architecture.\n"
        "BUILD-INFO.json records the source commit and the packaged binary's SHA256.\n"
        "SHA256SUMS.txt on the release page checks the whole download.\n"
        "RUNTIME-LIBRARIES.txt, when included, records the build's dynamic libraries;\n"
        "these libraries are not bundled. Packaging does not verify GUI startup.\n\n"
    )
    cpu = "x86_64 (64-bit Intel/AMD)" if arch == "x86_64" else "arm64"
    instructions = {
        "macos": (
            f"Requires macOS 15.0 or later, {cpu}.\n"
            "Extract the ZIP, move Solder.app to Applications, then open Solder.app.\n"
            "The app is ad hoc signed, without a Developer ID signature, and is not notarized.\n"
            "Gatekeeper may require approval in System Settings > Privacy & Security.\n"
        ),
        "windows": (
            f"Requires Windows for {cpu}.\n"
            "Extract the ZIP to a folder, then double-click solder.exe.\n"
            "Microsoft Edge WebView2 Runtime is required for extension pages.\n"
            "The executable is unsigned; Windows may show an unknown-publisher warning.\n"
        ),
        "linux": (
            f"Native baseline: Ubuntu 24.04, {cpu}. Other distributions are not verified.\n"
            "Extract the .tar.gz, open a terminal in that folder, then run ./solder.\n"
            "Requires an X11 or Wayland desktop and a Vulkan-capable GPU and driver.\n"
            "Runtime packages include libvulkan1, libxkbcommon0, libxkbcommon-x11-0,\n"
            "libwayland-client0, libx11-xcb1, libxcb1, libfontconfig1, libfreetype6,\n"
            "libasound2t64, libzstd1 and libwebkit2gtk-4.1-0 (WebKitGTK 4.1).\n"
            "Extension pages currently require X11; they are not supported under Wayland.\n"
            "This archive is unsigned.\n"
        ),
    }
    return common + instructions[platform]


def sign_macos(bundle):
    if sys.platform != "darwin":
        raise ValueError("macOS packaging requires macOS and the system /usr/bin/codesign")
    # An absolute system tool and argv avoid PATH substitution and shell expansion.
    subprocess.run(
        ["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", str(bundle)],
        check=True, capture_output=True, text=True, timeout=60,
    )
    subprocess.run(
        ["/usr/bin/codesign", "--verify", "--strict", str(bundle)],
        check=True, capture_output=True, text=True, timeout=60,
    )


def write_archive(root, destination, platform, executable, executable_mode):
    paths = sorted(root.rglob("*"))
    if platform == "linux":
        with tarfile.open(destination, "w:gz") as archive:
            for path in paths:
                info = archive.gettarinfo(str(path), path.relative_to(root).as_posix())
                info.mode = executable_mode if path == executable else (0o755 if path.is_dir() else 0o644)
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                if path.is_file():
                    with path.open("rb") as stream:
                        archive.addfile(info, stream)
                else:
                    archive.addfile(info)
    else:
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for path in paths:
                name = path.relative_to(root).as_posix()
                directory = path.is_dir()
                mode = executable_mode if path == executable else (0o755 if directory else 0o644)
                info = zipfile.ZipInfo(name + "/" if directory else name)
                # Explicit Unix metadata also works when these tests run on Windows.
                info.create_system = 3
                info.external_attr = ((stat.S_IFDIR if directory else stat.S_IFREG) | mode) << 16
                if directory:
                    info.external_attr |= 0x10
                    archive.writestr(info, b"")
                else:
                    info.compress_type = zipfile.ZIP_DEFLATED
                    with path.open("rb") as source, archive.open(info, "w", force_zip64=True) as target:
                        shutil.copyfileobj(source, target)


def package(args):
    if not args.binary.is_file() or args.binary.stat().st_size == 0:
        raise ValueError("binary must be a nonempty regular file")
    if args.runtime_report is not None and not args.runtime_report.is_file():
        raise ValueError("runtime report must be a regular file")
    if args.platform == "macos" and sys.platform != "darwin":
        raise ValueError("macOS packaging requires macOS and the system /usr/bin/codesign")

    suffix = ".tar.gz" if args.platform == "linux" else ".zip"
    name = f"solder-{args.tag}-{args.platform}-{args.arch}{suffix}"
    archive_path = args.out / name
    checksum_path = args.out / (name + ".sha256")
    for path in (archive_path, checksum_path):
        if os.path.lexists(path):
            raise ValueError(f"refusing to overwrite {path}")

    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".solder-package-", dir=args.out) as temporary:
        staging = Path(temporary)
        root = staging / "payload"
        root.mkdir()
        binary_name = {"macos": "Solder.app/Contents/MacOS/solder",
                       "windows": "solder.exe", "linux": "solder"}[args.platform]
        executable = root / binary_name
        executable.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(args.binary, executable)
        # Keep existing Unix execute permissions; CI inputs without them must still launch.
        mode = stat.S_IMODE(args.binary.stat().st_mode) & 0o777 if os.name != "nt" else 0o755
        if not mode & stat.S_IXUSR:
            mode |= 0o111
        executable.chmod(mode)

        if args.platform == "macos":
            version = ".".join(TAG.fullmatch(args.tag).groups())
            info = {
                "CFBundleIdentifier": "dev.solder.ide",
                "CFBundleName": "Solder",
                "CFBundleDisplayName": "Solder",
                "CFBundleExecutable": "solder",
                "CFBundlePackageType": "APPL",
                "CFBundleInfoDictionaryVersion": "6.0",
                "CFBundleShortVersionString": version,
                "CFBundleVersion": version,
                "LSMinimumSystemVersion": "15.0",
                "NSHighResolutionCapable": True,
            }
            (root / "Solder.app/Contents/Info.plist").write_bytes(plistlib.dumps(info))
            sign_macos(root / "Solder.app")

        metadata = {
            "source": args.source,
            "tag": args.tag,
            "platform": args.platform,
            "arch": args.arch,
            "binary": binary_name,
            "binary_sha256": sha256(executable),
        }
        (root / "BUILD-INFO.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        (root / "README.txt").write_text(readme(args.platform, args.arch), encoding="utf-8")
        if args.runtime_report is not None:
            shutil.copyfile(args.runtime_report, root / "RUNTIME-LIBRARIES.txt")

        staged_archive = staging / name
        write_archive(root, staged_archive, args.platform, executable, mode)
        staged_checksum = staging / checksum_path.name
        staged_checksum.write_text(f"{sha256(staged_archive)}  {name}\n", encoding="ascii")
        created = []
        try:
            for source, target in ((staged_archive, archive_path), (staged_checksum, checksum_path)):
                # Exclusive creation closes the race after the early overwrite check.
                with target.open("xb") as destination:
                    created.append(target)
                    with source.open("rb") as stream:
                        shutil.copyfileobj(stream, destination)
        except BaseException:
            for path in reversed(created):
                path.unlink()
            raise
    return archive_path


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--platform", choices=("macos", "windows", "linux"), required=True)
    parser.add_argument("--arch", choices=("arm64", "x86_64"), required=True)
    parser.add_argument("--tag", type=release_tag, required=True)
    parser.add_argument("--source", type=source_revision, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--runtime-report", type=Path)
    args = parser.parse_args(argv)
    try:
        archive = package(args)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        detail = error.stderr.strip() if isinstance(error, subprocess.CalledProcessError) and error.stderr else str(error)
        parser.exit(1, f"Packaging failed: {detail}\n")
    print(archive)
    return 0


if __name__ == "__main__":
    sys.exit(main())
