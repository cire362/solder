# Roadmap

Everything the website promises, grouped into phases. Each phase only depends on
the ones above it. Optimization work (see README) starts after phase 7.
Phase 8 goes past what the website promises: it is what it takes for an
extension made for Zed or VS Code to be installed with one button and work
as it does in the editor it was made for. Phase 9 makes the window the
user's own: where every panel and button is, how it looks and which keys
drive it. Phase 10 is what an editor used all day is expected to have and
this one does not have yet.

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
- [x] Context servers for the agent (MCP): servers named in `settings.json`
      are started with a task, their tools are offered to the model next to
      the agent's own, and the first call of each waits for the user
- [ ] Context servers reached over the network (HTTP), and what a server has
      besides tools: prompts and resources

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
      it
- [x] Extensions built for every version of Zed's API so far, 0.0.1 to 0.7:
      each of its nine worlds has its bindings, over one implementation
- [ ] The plugin registry (needs the hosted service)
- [ ] Preview environments per branch (needs the hosted service)

## 8. Extensions, in full

Where it stands: extensions install from Zed's catalog and Open VSX. A Zed
extension brings its languages (highlighting and how they are typed),
snippets, themes and language servers. A VS Code extension brings its themes
and snippets only.

The two kinds are different work. A Zed extension is data plus a small
sandboxed program with a fixed interface, so it can be supported completely.
A VS Code extension is a Node program written against VS Code's own API: it
runs here only as far as that API is rebuilt here, one area at a time, and
it runs with the user's rights, not in a sandbox. The items are in the order
to build them.

### The Extensions tab
- [x] Extensions as a tab of the sidebar, next to Files and Git, in place of
      the window: installed, Zed's catalog and Open VSX in one list, search,
      one Install button, progress and errors on the row
- [x] Before the first install of an extension, what it will do outside a
      sandbox: the servers it downloads and starts and the commands its
      manifest declares. It is downloaded and waits until that is allowed; a
      later version asks only for what is new. (A VS Code extension's code
      does not run yet, so it has nothing to ask.)
- [x] Updates: newer versions are looked for when the tab is opened, never at
      startup; update one or all; keep a version
- [x] Turn an extension off without removing it
- [x] For each installed extension, what works and what does not, from what it
      asked for at run time and did not get, not from its manifest alone:
      what its code ran, installed and downloaded since the app started, what
      it was refused, and which server it could not get ready
- [x] A section on extensions in `editor/README.md` and in the website's docs

### Zed extensions, complete
- [x] Every language server an extension lists for a language, not only the
      first, next to the one Solder knows for it, with their answers merged
      (completions, diagnostics, actions). Dockerfile has two
- [x] One extension setting up another's server
      (`language-server-additional-*`): Vue adds its plugin to the TypeScript
      server, which then serves Vue files too and is started again if it ran
      without the plugin; the questions Vue's server has for it pass through
      the editor
- [x] The user's settings reach the extension: a server's binary, options and
      settings from `settings.json`, and the language's tab size. The editor
      also applies them itself to a server an extension brings, for the
      extensions that do not ask
- [x] The rest of a language's files: indentation (`indents.scm` and the
      two patterns), brackets, the outline (in the project map the AI gets;
      there is no outline to look at yet), and from `config.toml` the pairs
      that close themselves, the block comment and the word characters
- [x] Completion labels as the extension paints them
      (`labels-for-completions`): colored as code in the file's language,
      with the typed word matched against the part that is the name
- [ ] Symbol labels (`labels-for-symbols`). The editor has no list of a
      file's or a project's symbols to paint them in; it comes with one
- [x] The languages Zed builds in and so does not list in its catalog (C,
      C++, Markdown, YAML, shell): built in here too, as grammars in the
      binary, with the servers Zed has for them (clangd, yaml-language-server)
      taken from the PATH. Markdown is two grammars and the languages of its
      code blocks; a paragraph that did not change is not parsed again
- [x] Icon themes: file icons in the tree and on tabs, chosen in the
      Extensions tab or by `icon_theme` in `settings.json`
- [x] A Node of Solder's own for servers written in JavaScript when the
      machine has none, downloaded on first need: the newest long-term
      release, checked against its published hash, with the npm it brings
- [x] The commands an extension may run and the hosts it may download from,
      granted or refused per extension (Zed's capabilities): commands, npm,
      downloads and single hosts, each taken back or given again in the
      extension's details
- [ ] A time limit for a grammar's scanner, which today can hold a parse.
      tree-sitter 0.26 makes the WebAssembly store itself and has no way to
      put a deadline on it (with interruption turned on in the engine, its
      store would trap at once). What is left is to reach into its store, to
      parse such grammars off the UI thread and give up on one that hangs,
      or to change tree-sitter. Decided on 2026-10-05: left open until
      tree-sitter has a way of its own, as Zed has no such limit either
- [x] Debug adapters of extensions, in the debugger: the open file with
      each adapter its language names, started as the extension says, over
      a port or the adapter's own input and output
- [ ] Which of a language's servers start. Every server an extension lists
      for a language is started today, and Ruby's lists eight: the user's
      choice per language (`language_servers`, with `!name` to leave one
      out), and Zed's defaults for the languages that need them
- [x] Context servers of extensions, in the agent. They are MCP servers
      (context7, GitHub and Postgres are the most installed): the extension
      says how to start one and reads the user's settings for it, and the
      agent uses its tools like those of a server named in `settings.json`
- [ ] Each new version of Zed's API as it is published (0.8 is the next;
      0.7.0 was still the newest on 2026-10-05)

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
Not tasks, but said so nobody waits for them: slash commands of Zed
extensions, which no extension in Zed's catalog has (2026-10-05), so there is
nothing to run them for; extensions whose licence or own checks tie them to
Microsoft's products (Pylance, C# Dev Kit, Remote SSH, Live Share); and
anything published only on Microsoft's Marketplace, which other editors may
not use.

## 9. An editor shaped by its user

Where it stands: keys are the user's (`keymap.json`), and so are the colors
(a theme file, 30 tokens). The rest is fixed in code: the sidebar is on the
left and 390 px wide, the chat on the right, the terminal at the bottom, the
sidebar's tabs and the status bar's items in one order, the interface font
and its size, and the only icons are those of files, from an extension's
icon theme; buttons and panels are words.

How it is built: `layout.json`, next to `settings.json`, is the truth and is
read again when it is saved. Whatever is done by hand in the window (a border
dragged, a tab moved, a button hidden from its menu) is written to the same
file, so the two never disagree. It starts after the Zed items of phase 8.

### The layout file
- [x] `layout.json`: the sizes that were constants (sidebar, chat, terminal
      dock, title bar, tab bar, status bar), read at start and when the file
      is saved. A mistake in it is reported like one in `settings.json`, and
      the last layout that was right stays
- [x] Docks on the left, right and bottom, and any panel in any of them:
      Files, Search, Git, Services, Database, API, AI, Extensions, the chat,
      the agent, the terminal, the debugger. Their order in a dock, which
      are hidden, which one is open. Every dock holds any of the fourteen
      panels (the terminals, the debugger, the response and the results
      with the ten of the sides), in the file's order, with hidden ones.
      `open` names the panel each dock has open and is written when that
      changes, so the window starts as it was left
- [x] The title bar and the status bar as lists of items: which are there, in
      what order, on which side. The tab bar: above or below the editor, or
      none. Eleven items, two of them new (`file`, `branch`)
- [ ] An item is drawn as a word, an icon or both, and its command and icon
      can be changed, so a button for any command can be put on a bar
- [x] The interface font, its size and how dense rows are, in
      `settings.json` (`ui_font_family`, `ui_font_size`, `ui_density`)

### By hand
- [x] Borders are dragged to resize; a double click puts a size back
- [~] A panel's tab is dragged to another place in its dock or to another
      dock; an item of a bar is dragged along it or to the other bar. Tabs
      are, onto a closed dock too; the bars wait for their lists of items
- [~] A menu on every panel tab and bar item: hide, move to, and the list of
      what is hidden, to bring it back. Tabs have it; bar items not yet
- [x] A command that puts the layout back as it came: Reset Layout, which
      puts the file aside as `layout.json.old`

### Layouts by name
- [ ] Several layouts kept by name (writing, review, debugging), switched by
      a command or a key; the one a project was left in is the one it opens
      with
- [ ] Key layouts by name: the built-in one, the ones that follow other
      editors, and the user's own, switched by a command, with `keymap.json`
      still on top

### Themes
- [x] Every color in the window comes from a token a theme can set: the ones
      still written in code become tokens. They were the terminal's sixteen;
      themes from VS Code and Zed bring theirs
- [~] Shapes as tokens next to colors: the radii, the spacing, the width of
      borders. The radii and the width of borders are (`shapes` in a theme
      file and in `theme_overrides`). Spacing is not a theme's: it is in
      rems like the text, so it follows `ui_font_size`, and a token of its
      own would need every text size to know of it
- [x] A theme file is applied as it is saved, and any token can be set in
      `settings.json` on top of the theme in use (`theme_overrides`)

### Icons
- [ ] A set of icons of Solder's own for buttons, panels and the bars
      (Phosphor, MIT, drawn from files kept in the app, so nothing is
      downloaded)
- [~] File icons in the tree and on tabs from the icon themes of Zed and
      VS Code extensions (the items of phase 8), chosen in `settings.json`.
      Zed's are in; VS Code's are not yet

## 10. What a daily editor has

Taken on 2026-10-09 from the checklist of another editor's rewrite, read
against what Solder has: these are the parts it lacked. Each block is one
update. They come after phase 9, in this order unless one is needed sooner.

### The editor
- [ ] Folding of blocks, indent guides, lines wrapped at the window's edge
- [ ] Selection by column (a rectangle of cursors)
- [ ] Tabs and cursors restored when a project is opened again; pinned tabs;
      the tab closed last opened again (`cmd-shift-t`)
- [ ] Unsaved text kept aside as it is typed and offered back after a crash
- [ ] Line endings and encoding of a file kept as they were; files too large
      to edit opened for reading, with the reason said

### Symbols and problems
- [ ] Symbols of the file (`cmd-shift-o`) and of the project (`cmd-t`), from
      the language server, painted as an extension says (`labels-for-symbols`)
- [ ] A Structure panel: the file's outline, from the server or from the
      language's outline query
- [ ] A Problems panel: every diagnostic of the project, by file
- [ ] The other places a symbol is used lit up; go to implementations;
      breadcrumbs with the path and the symbol under the cursor

### Git history
- [ ] The history as a graph, the history of one file, blame in the gutter
- [ ] Stash and fetch
- [ ] A check before a commit: keys and secrets in what is staged stop it
- [ ] A commit message proposed by the model from what is staged
- [ ] The pull request of the branch and the state of its checks, through `gh`

### Tests and running
- [ ] A tree of the project's tests: run one, see what failed, go to its line
- [ ] Breakpoints with a condition, and ones that only write to the console
- [ ] Python under the debugger (debugpy), now that an adapter is only
      something to start

### The terminal
- [ ] `file:line:column` in the output opens in the editor
- [ ] Search in what scrolled by; the dock split in two
- [ ] A command that failed offered to the model to fix

### Windows and the way in
- [ ] A first window with the projects opened last, open a folder, clone
- [ ] Several windows, a project in each, the set restored
- [ ] Markdown shown next to its source; pictures shown as pictures
- [ ] A log of errors with rotation, and a report of a crash at the next start
- [ ] The interface in Russian as well as English

### Projects that are somewhere else
- [ ] A project on a server over SSH: files, terminal, Git, search and
      language servers there (needs an SSH client, a dependency to decide on)
- [ ] Dev containers: `devcontainer.json`, with terminals, services and the
      agent inside

Left out on purpose: notebooks, a list of servers and deploys (far from an
editor for what they cost), settings synced through a folder (they are files
already), and moving data over from an Electron version (there is none).
Accessibility through the system's own tree and installers with updates
are wanted, but the first waits for GPUI and the second is publishing, which
is decided apart.
