# Recovery and notebook rescue: verification and benchmark

Measured on 2026-10-15 (Asia/Yekaterinburg) against main `5d8204a`, after PR #57.
Rust 1.94.0 release builds on Apple M4, arm64 macOS 26.6.2. Builds used separate
checkouts and target folders outside iCloud. Both opened the same Rust fixture
(128,463 bytes) outside a Git checkout. Every launch had an isolated HOME.

No extensions, notebook, remote connection or retained recovery files were active.
Builds and tests finished before measurement. Other user applications stayed open.
These are fresh-process starts with warm disk caches. No website claim changed.

## Standard script

`editor/scripts/bench.sh` ran once per checkout, with an isolated HOME.

- Base: cold_start_ms=135.0 (runs: 130.2 135.0 129.8 146.5 137.9); input_p50_ms=10 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=159
- Branch: cold_start_ms=124.2 (runs: 128.9 131.1 121.5 124.2 123.5); input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=2.2 memory_mb=157

## Interleaved startup

One warm-up per binary, then two rounds of 21 launches per build. Order alternated
on each pair and each round. Milliseconds: minimum / median / maximum.

- Round 1, base: 118.0 / 124.1 / 137.5
- Round 1, branch: 121.5 / 125.7 / 133.2
- Round 2, base: 123.6 / 127.9 / 140.0
- Round 2, branch: 117.3 / 126.7 / 142.2

## Typing

300 inputs, 16 ms apart; three alternating runs per build.

- Run 1, base: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1 frame_p99_ms=4.5 memory_mb=125
- Run 1, branch: input_p50_ms=9.3 input_p99_ms=18 frame_p50_ms=1.3 frame_p99_ms=4 memory_mb=134
- Run 2, branch: input_p50_ms=11 input_p99_ms=19 frame_p50_ms=1 frame_p99_ms=17 memory_mb=125
- Run 2, base: input_p50_ms=9.6 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.1 memory_mb=160
- Run 3, base: input_p50_ms=9.5 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.2 memory_mb=174
- Run 3, branch: input_p50_ms=9.8 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.1 memory_mb=161

One initial branch run had frame p99 of 17 ms, so three additional pairs were
run under the same conditions to check whether that outlier recurred.

- Run 4, base: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=0.9 frame_p99_ms=7.5 memory_mb=108
- Run 4, branch: input_p50_ms=8.8 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=6 memory_mb=121
- Run 5, branch: input_p50_ms=3.8 input_p99_ms=19 frame_p50_ms=0.8 frame_p99_ms=4.4 memory_mb=123
- Run 5, base: input_p50_ms=8.9 input_p99_ms=12 frame_p50_ms=0.8 frame_p99_ms=4 memory_mb=111
- Run 6, base: input_p50_ms=7.1 input_p99_ms=9.4 frame_p50_ms=0.8 frame_p99_ms=20 memory_mb=123
- Run 6, branch: input_p50_ms=7.7 input_p99_ms=17 frame_p50_ms=1.3 frame_p99_ms=4.8 memory_mb=125

The initial startup medians differ by +1.6 ms and -1.2 ms. The first three typing
p99 values span 20–21 ms on main and 18–21 ms on the branch. The frame outlier
did not recur on the branch (follow-up p99 4.4–6 ms); main later reached 20 ms.
Follow-up input p99 and memory also varied substantially. These small samples
do not establish a persistent performance change or a guarantee. Longer active
notebook/recovery workloads remain unmeasured.

The binary changed from 54,512,880 to 54,622,096 bytes (+109,216).
Raw samples are in [recovery-and-notebook-rescue.json](recovery-and-notebook-rescue.json).

## Verification

`scripts/check.sh` passed: 857 tests, including 514 editor tests. Rust 1.98.0
Clippy passed for the whole workspace and all targets with warnings denied.
One existing doctest remains ignored.

Headless and first-party process tests cover original local/remote revisions,
weak legacy hashes, native notebook metadata/attachments/output bundles, complete
extension snapshots, execution results, restore/edit and restore/close ordering,
pending writes, changed export destinations, source aliases, failed serializers,
single-use export tokens, retained recovery copies and cleanup after save/close.
No dependency was added and no published extension code ran on this machine.

Not verified: native-window crash recovery/export, Linux/Windows behavior, published
notebook extensions, independent views, active remote recovery, large outputs or
sudden power loss. Text encoding/line-ending choices and a recovery-manager screen
remain unfinished. Limits and remaining checks are in the roadmap.
