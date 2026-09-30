# Rules for agents working in this repository

Read this file fully before changing anything. It applies to every coding
agent (Claude Code, Codex, Cursor and others) and to humans. Details that
also matter to humans live in [CONTRIBUTING.md](CONTRIBUTING.md); this file
adds what an agent needs to avoid the mistakes already made here once.

## What this repository is

```
src/      website (Next.js 16, Tailwind 4): landing, docs, blog, pricing
editor/   Solder, the IDE the website sells (Rust, GPUI)
scripts/  checks shared by both
```

Start from [editor/ROADMAP.md](editor/ROADMAP.md): it lists what exists, what
is next, and in what order. [editor/README.md](editor/README.md) explains the
stack and the measured performance numbers.

## Workflow

- Never commit to `main` directly. Branch from an up-to-date `main`:
  `feat/<topic>`, `fix/<topic>`, `perf/<topic>`, `refactor/<topic>`,
  `docs/<topic>`, `chore/<topic>`, `ci/<topic>`. One roadmap item or bug
  per branch.
- Commits follow Conventional Commits with a scope: `site`, `editor`, `text`,
  `syntax`, `lsp`, `terminal`, `git`, `services`, `db`, `ci`. Example:
  `fix(lsp): send didChange before completion requests`. Explain *why* in
  the body when the diff does not.
- Every commit builds and passes tests on its own. Do not commit dead code
  that only compiles because a later commit uses it.
- Open a pull request with `gh pr create`, fill in
  `.github/pull_request_template.md`, and wait for CI. Do not merge red.
- Never force-push a branch someone else may have pulled, never rewrite
  history on `main`, never delete branches you did not create.
- If you work in parallel with another agent, use a separate `git worktree`
  so neither of you touches the other's uncommitted files.
- Tick the box in `editor/ROADMAP.md` in the same PR that finishes an item.

## Ask before

- Adding any dependency (Rust crate or npm package). Explain what it is for
  and why the existing code or the standard library is not enough. A
  proposal to add `git2` and `similar` was declined once; Git support was
  then built on the `git` CLI and an in-house diff instead.
- Changing the performance numbers or claims on the website.
- Anything outward-facing: pushing to a new remote, publishing, changing
  repository settings, creating releases.

## Checks

Run `scripts/check.sh` before every push. It runs exactly what CI runs:
ESLint and TypeScript for the site; `cargo fmt --check`, `cargo clippy -D
warnings` and `cargo test` for the editor. Do not silence a lint with
`#[allow]` unless the reason is written next to it.

## Editor (Rust)

### GPUI is not what you remember

The UI framework is `gpui = 0.2.2` from crates.io, not the version in Zed's
repository or in training data. Its API differs (element traits, `Context`,
`Entity`, action macros, key contexts). Before using an API, read its source
in `~/.cargo/registry/src/*/gpui-0.2.2/src` and the examples next to it.
Never guess a signature.

### Architecture

- `Document` (`document.rs`) owns a file: text, syntax tree, undo history,
  diagnostics. `Editor` (`editor.rs`) is a *view* of a document: selections,
  scroll, popovers. Several editors can show one document (split panes).
- Edits go through `Document::edit`/`apply_edits` with the `EntityId` of the
  **editor** that made them as `origin`. Passing the document's id made the
  editor move its own cursor twice; this bug happened once.
- `Workspace` owns panes, tabs, the sidebar, modals, the terminal dock and
  the find bar. Global state lives in `Settings`, `Theme`, `Perf` and
  `LspStore` (see `main.rs` for init order).
- Language-server requests triggered by typing must run after the current
  effect cycle (`cx.defer`), so the `didChange` for what was just typed
  reaches the server first. See `Editor::after_typing`.
- Git goes through `git.rs`, which shells out to `git` with stable
  `--porcelain=v2 -z` formats. Everything in it blocks: call it from
  `cx.background_executor()`.
- Databases live in `crates/db` (no GPUI). Drivers run on a Tokio runtime
  that starts with the first connection; every public future is spawned
  there and can be awaited from GPUI. `DatabaseStore` (`database.rs`) owns
  connections in the app. TLS is rustls with the ring provider: keep
  `default-features = false` on driver crates so aws-lc is never built.

### Performance rules

The site promises cold start, memory and input latency numbers
(`src/components/performance.tsx`). Keep them true:

- Nothing slow on the UI thread: file IO, `git`, process spawns, full
  parses and searches go to the background executor.
- Draw only what is visible. The editor element shapes visible rows only;
  new UI that lists things uses `uniform_list`.
- Grammars and queries load lazily. Do not add work to startup.
- For changes to startup, rendering or typing, run
  `editor/scripts/bench.sh` before and after and put both in the PR.

### Key bindings

Bindings are scoped by key context. Multi-line editing keys bind to
`Editor && mode == full` so that single-line editors (pickers, find bar,
rename) let Enter, Tab and arrows reach their parent. Follow the same
pattern for new widgets instead of calling `cx.propagate()` everywhere.

### Tests

- New behavior comes with a test in the same PR.
- UI is tested headlessly with `#[gpui::test]` and `VisualTestContext`
  (`simulate_input`, `simulate_keystrokes`); see the tests at the bottom of
  `workspace.rs` and `editor.rs`.
- Features that talk to processes are tested against real ones: the
  terminal runs `/bin/sh`, language features run
  `crates/solder/tests/fixtures/mock_lsp.py`, git tests create a throwaway
  repository, database drivers run against the servers CI starts
  (`crates/db/tests/servers.rs`, see CONTRIBUTING.md). Call `cx.executor().allow_parking()` and poll with a timeout.
- Tests must not depend on tools that may be missing on CI (CI is Ubuntu:
  `git`, `python3` and `/bin/sh` are there; `rust-analyzer` is not).

### Style

- Colors come from `theme.rs` tokens, which mirror the website's
  `globals.css`. Shapes: panels 16px, controls 8px, inline tokens 6px.
- Comments explain why, not what. Match the density of the surrounding code.
- User-facing strings are short and plain, with no em dashes.

## Website (Next.js)

- Next.js 16 has breaking changes; read the note at the end of this file and
  the docs in `node_modules/next/dist/docs/` before writing site code.
- Design tokens live in `src/app/globals.css`: one zinc palette, one orange
  accent, the same radius rule as the editor. Do not add colors inline.
- Copy has no em dashes and no invented statistics. Benchmark figures in
  `performance.tsx` are marked as mock until real measurements replace them.

<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
