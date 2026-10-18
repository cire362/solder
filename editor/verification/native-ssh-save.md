# Native SSH save and recovery on macOS

Checked on 2026-10-18 on Apple M4, macOS 26.6.2, Rust 1.94.0 `ci` build.
The corrected binary is based on `eee352f` plus this PR's Save As alias fix.
`native-ssh-save.json` records the binary and changed-source hashes, exact text,
file hashes, assertions and limits.

Used an isolated HOME and the repository's own Python worker with an
unprivileged system sshd on loopback. Keys, known hosts and configuration were
generated for this disposable check. No user SSH configuration was changed.
The fixture's seven helper checks passed before the native client was started.
The target directory contained a space, a quote and Japanese characters.

1. Seeded the local cache with `stale_cache_must_not_open = 0` and the server
   file with `remote_original = 1`. Opened the cached file from the command
   line. The native window connected and displayed the fresh server text.
2. Pasted `remote_native_saved = 2` plus LF and saved with Cmd-S. Checked the
   exact bytes on the server, independently of the cache.
3. Pasted `remote_unsaved_recovery = 3` plus LF without saving. Verified its
   recovery copy, then changed the server file externally to
   `remote_external_change = 4` plus LF. Cmd-S refused the changed revision;
   the dirty buffer and recovery copy remained.
4. Force-ended only the test Solder process. Restarted without command-line
   arguments. Its kept window reconnected, restored the unsaved text and
   retained the original read revision. Saving still left the external server
   text untouched.
5. Used the native Save As dialog to choose `rescue-fixed.txt`. It returned a
   `/tmp/...` cache spelling while the connection was registered under
   `/private/tmp/...`. The corrected build saved the complete recovered text to
   the server, kept the tab in its canonical remote namespace, and removed the
   now-saved recovery copy. The original server file still had the external text.

The first native attempt exposed the Save As bug: that alternate spelling
had created a local cache file and marked it saved. Its text was preserved in
that local copy. The fix resolves dialog aliases off the UI thread before
choosing the save destination and also protects the revision, alias and write
helpers. Resolution stops at the remote boundary, retaining the ban on links
inside a project. Those links are checked before a remote write is sent.

Regression tests cover a live first-party worker, a new remote destination,
an alias of a changed source, a disconnected namespace, an unregistered cache,
an interior link and unchanged local symlink behavior. A headless workspace
test exercises the actual Save As dialog callback and the tab's resulting path.

Native input used the desktop computer-use API. It occasionally failed to bind
the window or returned an old frame; rebinding, selecting the file and resizing
the test window provided fresh observations. Expected input was checked on disk
or in recovery before the next step. Native dialog capture timed out after Save;
server bytes, the kept tab path and recovery cleanup independently confirmed
completion. This does not claim uninterrupted bulk typing or rendering latency.

The test app, SSH clients and daemon were stopped and the server's private
temporary directory removed. This verifies a local macOS SSH transport and a
restart reconnect. External servers, Linux/Windows clients, interactive PTYs,
connection loss during work, disconnected multi-window Quit, power loss and
remote notebook execution remain open in the roadmap.
