# Roadmap

Everything the website promises, grouped into phases. Each phase only depends on
the ones above it. Optimization work (see README) starts after phase 7.
Phase 8 goes past what the website promises: it is what it takes for an
extension made for Zed or VS Code to be installed with one button and work
as it does in the editor it was made for.

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
- [x] Ask the codebase (chat), inline edits, streaming completions
      (streaming chat with the current file or selection and an optional project
      map; inline edits with a streamed preview, reviewed diff, one-step undo and
      stale-file protection; completions as you type, filled in the middle by
      llama.cpp or asked through chat, accepted with Tab)
- [x] Agent tasks in their own branch and worktree with an editable plan: edits,
      commands and tests in a loop, network and outside-project commands confirmed,
      review the diff and merge (commands sandboxed with `sandbox-exec` or `bwrap`)
- [x] AI review before push (outgoing commits' diff, findings as data, push waits
      only when something is found)

## 7. Debugger, plugins, onboarding
- [x] Debugger over DAP; one session across browser and server; query timeline
      (JavaScript and TypeScript through js-debug: breakpoints, steps, stack,
      variables, console; a Next.js server and its page in one session; HTTP and
      SQL of the run, each linked to its line)
- [x] Plugin host: WebAssembly sandbox, declared permissions, per-frame time budget
      (plugins in Rust against an SDK: commands, status bar, the open file, project
      files, HTTP to declared hosts; approved per module; typing work over budget is
      deferred and the plugin marked slow)
- [x] Import settings from VS Code, Cursor, JetBrains
      (and Zed: font, indent, format on save; themes from VS Code and Zed theme
      files, kept in `themes`; the editors' own keys and the bindings a user
      changed; offered once on a first launch; an import only adds)
- [x] Plugins in TypeScript and JavaScript (one `plugin.js` run by QuickJS inside
      the same sandbox, with typings; the budget is measured in time)
- [x] Plugins in Go (a program built for WASI, in the same sandbox, with a
      package that speaks to the editor)
- [x] Extensions of Zed and VS Code, installed from Zed's catalog and Open VSX
      with no service of our own: languages (grammars in WebAssembly, with the
      languages inside them), snippets and themes; a VS Code extension gives
      its themes and snippets and points to the Zed one for its language
- [x] Language servers of Zed extensions: the extension's own code, run in
      wasmtime, gets the server (npm, a GitHub release) and says how to start
      it. One server per language
- [x] Extensions built for every version of Zed's API so far, 0.0.1 to 0.7:
      each of its nine worlds has its bindings, over one implementation
- [ ] The plugin registry (needs the hosted service)
- [ ] Preview environments per branch (needs the hosted service)

## 8. Extensions, in full

Where it stands: extensions install from Zed's catalog and Open VSX. A Zed
extension brings its languages, snippets, themes and one language server. A
VS Code extension brings its themes and snippets only.

The two kinds are different work. A Zed extension is data plus a small
sandboxed program with a fixed interface, so it can be supported completely.
A VS Code extension is a Node program written against VS Code's own API: it
runs here only as far as that API is rebuilt here, one area at a time, and
it runs with the user's rights, not in a sandbox. The items are in the order
to build them.

### The Extensions tab
- [ ] Extensions as a tab of the sidebar, next to Files and Git, in place of
      the window: installed, Zed's catalog and Open VSX in one list, search,
      one Install button, progress and errors on the row
- [ ] Before the first install of an extension, what it will do outside a
      sandbox: the servers it downloads and starts, the commands its manifest
      declares, and for a VS Code extension that its code runs unconfined
- [ ] Updates: newer versions are looked for when the tab is opened, never at
      startup; update one or all; keep a version
- [ ] Turn an extension off without removing it
- [ ] For each installed extension, what works and what does not, from what it
      asked for at run time and did not get, not from its manifest alone
- [ ] A section on extensions in `editor/README.md` and in the website's docs

### Zed extensions, complete
- [ ] Every language server an extension lists for a language, not only the
      first, with their answers merged (completions, diagnostics, actions).
      Vue needs this, and Dockerfile has two
- [ ] One extension setting up another's server
      (`language-server-additional-*`: Vue adds its plugin to the TypeScript
      server)
- [ ] The user's settings reach the extension: a server's binary, options and
      settings from `settings.json`, and the language's tab size
- [ ] The rest of a language's files: indentation, brackets, the outline,
      and from `config.toml` the pairs that close themselves, the block
      comment and the word characters
- [ ] Completion and symbol labels as the extension paints them
      (`labels-for-completions`, `labels-for-symbols`)
- [ ] The languages Zed builds in and so does not list in its catalog (C,
      C++, Markdown, YAML, shell): grammars and servers of our own for them,
      installed the same way
- [ ] Icon themes: file icons in the tree and on tabs
- [ ] A Node of Solder's own for servers written in JavaScript when the
      machine has none, downloaded on first need
- [ ] The commands an extension may run and the hosts it may download from,
      granted or refused per extension (Zed's capabilities)
- [ ] A time limit for a grammar's scanner, which today can hold a parse
- [ ] Debug adapters of extensions, in the debugger
- [ ] Slash commands and context servers of extensions, in the chat and the
      agent
- [ ] Each new version of Zed's API as it is published (0.8 is the next)

### VS Code extensions: what needs no code
- [ ] TextMate grammars: highlighting for the languages only VS Code has an
      extension for (needs a regular expression engine with look-behind,
      which is a dependency to decide on)
- [ ] Language configuration: brackets, pairs that close themselves, comments,
      indentation rules, and which files are which language
- [ ] Themes in the old `.tmTheme` format
- [ ] Icon themes, through the same file icons as Zed's
- [ ] The settings an extension declares, with their defaults, in
      `settings.json`
- [ ] Debuggers an extension declares with a program to start, in the
      debugger
- [ ] The build for this machine when an extension has one per platform, and
      the extensions it depends on or packs, installed with it

### VS Code extensions: their code
- [ ] An extension host: a Node process next to the editor that loads
      extensions when their activation events happen and gives them a
      `vscode` module of Solder's own. One extension that crashes or hangs
      does not take the editor or the others with it
- [ ] The API every extension starts from: commands, messages, pickers and
      input boxes, status bar items, output channels, configuration, the
      workspace's folders and files, open documents, their edits and events
- [ ] Language features: the providers for completion, hover, definition,
      references, rename, formatting, code actions, symbols, semantic tokens,
      inlay hints and code lenses, and diagnostic collections. This is what
      `vscode-languageclient` is built on, so it carries most language
      extensions (and Prettier and ESLint)
- [ ] Commands, menus and key bindings an extension contributes, in the
      palette and the keymap
- [ ] Debugging: adapters and configurations that extensions register in
      code, in the debugger
- [ ] Tasks, terminals and file watching
- [ ] Source control providers, views in the sidebar (trees), decorations and
      test controllers
- [ ] Webviews, custom editors and notebooks. They need a browser view inside
      the window, which GPUI does not have; decided when the rest is done
- [ ] A check that runs with each release: the most installed extensions of
      Open VSX are installed, activated and asked for their main feature, and
      the result is the list of what works

### What will not run
Not tasks, but said so nobody waits for them: extensions whose licence or
own checks tie them to Microsoft's products (Pylance, C# Dev Kit, Remote
SSH, Live Share), and anything published only on Microsoft's Marketplace,
which other editors may not use.
