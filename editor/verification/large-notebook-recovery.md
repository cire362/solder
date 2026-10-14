# Large notebook and independent native views

Checked on 2026-10-15 (Asia/Yekaterinburg), Apple M4, macOS 26.6.2,
Rust 1.94.0, the `ci` build of `837dec9` plus the session-foreground fix
committed as `297d607`. These are functional checks, not performance samples.
Both app processes used separate isolated HOME and recovery folders.
No published extension or kernel ran.

## Large retained outputs

A Jupyter format 4 fixture was 5,400,478 bytes: one code cell with a
3,300,000-byte stream output and one Markdown cell with a 2,000,000-byte
text attachment, plus notebook/cell metadata and cell ids. The stream was
`large output retains every byte\\n` repeated 100,000 times (literal backslash
and n); the attachment was `retained attachment ` repeated 100,000 times.
The source code was `large_original = 1`, with execution count 9.

- Opened it in a real window and pasted `large_recovered_cell = 2` into the
  code cell. The on-screen output was bounded by the existing display limit.
- Checked the 5,400,828-byte recovery copy, then killed only verified test
  PID 80698. Relaunch showed the recovered cell, output, dirty mark and notice.
- Cmd-S saved the complete recovery model and removed its backup. Full JSON
  equality checked outputs, attachments, metadata and ids, beyond visible text.

## Independent views of one physical file

- Opened the saved notebook in a second native Solder process with a different
  bundle id, settings folder and recovery namespace. Both read the same file.
- Pasted `independent_view_b = 4` into the second process, leaving it dirty.
- Pasted `independent_view_a = 3` into the first and saved there.
- Cmd-S in the second refused with `The file changed; reopen it before saving`.
  Its text and recovery model remained its own; the file retained view A.
- Cmd-Alt-S exported `view-b-rescued.ipynb` through a real system save dialog.
  Its full JSON matched view B's recovery model; view A's saved bytes were
  unchanged. Closing view B with Don't Save after this export removed its backup.

Hashes and assertions are in `large-notebook-recovery.json`. This tests two
processes and native Jupyter on macOS with a recovery file below the 16 MB
retention limit. It does not check larger snapshots, physical power loss,
extension readers in independent views, remote files or Linux/Windows.
