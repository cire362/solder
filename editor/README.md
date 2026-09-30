# Solder editor

The native editor behind the website in `../src`. Rust, GPU-rendered UI, no Electron and no webview.

```
crates/
  text/     rope buffer, edits, undo history, cursor movement (no UI, fully unit-tested)
  syntax/   tree-sitter parsing and highlighting for Rust, TS/TSX, JS, JSON, CSS, Go, Python
  solder/   the app: GPUI window, editor element, file tree, tabs, status bar
```

## Why this stack

The site promises three numbers: cold start, memory on a large repo, and keystroke-to-pixel latency. Every choice below is the one that moves those numbers the most.

| Choice | Why |
|---|---|
| **GPUI** (Zed's UI framework) | Draws the whole UI on the GPU (Metal, DirectX, Vulkan) in a retained scene with no DOM, no JS runtime, no layout thrash. It is the only production Rust UI stack with proven editor-grade latency. |
| **ropey** | O(log n) edits and O(1) snapshots. A snapshot is cheap enough to hand to a worker thread on every keystroke. |
| **tree-sitter**, incremental | Reparses only what changed. Budgeted to 1 ms on the typing path, then moves to a worker thread. Highlights are queried only for the rows on screen. |
| **Lazy grammars** | Grammar queries compile on the first file that needs them, so startup never pays for languages you do not open. |
| **Lazy file tree** | One `read_dir` of the root at startup. Folders are read when expanded. |
| **mimalloc** | Faster small allocations, less fragmentation. |
| **Release profile** | Fat LTO, one codegen unit, `panic = "abort"`. |

## Build and run

```bash
cargo run --release -p solder -- path/to/file-or-folder
```

```bash
cargo test --workspace
```

```bash
scripts/bench.sh [file]
```

The status bar shows live input latency, frame time, memory and startup time. Toggle it with `cmd-alt-p`.

## Measured so far

Apple M4, 16 GB, built-in 60 Hz display, release build.

| | Website promise | Now |
|---|---|---|
| Cold start to an editable file | 0.4 s | ~0.10 s (median of 5) |
| Frame time while typing | | 0.7 to 1.6 ms |
| Keystroke to painted frame | 6 ms | p50 9 to 11 ms, p99 17 to 20 ms |
| Memory, one small file | | 110 to 130 MB footprint, about 75 MB RSS |
| Memory, one 50k-line TS file | 310 MB for 500k lines, 40 files | 243 MB footprint |

## What is next, in order of impact on those numbers

1. **Latency: present on input.** Frames take about 1 ms, but GPUI waits for the next display refresh after a keystroke. On a 60 Hz panel that is up to 16.7 ms of waiting. Drawing immediately on input (vendoring GPUI's macOS window) is the only way to reach 6 ms on 60 Hz displays. On 120 Hz ProMotion the wait halves without any change.
2. **Memory: page out syntax trees.** A tree-sitter tree costs about 110 bytes per node (1.15M nodes, 128 MB for the 50k-line file). Drop trees for background tabs and reparse on focus (about 200 ms for that file, a few ms for typical ones).
3. **Cold start: precompiled shaders.** The build uses `runtime_shaders` because this machine has Command Line Tools only. With full Xcode, shaders compile into a `.metallib` at build time and startup skips that step.
4. **Brand fidelity.** Bundle Geist Mono (the site's code font) and Phosphor icons as app assets.
