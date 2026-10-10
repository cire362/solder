# VS Code host benchmark

Baseline: origin/main `9ebbc68`. Branch: `feat/vscode-host`, feature commit `9fb658e`, plus the picker padding correction, with native extension views and decorations.
Release builds outside iCloud; all workspace packages rebuilt between versions. Same Rust fixture, isolated HOME, default settings/layout, no extension hosts.
`editor/scripts/bench.sh` ran on both versions. The samples below were collected after checks and compilation completed: one warm-up each, two rounds of 21 interleaved launches each, reversed order in round two.

Startup milliseconds, q1 / median / q3 (inclusive quartiles):

- Round 1, main: 109.0 / 115.9 / 119.2
- Round 1, vscode-host: 108.7 / 112.2 / 116.8
- Round 2, main: 109.1 / 110.9 / 113.6
- Round 2, vscode-host: 108.6 / 111.3 / 113.1

Typing: 300 inputs per run, 16 ms apart, three interleaved runs each.

- main, run 1: input_p50_ms=10 input_p99_ms=20 frame_p50_ms=1.0 frame_p99_ms=1.7 memory_mb=139
- main, run 2: input_p50_ms=9.9 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.7 memory_mb=131
- main, run 3: input_p50_ms=9.2 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.7 memory_mb=131
- vscode-host, run 1: input_p50_ms=9.8 input_p99_ms=20 frame_p50_ms=1.1 frame_p99_ms=1.8 memory_mb=131
- vscode-host, run 2: input_p50_ms=11 input_p99_ms=20 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=131
- vscode-host, run 3: input_p50_ms=9.5 input_p99_ms=20 frame_p50_ms=1.1 frame_p99_ms=1.7 memory_mb=131

Binary bytes: {'main': 50255760, 'vscode-host': 51205760}

Validation: scripts/check.sh passed, 484 Rust tests; Clippy 1.98.0 passed.
Node and headless UI tests cover lazy branches, action identity after refresh and re-registration, source control, test profiles, UTF-16 decorations, host shutdown and independent cleanup of marks and language-server hints.
The comparison covers the default layout without running extensions. Extension providers and their native panel are validated separately by integration tests and a synthetic live preview; these timings do not measure a published extension.

Live validation used a self-authored preview extension in an isolated HOME: expanded a tree, opened source-control files and replaced its message, ran a test profile and inspected both results, and observed the text annotation. The failed test name retained a complete Run action at 280 px dock width; the input guidance was fully visible after the padding correction. Impeccable scored both visual corrections resolved (`ship` for those two findings); its documentation pass found no new mismatch with incumbent tokens.

Standard `bench.sh` outputs:

```text
main: cold_start_ms=119.7 (runs: 130.0 426.2 119.7 106.6 113.4)
input_p50_ms=12 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.8 memory_mb=139
vscode-host: cold_start_ms=155.6 (runs: 125.6 140.0 155.6 180.0 197.5)
input_p50_ms=10.0 input_p99_ms=21 frame_p50_ms=1.2 frame_p99_ms=1.7 memory_mb=131
```

Collected on arm64 macOS with rustc 1.94.0 (4a4ef493e 2026-03-02). These are local comparisons, not changes to website performance claims.
