# Known defects: benchmark

Baseline: main `8ee2c2c`. Branch: `fix/known-defects` at `83fbbf5`. Release
builds with Rust 1.94.0 on arm64 macOS 26.6.2 (Apple M4), outside iCloud, in
separate target folders. Both opened the same Rust fixture (main's
`editor.rs`) by its name on the command line, each build with a HOME of its
own; `sessions` was removed before every launch.

The branch touches the typing path in one place: a key that changes the
first word of a line, in a language that has indent rules, may leave the
line owed when the tree is late. For a Rust file that path returns before
the new code. Startup code is not changed. These are measurements with the
new behaviour inactive.

`editor/scripts/bench.sh` on both builds:

- main: cold_start_ms=121.5 (runs: 121.5 137.2 116.8 117.8 123.4); input_p50_ms=10 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=2.0 memory_mb=147
- branch: cold_start_ms=128.6 (runs: 128.6 132.9 122.3 123.1 147.7); input_p50_ms=9.7 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=139

Startup milliseconds, q1 / median / q3. One warm-up per build, then two
rounds of 21 interleaved launches, main first in the first round and the
branch first in the second.

- Round 1, main: 147.7 / 159.1 / 172.3
- Round 1, branch: 148.2 / 156.0 / 169.1
- Round 2, main: 151.5 / 165.1 / 182.3
- Round 2, branch: 151.4 / 157.6 / 169.7

Typing: 300 inputs, 16 ms apart, six interleaved runs per build.

- Run 1, main: input_p50_ms=9.7 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- Run 1, branch: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=147
- Run 2, branch: input_p50_ms=11 input_p99_ms=21 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- Run 2, main: input_p50_ms=9.8 input_p99_ms=23 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- Run 3, main: input_p50_ms=10.0 input_p99_ms=23 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- Run 3, branch: input_p50_ms=10 input_p99_ms=23 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=139
- Run 4, branch: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=139
- Run 4, main: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=146
- Run 5, branch: input_p50_ms=10 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=139
- Run 5, main: input_p50_ms=11 input_p99_ms=23 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=138
- Run 6, branch: input_p50_ms=11 input_p99_ms=22 frame_p50_ms=1.3 frame_p99_ms=2.0 memory_mb=138
- Run 6, main: input_p50_ms=10 input_p99_ms=20 frame_p50_ms=1.3 frame_p99_ms=1.9 memory_mb=147

The binary grew by 624 bytes, from 54,638,608 to 54,639,232.

No difference is shown. The script's five launches put the branch 7 ms
slower and the 42 interleaved ones put it 3 to 8 ms faster at the median;
nothing in startup changed. Input p50 moves between 9.7 and 11 ms run to
run for both builds alike.

Not measured: typing in a language of an extension with a grammar that is
late, and saving large files.
