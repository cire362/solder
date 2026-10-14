# Extension notebook recovery in a native macOS window

Checked on 2026-10-15 (Asia/Yekaterinburg), Apple M4, macOS 26.6.2,
Rust 1.94.0, the `ci` build of source `837dec9`. This is a functional
check, not a performance measurement. All files and settings were in an
isolated temporary HOME. No published extension was installed or executed.

The first-party fixture is in `extension-notebook-fixture/`. Place its
`main.js` and `package.json` in the isolated extension store's
`vscode/roadmap.books` directory, and open `first.proofnote` from an isolated
project. Its controller only returns the cell text; it does not execute
JavaScript or start an external runtime. The serializer retains complete
cell data, output bytes, output metadata and notebook metadata.

## What was checked

1. The Extensions panel identified the installed fixture, said its Node code
   waited for permission, and showed an Allow control. The Files menu offered
   Open with Roadmap book after allowing it. No controller ran before that.
2. The native notebook displayed both original cells, saved output and the
   fixture controller. Pasting `extension_recovered_cell = 2` into its code
   cell and clicking Run all produced `fixture output: extension_recovered_cell = 2`.
3. The recovery JSON contained those exact bytes, both cells, metadata and the
   successful execution summary. The original file was still unchanged.
4. After SIGKILL of only the test editor's verified PID 55337, a restart
   restored the changed cell, output, dirty mark and recovery notice.
5. Running Roadmap: Fail next notebook save, then Cmd-S, showed
   `Roadmap fixture refuses this save`. Filesystem assertions confirmed the
   original remained unchanged and the complete recovery copy remained.
6. Cmd-Alt-S opened a real system save dialog. Saving
   `extension-rescued.proofnote` exported the exact recovered model while
   leaving the source, dirty mark and recovery copy unchanged.
7. Running Roadmap: Save notebook through extension called the notebook's
   own API. Its saved JSON matched the complete recovery model, the dirty
   mark cleared and the retained recovery copy was removed.

`extension-notebook-recovery.json` records the captured model and hashes.
Native controls were exercised through the desktop app's computer-use API;
byte comparisons and process termination were performed separately.

## Limits

This checks one local first-party reader/controller on macOS, small text
outputs, paste input, a serializer failure and a process kill. It does not
establish compatibility with a published extension, Linux/Windows, kernel
execution, remote files, large outputs, power loss or concurrent views.
Late save acknowledgements remain covered by process/headless tests rather
than this live sequence. Recovery and export notices use the error color,
and the long export path is clipped; these remain roadmap work.
