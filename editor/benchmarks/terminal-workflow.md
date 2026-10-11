# Terminal workflow: benchmark

Baseline: main `9a6042b`. Branch: `feat/terminal-workflow` at `99e83bf`.
Release builds with Rust 1.94.0 on arm64 macOS 26.6.2, outside iCloud,
in separate target folders. Both opened the same Rust fixture (main's
`editor.rs`, 126,560 bytes), outside a Git checkout and build folders.
Each launch used a fresh HOME with default settings and layout, no
remembered tabs, recovery text or extensions. No terminal was opened.
These measurements concern the cost with the new features inactive.

`editor/scripts/bench.sh` was run on both builds before the interleaved
comparison:

- main: cold_start_ms=118.1 (runs: 344.1 118.1 122.1 113.8 116.2); input_p50_ms=10.0 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=145
- branch: cold_start_ms=115.2 (runs: 115.2 110.3 115.4 123.1 114.4); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=146

The script calls its startup result `cold_start_ms`. These are fresh
processes and profiles, with disk caches already warmed by the builds
and preceding launches; disk-cold startup was not measured.

Startup milliseconds, q1 / median / q3 (inclusive quartiles). One warm-up
per build, then two rounds of 21 interleaved launches each, reversing the
order in the second round. No build, test run or preview window was active
during measurement.

- Round 1, main: 114.3 / 118.1 / 120.9
- Round 1, branch: 115.5 / 120.7 / 122.6
- Round 2, main: 124.3 / 126.4 / 132.5
- Round 2, branch: 123.4 / 125.9 / 127.1

Typing: 300 inputs, 16 ms apart, three interleaved runs per build,
reversing the order in the second run. Each run used a fresh HOME.

- Run 1, main: input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=146
- Run 1, branch: input_p50_ms=10 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=146
- Run 2, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=138
- Run 2, main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=145
- Run 3, main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=145
- Run 3, branch: input_p50_ms=10 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=140

Binary: 52,885,648 bytes on main, 53,054,080 on the branch, an increase
of 168,432 bytes.

The branch's startup medians differed by +2.6 and -0.5 ms; the quartile
ranges overlapped. Input p50 was 10 to 11 ms on both, and input p99 was
20 to 21 ms on the branch against 21 ms on main. Typing-run memory was
138 to 146 MB on the branch against 145 to 146 MB on main. These runs
do not establish a consistent startup or memory change caused by the
branch. Busy terminal output, retained-history search, two visible
terminal panes and model inference were not measured and remain in
ROADMAP.md.

The functional checks use the emulator, real PTYs and headless window
input. They cover source links, search while output keeps arriving,
split focus and resizing, and editable failed-command drafts. Shell
integration was checked in six Bash/Zsh startup configurations, including
existing prompt hooks and pipeline status. A macOS window was opened,
but automation did not reliably apply the terminal controls. Live
keyboard and mouse use remains unverified, as do other platforms and
explanations from a real model; these are recorded in ROADMAP.md.

Collected on 2026-10-11. Local comparisons, with no changes to the
website's performance claims.
