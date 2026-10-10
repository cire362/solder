# The editor block of phase 10: benchmark

Baseline: main `a7834d6`. Branch: `feat/daily-editor` at `580cfde`: pinned tabs, selection by column, encodings and line endings, unsaved text kept aside, indent guides, folding, wrapping.
Release builds outside iCloud, each in a target folder of its own. Same Rust fixture (`editor.rs` of main, 100 KB), isolated HOME, default settings and layout, no extension installed.
Every launch starts from no remembered tabs and no kept text: the folder of both is removed before it. Why that matters is below.

Startup milliseconds, q1 / median / q3. One warm-up each, two rounds of 21 interleaved launches, the order reversed in the second.

- Round 1, main: 101.7 / 102.4 / 106.5
- Round 1, daily-editor: 100.6 / 102.3 / 105.2
- Round 2, main: 100.9 / 103.0 / 104.9
- Round 2, daily-editor: 101.8 / 102.9 / 104.1

With unsaved text of the fixture waiting to be put back (15 launches of the branch): 102.2 / 103.5 / 105.9. The text is put back after the first frame, not before it.

Typing: 300 inputs per run, 16 ms apart, five interleaved runs each.

- main: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=131
- main: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=131
- main: input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=132
- main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=132
- main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=132
- daily-editor: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=141
- daily-editor: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=132
- daily-editor: input_p50_ms=11 input_p99_ms=23 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=132
- daily-editor: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=140
- daily-editor: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=132

In these runs the branch drew indent guides and kept the typed text aside every second, as it does in use. Memory was 132 MB in three runs of the branch and 140 to 141 in two; main was 131 to 132 in all five. Earlier runs of main alone, on other days, also read 131 to 140, so this is not shown to be the branch's, and it is not shown not to be.

With wrapping on (`"soft_wrap": true`), three runs of the branch:

- input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.4 frame_p99_ms=2.1 memory_mb=141
- input_p50_ms=11 input_p99_ms=23 frame_p50_ms=1.4 frame_p99_ms=2.1 memory_mb=133
- input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.4 frame_p99_ms=2.1 memory_mb=132

Wrapping costs 0.2 ms of a frame on this file: the lines that take more than one row are found again after every key.

Binary: 51 966 512 bytes on main, 52 198 800 on the branch.

## What the first measurements got wrong

The first runs had the branch's frame at 0.8 to 0.9 ms against 0.6 on main, and its startup 2 to 7 ms slower. Neither was the drawing. The typing benchmark types into the fixture and quits without saving; the branch keeps unsaved text and puts it back at the next start, so each run of the branch began with what every run before it had typed, under a line saying so. Runs were not the same run twice. Clearing the kept text before each launch gave the numbers above, and a benchmark no longer keeps what it types (`c283c5a`), which would also have put three hundred x's into the fixture's tab the next time its project was opened.

One change did come of it: a guide was a rectangle for every row at every level, and is now one for a run of rows (`580cfde`). Its cost before that was not measured cleanly.

Not measured: folding, a file in another encoding, a file of many megabytes with wrapping on (the pass that finds long lines reads the whole text after every key), and a window with many pinned tabs.

Collected on arm64 macOS on 2026-10-10. Local comparisons, not changes to the website's performance claims.
