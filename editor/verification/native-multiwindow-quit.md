# Native multi-window Quit on macOS

Checked on 2026-10-18, Apple M4, macOS 26.6.2, Rust 1.94.0 `ci` build.
The source includes session fix `297d607`; Rust code is unchanged at `9b96b01`.
Two disposable project roots and an isolated HOME were used in one real process.
No user project files or settings were changed.

1. Restored two real project windows, each showing a text file. Pasted
   `second_final_saved = 2` plus LF in one and `first_final_saved = 2` plus LF
   in the other. The project picker brought the already open window forward.
2. Cmd-Q offered one decision for two changed files. Cancel kept both windows,
   both originals and both exact recovery copies.
3. The first window's close button offered its own save decision. Cancel kept it.
4. Made only the first test fixture read-only on disk. Cmd-Q and Save All wrote
   the second project's changes, failed the first with `Not saved: Permission
   denied (os error 13)`, and kept the process running, the first buffer dirty,
   its original unchanged and its recovery copy present.
5. Restored write permission on that test fixture. Cmd-Q asked only about the
   remaining first file. Save All saved it and exited the process. Filesystem
   checks verified both exact byte sequences and removal of both projects'
   recovery copies.

`native-multiwindow-quit.json` records the assertions and hashes. Native input
used the desktop computer-use API; filesystem assertions and file-mode changes
were separate. The tool's clipboard/window-capture interruptions were followed
by fresh state reads; expected fixture input was checked before proceeding.

This covers two local text projects on macOS. It does not check edits made while
an OS decision is open, a pending notebook save during multi-window Quit, direct
system Quit, force termination, SSH disconnect or Linux/Windows multi-window
controls. Those remain in the roadmap or in the existing headless tests.
