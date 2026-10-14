# Atomic text save through native macOS controls

Checked on 2026-10-15 (Asia/Yekaterinburg), Apple M4, macOS 26.6.2,
Rust 1.94.0, the `ci` build with session fix `297d607`. This checks behavior,
not performance. The file, symlink, settings and recovery belonged to one
isolated temporary fixture. No other project files were edited.

- Created `atomic.txt` with `atomic_original = 1` plus LF, mode 0640 and the
  macOS attribute `com.solder.roadmap=preserve-native-save`. Opened the relative
  symlink `atomic-alias.txt` through the CLI in an actual Solder window.
- Pasted `atomic_live_saved = 2` plus LF and used Cmd-S. The physical file had
  exactly those UTF-8 bytes. The symlink still pointed to `atomic.txt`; mode
  0640 and the extended attribute remained unchanged.
- Pasted `atomic_unsaved_rescue = 3` plus LF without saving, then changed the
  physical file externally to `external_revision_survives = 4` plus LF.
- Cmd-S refused with `The file changed on the target. Reopen it before saving.`
  The native editor retained the unsaved text and changed mark.
- Cmd-Alt-S and the real system Save dialog wrote `atomic-rescued.txt` with
  the exact unsaved bytes. The newer source hash remained unchanged. The
  renamed text tab closed with Cmd-W without a save question and its retained
  recovery file was removed.

Hashes and assertions are in `native-atomic-save.json`. The earlier process
and fault-injection tests check atomic publication itself; this verifies
native input reaching that save path, metadata retention and stale-write
refusal. It covers small UTF-8 files on the local macOS filesystem. Windows
replacement/ACLs, Linux attributes/ACLs, network filesystems, very large files
and sudden power loss remain unverified here.
