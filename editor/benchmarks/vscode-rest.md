# What was left of the VS Code host: benchmark

Baseline: main `de24368`. Branch: `feat/vscode-rest` at `38dfec5`: lists that take several, terminals and tasks that are an extension's own code, the code lens at the end of its line, notebooks.
Release builds outside iCloud, each in a target folder of its own. Same Rust fixture, isolated HOME, default settings and layout, no extension installed, so no host runs, no server offers a lens and no notebook is open: this is what the branch costs someone who uses none of it. What it touches on that path is the editor's element, which now reads the document's lenses for the rows it draws, and two more key bindings at startup.
One warm-up each, then rounds of 21 interleaved launches, the order reversed in every second round.

Startup milliseconds, q1 / median / q3:

- Round 1, main: 110.0 / 116.5 / 122.2
- Round 1, vscode-rest: 111.1 / 119.8 / 125.9
- Round 2, main: 113.7 / 116.9 / 121.0
- Round 2, vscode-rest: 115.7 / 118.0 / 122.0
- Round 3, main: 92.1 / 94.1 / 96.7
- Round 3, vscode-rest: 93.4 / 94.0 / 95.5
- Round 4, main: 97.3 / 100.4 / 102.0
- Round 4, vscode-rest: 97.6 / 100.2 / 101.7

Rounds 1 and 2 were taken right after the two builds ended, with the machine still busy: both versions are 20 ms slower there than an hour before, and the branch's median is 1 to 3 ms over main's inside quartiles that overlap. Rounds 3 and 4, taken after the typing runs, have the two within 0.2 ms. All four are given, not the better two.

Typing: 300 inputs per run, 16 ms apart, three interleaved runs each.

- main: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=139
- main: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.1 frame_p99_ms=1.7 memory_mb=132
- main: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.1 frame_p99_ms=1.8 memory_mb=140
- vscode-rest: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.7 memory_mb=131
- vscode-rest: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=131
- vscode-rest: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.1 frame_p99_ms=1.8 memory_mb=131

Binary: 51 495 376 bytes on main, 51 734 912 on the branch.

Not measured: a file with lenses drawn, and a notebook. One look at a notebook of four cells with a picture under one, in a debug build: frames of 2.0 to 3.5 ms and 120 to 137 MB for the process. That is one look, not a measurement.

Collected on arm64 macOS on 2026-10-10. Local comparisons, not changes to the website's performance claims.
