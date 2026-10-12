# Russian interface: benchmark

Measured on 2026-10-13, main `5f2b52f` against `feat/russian-interface`
`6355a31`. Release builds with Rust 1.94.0 on arm64 macOS 26.6.2
(Apple M4), outside iCloud and in separate target folders. Both opened
the same Rust fixture (`editor.rs`, 126,560 bytes), outside a Git checkout.

Each interleaved launch used a fresh HOME with only `language: en` or
`language: ru` configured. Each standard-script invocation used its own
fresh HOME. No extensions were installed, and no tool panels were active.
No builds or test runners were active during measurement. Other programs
of the user's remained open.

These are fresh processes with disk caches warm, not disk-cold startup.
The startup result of the standard script is named `cold_start_ms` even
though it discards the first launch. No website claim was changed.

## Standard script

`editor/scripts/bench.sh` was run on both builds in each language:

- en, base: cold_start_ms=135.5 (runs: 136.6 138.8 135.5 127.9 128.1); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- en, branch: cold_start_ms=122.3 (runs: 151.3 130.5 122.3 113.7 121.6); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=145
- ru, base: cold_start_ms=117.6 (runs: 121.9 116.6 117.6 119.5 113.2); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- ru, branch: cold_start_ms=122.2 (runs: 126.8 115.4 122.3 113.1 122.2); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146

## Interleaved startup

One warm-up per build and language, then two rounds of 21 launches per
build. The build run first alternated on each pair and on the next round.
Milliseconds, least / median / most:

- en, round 1, base: 112.3 / 120.6 / 133.1
- en, round 1, branch: 115.1 / 120.1 / 144.9
- en, round 2, base: 121.8 / 126.1 / 148.9
- en, round 2, branch: 117.9 / 126.0 / 134.8
- ru, round 1, base: 115.5 / 122.9 / 136.5
- ru, round 1, branch: 116.3 / 122.0 / 150.4
- ru, round 2, base: 119.0 / 129.0 / 145.0
- ru, round 2, branch: 116.8 / 129.3 / 160.4

## Typing

300 inputs, 16 ms apart. Three interleaved runs per build and language,
base first in each pair:

- en, run 1, base: input_p50_ms=9.9 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=139
- en, run 1, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=137
- en, run 2, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- en, run 2, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- en, run 3, base: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=145
- en, run 3, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=137
- ru, run 1, base: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.8 memory_mb=138
- ru, run 1, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=138
- ru, run 2, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=138
- ru, run 2, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.8 memory_mb=138
- ru, run 3, base: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=145
- ru, run 3, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138

The binary changed from 53,482,128 to 53,615,984 bytes
(+133,856 bytes).

Startup medians differ by at most 0.9 ms across the four paired rounds.
Input p99 stays at 20–21 ms in both languages and both builds. The ranges
overlap; this comparison shows no consistent slowdown. It does not
establish a performance guarantee.

Not measured: populated Russian tool panels, changing language during an
active operation, previews, notebooks, restored windows, or other platforms.
The Russian layout has not been visually reviewed. All launch samples and
the full output of each typing run are in [russian-interface.json](russian-interface.json).
