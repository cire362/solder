# Native Linux and Windows verification

Dispatched from `9b96b01` on the global completion branch:
[run 38633325807](https://github.com/cire362/solder/actions/runs/38633325807).
Windows passed all six native stages; its downloaded models and screenshots
were independently reviewed (`windows-native-notebook.md`). Linux built but
exited before creating an OS window: GPU initialization returned
`NoSupportedDeviceFound`. Native Linux verification remains open. Local harness
contract tests do not verify either platform's editor window.

The existing `extensions.yml` workflow has an explicit `native-verification`
mode on disposable Ubuntu 24.04 and Windows 2022 runners. This mode cannot
run `extension-census`, install published extensions or use repository secrets.
Checkout credentials are not persisted. Linux build libraries, Rust setup and
locked Cargo dependencies are the same as the existing editor build; the
harness itself uses only Python's standard library and OS window/input APIs.

```sh
gh workflow run extensions.yml --ref feat/roadmap-completion -f mode=native-verification -f native_platform=linux-only
```

The scripts require GitHub Actions and output under RUNNER_TEMP. They refuse
to replace an existing Solder profile. Native state is copied to evidence before
their owned profile is removed. Build files stay outside the checkout. Manual
source ref selection is available through `verification_ref`.

## Evidence

- Native X11/XTEST or Win32 windows, foreground ownership and real OS keys.
- An explicitly opened file over an empty saved session, with actual saved input.
- Notebook source edits, outputs, metadata, attachments and unknown fields.
- Recovery written without touching the source; process termination and a
  restart with no notebook argument; further native input in the restored cell.
- The actual system Save File dialog, a separate export, complete JSON equality,
  an unchanged source and retention of the source recovery copy.
- Successful recovered save and recovery cleanup.

The workflow retains report.json, a job summary, OS screenshots, file bytes,
process logs, source/binary hashes and session/recovery snapshots. Twenty-two local
contract tests validate preservation, schema, race waits, partial results and
desktop/driver prerequisite handling. Exit 0 means all native stages passed; 1 means failure; 2 means a native
prerequisite or stage was unavailable. A partial run must keep the corresponding
roadmap entries open.

Linux uses an existing Xvfb/dbus session if needed; it still opens actual X11
windows rather than GPUI's headless platform. A file chooser portal, usable
rendering device and Windows interactive desktop remain runner prerequisites;
missing ones are reported rather than silently installed. X11, not Wayland,
is exercised. No physical display/GPU, power loss, published extension, kernel
execution, remote project or independent notebook view is claimed by this suite.

## Linux retry preparation

The first runner built Solder but GPUI could not initialize a Vulkan device.
The retry selects a preinstalled Lavapipe or browser-bundled SwiftShader driver
through the Vulkan loader's driver manifest setting. It installs no driver and
keeps X11/real OS input. A small first-party C probe records default and selected
driver capabilities; that inventory cannot pass a native UI stage. Missing or
unsupported devices stay blocked, and unrelated crashes fail. Diagnostic logs
are also printed on a failed run. The platform selector defaults to both;
`linux-only` avoids repeating the successful Windows build.

The retry at `eee352f`, [run 39112604725](https://github.com/cire362/solder/actions/runs/39112604725),
created an OS window with the runner's existing Chrome SwiftShader driver
(`/opt/google/chrome/vk_swiftshader_icd.json`). It failed the first stage with
`Timed out waiting for Solder to select the foreground`; the remaining five
stages were blocked. No notebook input, recovery or export passed on Linux.
`linux-native-attempt.json` retains that result and the reviewed job-log hash.

The harness had started bare Xvfb without a window manager. GPUI's X11 window
activation sends `_NET_ACTIVE_WINDOW` to a manager; its application activation
is a no-op. The next preparation observes an existing manager, or starts only a
preinstalled Openbox, Xfwm4 or Metacity on its own Xvfb. It never sets Solder's
focus itself or alters an existing unmanaged display. Missing managers remain
blocked before the build. The manager, log and display have owned cleanup.
No manager or other dependency was installed.

Window discovery and Vulkan diagnostics are not the six native stages. The
managed-desktop retry must still pass and produce native evidence before its
roadmap box can be checked. A file chooser portal remains a separate prerequisite.

## Managed-desktop result and package plan

The attempt at `a166bee`, [run 39114083965](https://github.com/cire362/solder/actions/runs/39114083965),
found none of the three supported window managers on the runner. It correctly
blocked all six stages before building or claiming a native result. No manager
was installed. The job log was reviewed on 2026-10-21; its hash and exact result
are retained in `linux-native-attempt.json`.

A separate, manual-only `native-prerequisites` mode now prepares the dependency
decision on a disposable Ubuntu 24.04 runner. It refreshes existing package
metadata and plans Openbox, the desktop portal service, its GTK backend and
solver-required dependencies, without recommendations. It neither builds Rust
nor starts an editor, downloads package archives, installs packages or runs a
published extension. No new runtime dependency is added by this preparation.

`native-prerequisites.py` uses APT's URI-printing mode, an empty private archive
cache and no root privileges for resolution. It prints each archive's exact
filename, official Ubuntu source URL, byte count and checksum to the job log
and summary. It also verifies that no .deb appeared and the installed-package
database did not change. URI-printing behavior is documented in the
[APT manual](https://manpages.debian.org/bookworm/apt/apt-get.8.en.html#OPTIONS).
Fifteen offline planner contracts and the 22 native-harness contracts run in
`scripts/check.sh` and editor CI.

```sh
gh workflow run extensions.yml --ref feat/roadmap-completion -f mode=native-prerequisites
```

This is only a package plan. Exact archives and sizes must be reviewed and
approved before any acquisition or installation; changing runner images or
package versions requires a fresh plan. Native Linux results remain open.

The first package plan, [run 39606874735](https://github.com/cire362/solder/actions/runs/39606874735),
refused the runner's `mirror+file:/etc/apt/apt-mirrors.txt/pool/...` aliases.
APT had printed 32 proposed archives, but no final source URL plan was produced;
no package archive was fetched or installed. The actual APT records are retained
as an offline regression fixture, without executable package contents.

The planner now reads only the two allowlisted existing Ubuntu mirror lists.
It filters eligible official Ubuntu mirrors and pins one concrete URL for human
review, retaining the original APT alias in the report. This is a deterministic
review choice, not a claim about APT's randomized fallback selection. Local files,
foreign hosts, unrelated pool paths and unsupported partial-mirror constraints
cannot become download locations. Mirror-list behavior is documented by the
[APT transport manual](https://manpages.debian.org/bookworm/apt/apt-transport-mirror.1.en.html).
The corrected live plan must still run before any archive approval.
