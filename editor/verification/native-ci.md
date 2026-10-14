# Native Linux and Windows verification

Prepared on the global completion branch. Native runner results remain open
until a dispatched run produces evidence; local harness contract tests do not
verify either platform's editor window.

The existing `extensions.yml` workflow has an explicit `native-verification`
mode on disposable Ubuntu 24.04 and Windows 2022 runners. This mode cannot
run `extension-census`, install published extensions or use repository secrets.
Checkout credentials are not persisted. Linux build libraries, Rust setup and
locked Cargo dependencies are the same as the existing editor build; the
harness itself uses only Python's standard library and OS window/input APIs.

```sh
gh workflow run extensions.yml --ref feat/roadmap-completion -f mode=native-verification
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
