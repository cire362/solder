# Tests and running: benchmark

Baseline: main `74738e0`. Branch: `feat/tests-running` at `71c518a`.
Release builds with Rust 1.94.0 on arm64 macOS, outside iCloud, in separate
target folders. Both opened the same Rust fixture (main's `editor.rs`,
126,560 bytes), outside a Git checkout. Each launch used a fresh HOME with
default settings and layout, no remembered tabs, recovery text or extensions.
The Tests panel stayed closed and no discovery or debugger ran. These
measurements concern the cost with the new features inactive.

`editor/scripts/bench.sh` was run on both builds before the interleaved comparison:

- main: cold_start_ms=132.0 (runs: 147.5 128.2 124.2 134.2 132.0); input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=146
- branch: cold_start_ms=116.7 (runs: 117.5 115.6 116.7 112.7 119.4); input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=138

Startup milliseconds, q1 / median / q3. One warm-up per build, then two
rounds of 21 interleaved launches each, reversing the order in the second
round. No build, test run or preview window was active during measurement.

- Round 1, main: 144.4 / 169.1 / 172.9
- Round 1, branch: 158.4 / 169.4 / 172.5
- Round 2, main: 165.1 / 168.3 / 175.8
- Round 2, branch: 167.4 / 170.2 / 171.5

Typing: 300 inputs, 16 ms apart, three interleaved runs per build, reversing
the order in the second run. Each run used a fresh HOME.

- Run 1, main: input_p50_ms=7.2 input_p99_ms=20 frame_p50_ms=1.0 frame_p99_ms=1.8 memory_mb=146
- Run 1, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=146
- Run 2, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=146
- Run 2, main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=145
- Run 3, main: input_p50_ms=10 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=140
- Run 3, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=146

Binary: 52,657,760 bytes on main, 52,885,648 on the branch.

The interleaved startup medians were 0.3 and 1.9 ms higher on the branch;
the startup quartiles and input p99 ranges overlapped. Input p50 was 11 ms
on the branch and 7.2 to 11 ms on main. Typing-run memory was 146 MB on the
branch and 140 to 146 MB on main. These local runs do not establish that
the branch caused a memory or input-median change. Active test discovery,
large output and a real Python debug session were not measured and remain
in ROADMAP.md.

In a disposable macOS project, the live window discovered and ran passing
and failing unittest tests, showed clean output and opened the failed
test's source. Long traceback lines initially overlapped adjacent rows;
the corrected clipping was checked in the live window again. Entering a
breakpoint condition through the palette produced its gutter marker. F5
showed the missing-debugpy message and selected interpreter. Real debugpy
was not installed locally; its approved CI check is separate. Published
test extensions and other platforms remain unverified in ROADMAP.md.

Collected on 2026-10-11. Local comparisons, with no changes to the website's
performance claims.
