# Notebook follow-up: benchmark

Baseline: main `de24368`. Branch: `feat/vscode-rest` at `aa23f1e`.
Release builds with Rust 1.94.0 on arm64 macOS, each in a separate target
folder outside iCloud. Same 88,587-byte Rust fixture (2,288 lines), copied
from the baseline's editor.rs into a clean project directory. Each launch
gets a fresh isolated HOME outside that project; no extension is installed.
Session restoration and saving are enabled, as in normal use.

One warm-up each, then two rounds of 21 interleaved launches per version,
reversing the order on alternate pairs. Startup milliseconds, q1 / median / q3:

- Round 1, main: 105.8 / 111.4 / 113.8
- Round 1, branch: 106.2 / 110.6 / 114.2
- Round 2, main: 109.8 / 112.5 / 115.2
- Round 2, branch: 110.9 / 113.3 / 116.6

Typing: 300 inputs 16 ms apart, three interleaved runs per version.

- main: input_p50_ms=9.9, input_p99_ms=20, frame_p50_ms=1.2, frame_p99_ms=1.7, memory_mb=139
- branch: input_p50_ms=12, input_p99_ms=20, frame_p50_ms=1.2, frame_p99_ms=1.8, memory_mb=132
- branch: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.1, frame_p99_ms=1.7, memory_mb=140
- main: input_p50_ms=10, input_p99_ms=21, frame_p50_ms=1.2, frame_p99_ms=1.7, memory_mb=132
- main: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.1, frame_p99_ms=1.7, memory_mb=139
- branch: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.2, frame_p99_ms=1.8, memory_mb=132

Binary: 51,495,376 bytes on main, 51,966,512 on the branch.

The first comparison accidentally opened the parent of the build caches as
the project, with the benchmark homes inside it. Main used 145–162 MB;
the branch used 192–210 MB. A clean project brought both back to 131–139 MB.
That also exposed a real edge case: atomic session saves inside an open
project generated structural file events and repeated scans of the entire
project. Solder's session directory is now excluded from both the watcher
and the finder; user directories called sessions are still included.

The original large-project case was repeated after that fix, with settings
inside the project. Its typing runs:

- main: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.2, frame_p99_ms=1.9, memory_mb=159
- branch: input_p50_ms=10, input_p99_ms=21, frame_p50_ms=1.3, frame_p99_ms=1.9, memory_mb=150
- branch: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.3, frame_p99_ms=1.9, memory_mb=148
- main: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.3, frame_p99_ms=2, memory_mb=154
- main: input_p50_ms=11, input_p99_ms=21, frame_p50_ms=1.2, frame_p99_ms=2, memory_mb=150
- branch: input_p50_ms=11, input_p99_ms=22, frame_p50_ms=1.3, frame_p99_ms=1.9, memory_mb=142

These compare ordinary editing with no extension host, no drawn lenses and
no notebook open. They do not measure large notebooks or a file containing
many lenses. Live notebook typing, saving, HTML, renderer messaging and
restoration were verified separately with a small local fixture.

Collected on 2026-10-10. Local comparisons; website performance claims are
unchanged.
