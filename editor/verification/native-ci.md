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
process logs, source/binary hashes and session/recovery snapshots. Seven local
contract tests validate its preservation oracle, schema, race waits and partial
results. Exit 0 means all native stages passed; 1 means failure; 2 means a native
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

This preparation is not proof that Linux can render; the retry must pass and
produce native evidence before its roadmap box can be checked.
