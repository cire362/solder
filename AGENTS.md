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
  `docs/<topic>`, `chore/<topic>`, `ci/<topic>`. A branch is for a large
  update: a whole block of the roadmap (all of "Zed extensions, complete"),
  not each of its items. All of its work is committed to that one branch and
  it is merged once, when the whole update is ready: do not open a branch or
  a pull request per item, and do not wait for a merge to go on. A small
  change that is already checked (a visual fix, a lint, a bug found on the
  way) does not get a branch of its own either: commit it separately on the
  branch in progress and mention it in that pull request.
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
- `Workspace` owns panes, tabs, the docks, modals and the find bar. Where
  the parts of the window are and how large is `Layout` (`layout.rs`), read
  from `layout.json`: do not write a size or a panel's place into the code.
  A named layout uses `layouts/<name>.json` instead; `layout::path(cx)` names
  the file in use. `layouts.json` remembers the shared choice and the one
  each open project was left in. Saving, selecting and resetting a layout
  use the same queue as a size change, so pending edits reach their original
  file before another layout is selected.
  Each of the three docks shows one `Panel`, the terminals and the
  debugger included; which dock a panel is in is the layout's to say
  (`Workspace::show_panel`), never a field of its own, and one that has
  nothing left to show leaves through `Workspace::panel_gone`. What the
  title bar and the status bar say is `layout::Item`s, drawn by
  `Workspace::bar_item`: a new thing to say there is a new item, not a
  line in the bar's own code. A button on a bar is an item too
  (`Item::Button`), and runs its command through `bar_commands::run`, the
  same commands the palette and the keymap have. A change made
  by hand in the window is written back to the file (`layout::keep` for a
  size, `layout::put` for the rest): one key at a time and one write after another, and
  the file is not read while a write of ours is on its way. Global state lives in `Settings`, `Theme`, `Perf` and
  `LspStore` (see `main.rs` for init order).
- A document belongs to several language servers (`LspStore`): the one
  Solder knows for its language, then every one an installed extension
  brings. Answers that add up (completions, code actions, diagnostics) come
  from all of them, each in its server's own position encoding: convert
  before mixing. A request with one answer goes to the first server that
  advertises it (`LspStore::request`). Extensions may add to the options
  of a server Solder knows; options are read once, at the start, so such a
  server is started again (`LspStore::restart`) when they change, and a
  start that finishes after a later one began is dropped.
- Language-server requests triggered by typing must run after the current
  effect cycle (`cx.defer`), so the `didChange` for what was just typed
  reaches the server first. See `Editor::after_typing`.
- What a server draws into the text (inlay hints, semantic colors, code
  lenses) is the document's (`Document::inlays`, `Document::semantic`,
  `Document::lenses`), asked for a moment after the last change
  (`LspStore::schedule_hints`), never per key, and moved with the text
  until the answer comes. A hint is in the
  row and not in the file: `DisplayLine` maps columns both ways, as it
  does for tabs, and nothing else may assume a column is a place on
  screen. A lens is the last thing in its row, and is kept only if a
  click can do it: its command is one its server said it runs, or one
  of the few the editor does itself (`editor_lsp::LENS_COMMANDS`).
- Git goes through `git.rs`, which shells out to `git` with stable
  `--porcelain=v2 -z` formats. Everything in it blocks: call it from
  `cx.background_executor()`.
- Databases live in `crates/db` (no GPUI). Drivers run on a Tokio runtime
  that starts with the first connection; every public future is spawned
  there and can be awaited from GPUI. `DatabaseStore` (`database.rs`) owns
  connections in the app. TLS is rustls with the ring provider: keep
  `default-features = false` on driver crates so aws-lc is never built.

- Plugins run in `crates/plugin` (no GPUI): one WebAssembly instance and one
  thread per plugin, JSON messages both ways, permissions checked there
  before a request reaches the app (`plugin_store.rs`). The UI thread never
  waits for a plugin. Keep `wasmi` on `portable-dispatch`: its default
  dispatch overflowed the stack in dev builds and took the editor down.
  A plugin's budget is time, read between slices of fuel; fuel itself is
  not a measure (a unit is worth a hundred times less time in QuickJS, which
  runs `plugin.js` plugins, than in a plugin compiled from Rust).
  `editor/plugins/*/plugin.js` is compiled from `plugin.ts`; `npm run
  plugins` checks it is current, and checks the Go package and examples
  where Go is installed. A plugin that is a WASI program (QuickJS, or one
  compiled from Go) gets only what `wasi.rs` gives it: its streams, a clock
  and random numbers. Do not add files or sockets there.

- Context servers are Model Context Protocol servers the agent gets tools
  from: programs started here, or ones reached over HTTP. The client is
  `ai::mcp` (no GPUI, everything in it blocks, whichever way the server
  is talked to);
  `McpStore` (`mcp_store.rs`) starts the servers the settings name when an
  agent task begins, never at startup. A server's tool runs outside the
  agent's sandbox: a task asks the user before the first call of each
  (`agent_task.rs`), and nothing may call one without that.

- Extensions of other editors are read by `crates/extension` (no GPUI) and
  kept by `ExtensionStore` (`extension_store.rs`). A language from one is a
  tree-sitter grammar in WebAssembly: `crates/syntax` compiles it on the
  first file that needs it, never on the UI thread (`Language::is_ready`),
  and parsers that run such grammars come from a pool. Such a grammar
  never parses on the thread that asked either: its scanner cannot be
  stopped, so every parse goes through `parse_rope`, which runs it on a
  thread of its own and gives up on a grammar that does not answer
  (`wasm::Watch`). Do not call a pooled parser directly. A language of a
  VS Code extension has a TextMate grammar instead (`textmate.rs`): its
  `SyntaxTree` has lines and no tree, so anything that asks a tree must
  take `None` for an answer (`SyntaxTree::tree_at`). The network is used
  only when the Extensions tab is opened, searches or installs, and only against
  Zed's catalog and Open VSX. An archive is unpacked in a staging folder
  and checked before it replaces anything. Highlight queries follow the
  rule that the last matching pattern wins. What such a language says
  about typing (`syntax::Editing` from its `config.toml`, the queries in
  `crates/syntax/src/rules.rs`) holds for that language only: a built-in
  one keeps the editor's own rules. Indentation follows Zed's rules
  (`SyntaxTree::indent`), so that a query means here what it means there;
  the language's patterns are compiled in `indent.rs`, which has the regex.
  The code of a Zed extension runs in `crates/extension/src/host.rs`: a
  WebAssembly component in wasmtime, with one folder of its own and a fuel
  budget per call. All it does outside goes through the `World` trait
  (`world.rs` is the real one: npm with `--ignore-scripts`, GitHub
  releases, downloads), and through a `Gate` in front of it (`gate.rs`),
  which refuses what the user took back from that extension and writes
  down what it did. A new way out of the sandbox goes through both. Its calls block for as long as a download takes:
  `ExtensionStore::resolve` runs them on a thread of their own, never on
  the UI thread or the background executor. The files in
  `crates/extension/wit` are Zed's and stay as published. Each version of
  the API that changed the world has a folder there and a module in
  `src/host/`, which is only the bindings and a few macro lines: what the
  functions do is written once in `host.rs`. A new version is a new folder
  and a new module, never an edit to an old one, since extensions built for
  the old one stay in the catalog. Keep `wasmtime` and `wasmtime-wasi` on
  the version tree-sitter brings.
  The code of a VS Code extension has no sandbox, so it asks first
  (`Extension::outside`, `manifest::NODE_CODE`) and runs only once allowed
  (`ExtensionStore::may_run`). Each extension has a Node process of its own
  (`crates/extension/src/vscode.rs`), started with `host/host.js`, which is
  in the binary and gives the extension its `vscode` module; the two talk
  in JSON, a message a line. Everything in `vscode.rs` blocks: the store
  starts a host on a thread of its own, hears it through a channel
  (`ExtensionStore::heard`), and asks with `VsHost::ask`, which does not
  wait. Nothing of the editor waits for an extension: one that stops
  answering is ended by its watch. Ending a process waits for it, so it is
  never done on the UI thread (`stop_code`). A part of VS Code's API that
  is not in `host/*.js` must not throw: `namespace()` there gives a
  stand-in and tells the editor what was asked for.
  The editor's side of that API is `extension_api.rs`. What an extension
  reads without waiting (folders, settings, documents, the cursor) is a
  copy in its host: said once in `init`, then change by change, and never
  while no host runs, so typing costs nothing without extensions. A new
  thing of that kind is a new message both ways, not a request. What needs
  a window (a list, a file to show, an edit) is an `Ask` in the store's
  queue, taken by the workspace in front (`Workspace::extension_asks`);
  its answer goes through `Reply`, which answers "nothing" when dropped,
  so no extension waits forever. What extensions say in the status bar is
  `ExtensionStore::bar`, drawn by the `extensions` item.
  Language features of extensions add nothing to the editor's own: the
  host is a language server to it (`lsp::LanguageServer::linked`, a server
  that is no process), asked in the protocol and answered by
  `host/languages.js` from the providers the extension registered. A new
  feature for extensions is first a feature the editor has for language
  servers, then a handler there. What a server can do is read once, so
  when an extension registers something more its server is closed and
  the documents get a new one (`LspStore::hosts_changed`).
  What a manifest contributes (commands, menus, keys) is offered from the
  manifest, before any code runs: `ExtensionStore::palette`, `menu` and
  `sync_keys`, all through one action, `RunExtensionCommand`. Conditions
  are `extension::when`, read against `Workspace::extension_facts`; a
  fact it lacks is false, so a new fact is added there, never guessed.
  The store cannot read the workspace it is asked from, which is in the
  middle of its own update: the workspace brings the facts.
  The debugger starts adapters and speaks to them itself, so an
  extension's code is only asked what the launch and the adapter are
  (`debug.adapter`, through `ExtensionStore::ask_host`, which starts the
  code if it does not run). An adapter that is an object in that code is
  put behind a local port by `host/debug.js`: no second way to talk to
  an adapter in Rust.
  A terminal of an extension is a terminal of the dock, kept by the
  workspace that made it (`extension_terminals`); a task is a command
  in such a terminal, and what `host/shell.js` knows of its end is what
  the terminal's process ended with. A terminal the extension draws
  itself has no program, so its terminal runs `host/relay.js`, which
  carries between the terminal and the extension's object on a local
  port, under a token: again nothing new in Rust. Watching files is the
  host's own work with Node: the editor is not asked.
  A page of an extension (`webview.rs`) is the one browser view there
  is: the system's, through `wry`, a child of the window placed where
  GPUI laid out its tab. It is made when a page is first drawn, never
  at startup and never in tests, whose windows have no handle to give.
  A page is shut in, and each way out is closed where the browser is
  built: navigation, new windows, downloads, permissions, and files
  outside the folders the extension named. A new thing a page may do
  is a new line there, with a test of what it refuses.
  A notebook (`notebook.rs`) is no page: a tab of cells the editor draws
  itself, kept by the workspace next to the pages' tabs. The extension
  reads the file, runs the cells and writes it back (`host/notebooks.js`,
  which also reads and writes the file: no bytes of it cross to the
  editor); the editor says what is typed and which cells there are, a
  cell by a number that stays its own. A cell is an `Editor` as tall as
  its text (`Editor::fitted`): it never scrolls up or down itself, draws
  only the rows its place in the window leaves to be seen, and asks the
  list it is in to show the cursor. Cells are a `list`, so only the ones
  on screen are laid out. What a run puts out is words or a picture
  (`notebook::outputs`); a form that is neither is named, never drawn as
  a page.
  Whether published extensions work is found by `extension::census`,
  which installs and starts them. It runs their code, so it belongs to
  the workflow that runs it on a runner with no secrets: do not run it
  against Open VSX on a machine that matters, and test it against a
  catalog served locally, as its own test does.

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
- `debug_bounds` in a UI test says where an element was last drawn, and
  GPUI 0.2.2 never forgets one: it cannot tell that an element is gone.
  Assert that on the state behind it. For one that should appear, wait for
  it (`bounds_soon` in `workspace.rs`) and do not look in the same breath.
- Tests must not depend on tools that may be missing on CI (CI is Ubuntu:
  `git`, `python3` and `/bin/sh` are there; `rust-analyzer` is not).

### Style

- Colors come from `theme.rs` tokens, which mirror the website's
  `globals.css`, the terminal's sixteen included: write no color outside
  that file. Shapes are tokens too, never numbers: `theme.shape.panel`
  (16px as it comes), `.control` (8px), `.token` (6px) for corners, and
  `.border` (1px) for the lines between parts, as in
  `.border_b(theme.shape.border)`.
- Text sizes of the interface are `theme::TextSize`, never pixels or
  plain rems: `UI_FONT_SIZE`, `UI_FONT_SMALL` or `theme::text(11.5)`. A
  rem follows `ui_font_size` and the theme's `spacing`, and a `TextSize`
  takes the first alone. A theme is put in use with `theme::put`, which
  also sizes the rem: do not set the `Theme` global of a window directly. The height of a row of a list is `theme::row(ROW, cx)`,
  which follows `ui_density`. Code is sized by `buffer_font_size` alone.
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
