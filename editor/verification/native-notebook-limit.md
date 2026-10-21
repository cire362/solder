# Native notebook recovery size limit on macOS

Checked on 2026-10-21 (Asia/Yekaterinburg), Apple M4, macOS 26.6.2,
Rust 1.94.0, `ci` profile. Source parent `a166bee` plus the recovery-limit
correction in this commit; a boolean guard was simplified for Clippy after
the live run, with no behavior change. Exact run-source and binary hashes are in
[native-notebook-limit.json](native-notebook-limit.json).
This is a functional window check, not a performance measurement.

## Finding and correction

A serialized notebook larger than 16 MiB retained its preceding copy, but
reported success to the workspace. That generation was then remembered as
protected although it had never reached disk. Cells whose text alone exceeded
the limit were also skipped silently.

The writer now distinguishes a written copy, a size refusal and an IO error.
Only an acknowledged write advances the protected generation. A refusal keeps
the preceding file byte for byte and adds a separate warning above the cells;
it does not replace a save or serializer error. The same rejected generation
is not serialized every second. A later edit retries, and only a successful
current-generation copy or save clears the warning. Close/save cleanup remains
ordered behind pending writes.

## Live-window result

- Opened a 16,711,676-byte Jupyter format 4 file with a large stream output,
  cell id, execution count, metadata, unknown fields and a Markdown attachment.
  Used a disposable HOME. No published extension or kernel was started.
- Pasted a small comment into the code cell. Its 16,712,080-byte recovery file
  contained the edit and complete model; the source file's hash stayed unchanged.
- Pasted a further 131,075 bytes in one operation. The English warning appeared,
  the preceding backup stayed byte-identical, the source stayed unchanged and
  the tab retained its changed mark.
- Switched only the disposable profile to Russian with `ui_font_size: 24`.
  The full translated warning remained readable above the cells.
- Selected the cell's text and pasted a short replacement. Recovery resumed
  and its entire model equalled the original with only that source changed.
  The warning disappeared after acknowledgement.
- Cmd-S completed the ordinary save. The complete parsed notebook matched the
  expected model, metadata/attachment/output included. Its changed mark was
  gone, and no recovery or staged-save file remained. Cmd-Q exited cleanly.

Screenshots: [English warning](native-notebook-limit-en.png),
[Russian warning with larger text](native-notebook-limit-ru.png),
[completed save](native-notebook-limit-saved.png).

## Automated checks and remaining scope

The writer test crosses the real 16 MiB boundary and checks explicit outcomes,
byte-identical retention and the next successful write. The notebook test checks
large cell text, stale acknowledgements, an independent serializer notice,
failed save and successful save. The workspace test crosses the actual limit
through Paste, verifies the visible warning, complete retained model, retry,
save and cleanup. The 22 recovery-filtered tests passed locally.

This does not add recovery for models above 16 MiB. Their latest changes still
need an explicit save; the warning says so. Linux/Windows, extension-reader size
refusals and physical power loss remain open. Native input here is paste and
normal shortcuts, not continuous bulk key injection.
