# Git history: benchmark

Baseline: main `714d50b`. Branch: `feat/git-history` at `561a139`.
Release builds on arm64 macOS, outside iCloud, in separate target folders.
Both opened the same Rust fixture (main's `editor.rs`, 126,354 bytes), outside
a Git checkout. Settings and layout were the defaults; no extensions were
installed in the isolated homes. History stayed closed and blame stayed off.
This comparison measures the cost with the new features inactive.

`editor/scripts/bench.sh` was run on both builds before the interleaved comparison:

- main: cold_start_ms=125.2 (runs: 291.5 119.2 208.9 125.2 108.6); input_p50_ms=12 input_p99_ms=21 frame_p50_ms=1.4 frame_p99_ms=1.8 memory_mb=156
- branch: cold_start_ms=116.9 (runs: 117.6 116.9 111.9 114.1 135.5); input_p50_ms=10 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=2.0 memory_mb=158

Startup milliseconds, q1 / median / q3. One warm-up each, then two rounds
of 21 interleaved launches per build, reversing the order in the second
round. Every measured launch used a fresh HOME, without remembered tabs
or recovery text.

- Round 1, main: 120.8 / 148.6 / 154.1
- Round 1, branch: 127.9 / 148.7 / 155.6
- Round 2, main: 149.1 / 154.3 / 156.8
- Round 2, branch: 146.8 / 151.3 / 154.1

Typing: 300 inputs, 16 ms apart, three interleaved runs per build, reversing
the order in the second run. Each run used a fresh HOME.

- Run 1, main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.4 frame_p99_ms=1.7 memory_mb=158
- Run 1, branch: input_p50_ms=9.9 input_p99_ms=21 frame_p50_ms=1.4 frame_p99_ms=1.9 memory_mb=168
- Run 2, branch: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.8 memory_mb=163
- Run 2, main: input_p50_ms=9.6 input_p99_ms=22 frame_p50_ms=1.4 frame_p99_ms=1.8 memory_mb=164
- Run 3, main: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.4 frame_p99_ms=2.0 memory_mb=161
- Run 3, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.4 frame_p99_ms=2.2 memory_mb=157

Binary: 52,406,848 bytes on main, 52,657,760 on the branch.

No consistent slowdown in ordinary startup or input appeared in these
runs. Process memory overlapped: 158 to 164 MB on main and 157 to 168 MB on
the branch. This comparison does not establish a memory change caused by
the branch. It does not measure an open history, a large commit patch or
typing with blame enabled; those cases remain in ROADMAP.md.

Visual check on a disposable macOS repository: the branch graph, file
history, opening a commit patch, authors in the gutter and inserting text
with blame enabled. Changed text became **Not committed**. A clean merge
initially opened without a diff; after the fix its changes against the
first parent were seen in the real window. Opening an older patch after a
rename is covered by a headless click test. Stash, the commit guard, model
suggestions and the PR panel are covered by process and headless tests.

Collected on 2026-10-10. Local comparisons, not changes to the website's
performance claims.
