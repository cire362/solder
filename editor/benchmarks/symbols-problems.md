# Symbols and problems: benchmark

Baseline: main `49104a9`. Branch: `feat/symbols-problems` at `9fe8ec2`. Release builds outside iCloud, in separate target folders. The same Rust fixture (main's `editor.rs`, about 100 KB), a fresh isolated HOME for each measured launch, default settings and layout, no extension or language server running. Structure and Problems were closed; the branch's default breadcrumbs still find the file's fallback outline.

`editor/scripts/bench.sh` was run on both builds before the interleaved comparison:

- main: cold_start_ms=112.7 (runs: 152.5 115.6 110.3 112.7 110.5); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=133
- branch: cold_start_ms=109.3 (runs: 235.1 109.3 109.3 113.2 107.4); input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=137

Startup milliseconds, q1 / median / q3. One warm-up each, then two rounds of 21 interleaved launches per build; order reversed for the second round. Each measured launch begins with no remembered tabs or recovery text.

- Round 1, main: 111.0 / 114.2 / 116.9
- Round 1, branch: 111.5 / 114.5 / 117.0
- Round 2, main: 115.0 / 121.0 / 123.1
- Round 2, branch: 118.3 / 119.6 / 121.0

Typing: 300 inputs, 16 ms apart, three interleaved runs per build. Order reversed in the second run.

- Run 1, main: input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=133
- Run 1, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=137
- Run 2, branch: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=137
- Run 2, main: input_p50_ms=9.6 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=133
- Run 3, main: input_p50_ms=9.7 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=141
- Run 3, branch: input_p50_ms=9.9 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=137

The branch read 137 MB in all three typing runs, versus 133, 133 and 141 MB on main. To check the effect of the visible outline, three additional branch runs removed breadcrumbs from the title bar through `layout.json` (the Structure panel stayed closed):

- input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=133
- input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.9 memory_mb=132
- input_p50_ms=10 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=132

These runs read 132 to 133 MB. This associates about 4 to 5 MB with showing and finding the fallback outline on this fixture; it is not a heap profile, and the 141 MB baseline run shows that process memory also varies between launches. No change to the default layout was made.

Binary: 52,198,832 bytes on main, 52,406,848 on the branch.

No consistent slowdown in ordinary startup or input was observed in this comparison. It does not measure highlights with a running server, large outlines or streams of project diagnostics. Those cases remain in ROADMAP.md under Not verified, not finished.

Visual check: Structure, breadcrumbs, symbol highlights and diagnostics of two files were looked at in a real macOS window with `mock_lsp.py`. Input navigation is covered by headless tests; macOS refused synthetic key input into the real window.

Collected on arm64 macOS on 2026-10-10. Local comparisons, not changes to the website's performance claims.
