# Native notebook recovery on macOS

Checked on 2026-10-15 (Asia/Yekaterinburg), Apple M4, macOS 26.6.2.
Solder at `837dec9`, Rust 1.94.0, `ci` profile. This is a functional live-window
check, not a performance measurement. An app wrapper used an isolated HOME and
two disposable Jupyter format 4 files; no published extension was launched.

## Changed source and rescue copy

- Opened a code cell with saved stream/HTML outputs and a Markdown cell with
  an attachment. Edited both through native window input. Native paste inserted
  full Latin text; separate key events inserted Cyrillic characters matching the
  machine's current keyboard layout. Bulk `typeText` delivered only separators,
  so continuous bulk typing is not claimed from this check.
- Verified that the on-disk notebook was unchanged while its recovery file
  contained the two edits, output bundles, metadata and attachment.
- Killed only that test process with SIGKILL and changed the original file
  outside Solder. Relaunched the same project and observed the recovered cells,
  outputs, changed mark and recovery notice.
- Cmd-S refused the write with “The file changed; reopen it before saving”.
  The newer original's SHA-256 remained unchanged.
- Cmd-Alt-S opened the native Save dialog. Exported `rescued.ipynb`; its entire
  parsed model equals the recovery model. The original tab name and changed mark
  stayed, and its backup was retained.
- Cmd-W prompted. Cancel kept the notebook open. Closing with Don't Save after
  the rescue copy was written removed its recovery file; both the newer original
  and exported copy stayed intact.

## Unchanged source and successful save

- Edited a second notebook, verified its backup, and killed its test process.
- Relaunched without changing the source. The unsaved cell and outputs returned.
- Cmd-S wrote the complete recovered model, removed the changed mark and recovery
  notice, and released the recovery file. Metadata, outputs and attachment remained.

## Finding and limits

Starting with an explicit notebook after an earlier empty saved project session
left its tab inactive and the content blank. Clicking the tab made it active.
It is fixed in a separate commit on the same global branch: restoration keeps
an already selected view and choices made while it loads. After rebuilding,
launching the explicit notebook over an isolated empty saved session displayed
its active tab and cells immediately, with no click. Five regression tests cover
CLI files/cursors, notebooks, picture reuse, a choice during loading and pages
restored without taking the current view.

Success/recovery notices are currently colored like errors; this remains recorded
in the roadmap. This check covers the built-in Jupyter reader on macOS only.
The first-party extension reader is covered separately in
`extension-notebook-recovery.md`. Linux/Windows, large outputs, independent
views, power loss, continuous bulk keyboard injection and a Jupyter kernel
remain unverified by this native-reader check.
Hashes, the preserved model and lifecycle checks are in
[native-notebook-recovery.json](native-notebook-recovery.json).
