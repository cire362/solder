# Windows and the way in: benchmark

Baseline: `feat/terminal-workflow` at `3d9411e`, which this block is made
on (main `9a6042b` with the terminal block, not merged yet). Branch:
`feat/windows-and-way-in` at `6b30ad7`. Release builds with Rust 1.94.0 on
arm64 macOS 26.6.2 (Apple M4), outside iCloud, in separate target folders.
Both opened the same Rust fixture (`editor.rs`, 126,560 bytes) by its name
on the command line, outside a Git checkout and build folders, each build
with a HOME of its own that held default settings and no remembered tabs:
`sessions` was removed before every launch.

A file named on the command line is the path a benchmark takes, and what
this block adds to it is small: the hook that writes a crash report, the
thread that waits to write the log, the language of the settings, and
reading the projects opened before, which is done off the thread that
draws. No window was put back, no preview or picture was open, and
nothing is kept of a benchmark's windows. These measurements are of the
cost with the new features inactive.

`editor/scripts/bench.sh` on both builds, before the interleaved
comparison:

- base: cold_start_ms=121.2 (runs: 133.9 169.4 121.2 121.2 117.3); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- branch: cold_start_ms=114.3 (runs: 114.5 144.4 113.4 112.5 114.3); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=138

The script calls its startup result `cold_start_ms`. These are fresh
processes with disk caches already warm; disk-cold startup was not
measured.

Startup milliseconds, least / median / most. One warm-up per build, then
two rounds of 21 interleaved launches each, base first in both rounds.
Tests had been run on the machine minutes before, and nothing else was
started during measurement; other programs of the user's were open.

- Round 1, base: 107.5 / 146.2 / 216.7
- Round 1, branch: 108.5 / 146.4 / 161.8
- Round 2, base: 136.5 / 143.4 / 162.1
- Round 2, branch: 135.7 / 141.6 / 171.3

Typing: 300 inputs, 16 ms apart, three interleaved runs per build, base
first.

- Run 1, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.1 memory_mb=146
- Run 1, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- Run 2, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.0 memory_mb=146
- Run 2, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=2.0 memory_mb=137
- Run 3, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- Run 3, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=145

The binary grew by 427,952 bytes, from 53,054,080 to 53,482,032.

No difference is shown between the two builds. The medians of the
interleaved rounds are higher than the script's (about 145 ms against
about 118 ms) for both builds alike. Why was not looked into: the rounds
removed `sessions` before every launch and the script does not, and the
machine was not otherwise the same minute to minute.

Not measured: a start that puts several windows back, a start that shows
the way in, typing into a Markdown file with its preview open, a long
Markdown file, a large picture, and anything in Russian.
