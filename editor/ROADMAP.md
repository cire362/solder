# Roadmap

Everything the website promises, grouped into phases. Each phase only depends on
the ones above it. Optimization work (see README) starts after phase 7.

Status: `[x]` done, `[~]` partial, `[ ]` not started.

## 0. Editor core
- [x] Rope buffer, undo/redo, multi-cursor, IME, clipboard
- [x] Tree-sitter highlighting for Rust, TS/TSX, JS, JSON, CSS, Go, Python
- [x] File tree, tabs, status bar, save

## 1. Navigation and search
- [x] Single-line editor mode (reused by every text field)
- [x] Command palette (`cmd-shift-p`, `f1`)
- [x] File finder with fuzzy matching over the project, respecting `.gitignore` (`cmd-p`)
- [x] Find and replace in file, regex, case and whole-word options (`cmd-f`, `cmd-alt-f`)
- [x] Search across the project with ripgrep's engine (`cmd-shift-f`)
- [x] Go to line (`ctrl-g`)
- [x] Auto-closing brackets and quotes, matching bracket highlight
- [x] File tree: new, rename, move to trash, copy path, reveal in Finder, keyboard navigation
- [x] Reload files changed on disk, refresh the tree
- [x] Ask before closing unsaved tabs and windows; untitled files and Save As
- [x] Settings file and keymap file, applied on save (`cmd-,`)

## 2. Language intelligence and terminal
- [x] Shared buffers: one file, many views
- [x] LSP client: diagnostics, completion with snippets, hover, go to definition (F12, cmd-click), references, rename, format (and on save)
- [x] LSP: code actions (`cmd-.`, with resolve and server-driven edits), signature help
- [x] Integrated terminal: PTY, 256 colors and truecolor, selection, scrollback, several tabs (`ctrl-\``)
- [x] Split panes (`cmd-\\`), focus between panes (`cmd-k cmd-left/right`)

## 3. Git built for review
- [x] Status in the file tree and gutter diff markers (against the index)
- [x] Stage selected lines (`cmd-alt-y`), revert a hunk (`cmd-alt-z`), step through hunks, commit and amend from the Git tab (`ctrl-shift-g`)
- [x] Three-way conflict view (ours | file | theirs) with accept ours, theirs or both (`cmd-k 1/2/3`)
- [x] Switch or create branches, push and pull in the terminal, open a pull request for GitHub and GitLab remotes
- [x] Side-by-side diff for a whole file

## 4. Services and containers
- [x] Detect services from `package.json` (npm, pnpm, yarn, bun), `go.mod`, `Cargo.toml`, Django/FastAPI/Flask entry points and compose files; add or override them in `.solder/services.json`
- [x] Services tab (`cmd-shift-s`): start, stop (Ctrl+C, then hang up), restart, ports read from the output, logs in a real terminal, Run stack and Stop all (also in the command palette)
- [x] Containers in the Services tab: status, start, stop, restart, follow logs

## 5. Database and HTTP clients
- [x] Connections from `.env` and compose; Postgres, MySQL, SQLite, Redis, MongoDB (Database tab, `ctrl-shift-d`; table previews in the Results tab)
- [x] Query runner (`cmd-enter` runs the statement under the cursor), schema-aware completion
- [x] Results grid with staged edits applied as one transaction; read-only production
- [x] Rows: add, duplicate and delete; defaults and server-filled columns; pick foreign key values from the referenced table
- [x] Browse without SQL: filter and sort by column, page through large tables, follow foreign keys
- [x] Table structure: create, change and drop tables, columns, indexes and foreign keys; review the DDL, then apply it or save it as a migration
- [x] ERD: tables and relations with layout, pan and zoom; open a table's data or structure; add tables, columns and relations; export SVG, PNG and Mermaid
- [x] Redis keys (every type, TTL) and MongoDB documents: add, edit, delete
- [x] HTTP client with requests generated from API routes, OpenAPI 3.1 import

## 6. Solder AI
- [x] Local models: a benchmark that reads the machine, times a small model and
      picks the best model it runs well; one-click install of llama.cpp and models
      (pinned build, SHA-256 checked, resumable); a catalog of fifteen models, any
      GGUF from Hugging Face or disk, and LM Studio's models
- [x] Providers: local llama.cpp, Ollama, LM Studio, OpenAI-compatible, Anthropic
      with your own key; model per task; offline mode
- [x] Project map for context; `.solderignore`; `.env` always excluded
- [~] Ask the codebase (chat), inline edits, streaming completions
      (streaming chat with the current file or selection and an optional project
      map; inline edits with a streamed preview, reviewed diff, one-step undo and
      stale-file protection; streaming completions are still to come)
- [ ] Agent tasks in their own branch and worktree with an editable plan: edits,
      commands and tests in a loop, network and outside-project commands confirmed,
      review the diff and merge
- [ ] AI review before push

## 7. Debugger, plugins, onboarding
- [ ] Debugger over DAP; one session across browser and server; query timeline
- [ ] Plugin host: WebAssembly sandbox, declared permissions, per-frame time budget
- [ ] Import settings from VS Code, Cursor, JetBrains
- [ ] Preview environments per branch (needs the hosted service)
