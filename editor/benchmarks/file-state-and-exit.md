# File state and exit: verification and benchmark

Measured on 2026-10-14 against main `eb3fc0a`. Rust 1.94.0 release builds
on arm64 macOS 26.6.2 (Apple M4), outside iCloud and in separate target folders.
The branch implements saved history checkpoints, serialized saves, confirmed
quit/close and original remote revisions in recovery. Both builds opened the
same Rust fixture (`editor.rs`, 128,463 bytes) outside a Git checkout.

No extensions, remote connection or notebook were active. Builds, test runners
and preview windows were stopped before measurement. Other user applications
remained open. These are fresh processes with warm disk caches, not disk-cold
starts. No website performance claim changed.

## Standard script

`editor/scripts/bench.sh` ran once per build, each with its own isolated HOME.

- Base: cold_start_ms=132.5 (273.7 132.5 125.9 124.5 136.5); input_p50_ms=13 input_p99_ms=21 frame_p50_ms=1.4 frame_p99_ms=1.7 memory_mb=161
- Branch: cold_start_ms=120.5 (243.4 117.9 120.5 124.2 120.1); input_p50_ms=12 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.6 memory_mb=159

## Interleaved startup

One warm-up per build, then two rounds of 21 launches per build. The first
build alternated on each pair and each round. Every launch used a fresh HOME.
Milliseconds, least / median / most:

- Round 1, base: 123.7 / 130.0 / 260.1
- Round 1, branch: 116.3 / 131.5 / 268.7
- Round 2, base: 126.4 / 137.7 / 157.8
- Round 2, branch: 129.6 / 137.4 / 153.8

## Typing

300 inputs, 16 ms apart. Three interleaved runs per build, each with a fresh
HOME, reversing order in the second pair:

- Run 1, base: input_p50_ms=13 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=157
- Run 1, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.6 memory_mb=172
- Run 2, branch: input_p50_ms=12 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.7 memory_mb=151
- Run 2, base: input_p50_ms=12 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.7 memory_mb=162
- Run 3, base: input_p50_ms=13 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.6 memory_mb=160
- Run 3, branch: input_p50_ms=13 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.6 memory_mb=150

The binary changed from 54,337,488 to 54,477,280 bytes (+139,792 bytes).
Startup medians differ by +1.5 ms in the first round and -0.3 ms in the
second. Input p99 is 21–22 ms for both. These samples show no consistent
startup or input slowdown. Branch memory varied more (150–172 MB against
157–162 MB); the ranges overlap and three runs do not establish a memory
regression or a guarantee. Raw samples are in
[file-state-and-exit.json](file-state-and-exit.json).

Not measured: pending saves, recovery of large files, active remote projects,
notebooks, or startup restoring several windows.

## Verification

`scripts/check.sh` passed with 784 tests, including 465 editor tests. Rust
1.98.0 clippy passed for the whole workspace and all targets. Tests cover
clean undo/redo, captured save checkpoints, overlapping writes, failed Save As,
queued remote revisions, preserved legacy/recovered text, quit/close decisions,
new edits while waiting, duplicate paths, Unix hard links and notebook copies.

A release macOS window used an isolated HOME and disposable text file. Paste
changed the document; Undo twice restored the disk text and removed its changed
mark; Redo restored both. Cmd-Q showed Save All / Don't Save / Cancel. Cancel
kept the window and edits. Save All wrote the edited bytes, ended the original
process and left no recovery files. Accessibility observation after exit
relaunched the temporary application; that extra process was stopped separately.
Facts are in [file-state-and-exit-native.json](file-state-and-exit-native.json).

Not verified live: character-by-character input (automation was unreliable),
multi-window decisions, closing tabs/windows, failure controls, notebooks,
remote recovery, Linux or Windows. Direct system Quit cannot be cancelled by
GPUI's shutdown callback; notebook recovery and atomic local text saves remain
unfinished in the roadmap.
