# Solder editor

The native editor behind the website in `../src`. Rust, GPU-rendered UI, no Electron and no webview.

```
crates/
  text/     rope buffer, edits, undo history, cursor movement (no UI, fully unit-tested)
  syntax/   tree-sitter parsing and highlighting for Rust, TS/TSX, JS, JSON, CSS, Go, Python
  db/       database connections: detection, drivers, statement splitting (no UI)
  rest/     HTTP: .http files, route detection, OpenAPI import, sending (no UI)
  ai/       local models: hardware, catalog, downloads, llama-server, benchmark (no UI)
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

## Review changes

Open the Git sidebar with `ctrl-shift-g` and click a file to compare both versions.
Staged files compare HEAD with the index; Changes and Untracked compare the index
with the working copy, including unsaved text in open editors. Added and deleted
lines stay aligned with blank cells on the other side. Both columns share scrolling.

- `cmd-alt-d`: compare the current file with the index.
- `alt-up` / `alt-down`: previous / next change.
- `alt-left` / `alt-right` or horizontal scrolling: pan both code columns together.
- `cmd-r`: refresh the comparison after external changes.
- `enter`: open the working file; `escape` or `cmd-w`: return to the previous editor.

The comparison is read-only. Deleted files remain reviewable without a working
file; binary and non-UTF-8 files show an explanation rather than corrupted text.
Conflicted files still open the three-way conflict view.

## Run the stack

Open the Services tab with `cmd-shift-s` (`ctrl-shift-s` on Linux). Solder lists the
dev servers it finds: `dev`, `start` or `serve` scripts in any `package.json`
(with the package manager its lockfile implies), Go `main` packages, Rust binaries,
Django, FastAPI and Flask apps, and compose services. **Run stack** starts them all.

Each service runs in a terminal in the bottom dock, through your login shell so nvm,
asdf and pyenv behave as they do in your own terminal. Ports a service prints
(`http://localhost:3000`, "listening on port 8080") appear next to it and open in
the browser. A service that exits keeps its terminal so the error stays readable.
Stop sends Ctrl+C and closes the terminal if the service is still running after
three seconds.

Add or override services in `.solder/services.json`:

```json
{ "services": [{ "name": "api", "command": "make run", "dir": "apps/api", "ports": [8080] }] }
```

Containers are listed below the services when a Docker runtime is running: Docker
Desktop, OrbStack or Colima. Solder uses the `docker` CLI and its current context,
and finds OrbStack's CLI in `~/.orbstack/bin` even without the `/usr/local/bin` links.

## Databases

Open the Database tab with `ctrl-shift-d`. Solder lists the databases it finds:
connection URLs in `.env` files (Postgres, MySQL, SQLite, Redis, MongoDB,
Prisma's `file:` paths), database images in compose files with their published
ports and credentials, and SQLite files in the project. Nothing is read until the
tab is opened, and a connection opens the first time you expand it.

Expand a connection to see its tables, views, collections or keys. Click one to
list its columns in the tab and show its first 200 rows in the Results tab of the
bottom dock. In the grid, arrow keys move the selected cell and
`cmd-c` copies its value.

Connections from `.env.production`, or whose name contains "prod", are read-only:
the session itself refuses writes (Postgres and MySQL read-only transactions,
SQLite opened read-only, write commands blocked for Redis and MongoDB). Click the
**read-only** tag, or **Allow changes** in Results, to allow writes until Solder quits.

**New connection** takes a URL or a SQLite path and saves it to
`~/.config/solder/connections.json` (readable by you only), outside the project so
passwords are never committed. Shared connections belong in
`.solder/connections.json`, where `${VAR}` comes from the project's `.env`:

```json
{ "connections": [{ "name": "analytics", "url": "postgres://ro@${DB_HOST}/stats", "readOnly": true }] }
```

TLS uses rustls. For Postgres, `sslmode=require` encrypts without checking the
certificate, as libpq does; `verify-full` checks it against the Mozilla roots.

Use `sslrootcert=/absolute/path/ca.pem` for a private Postgres or Redis CA,
`ssl-ca=/absolute/path/ca.pem` for MySQL, or `tlsCAFile=/absolute/path/ca.pem`
with `tls=true` for MongoDB. Redis requires a `rediss://` URL. A CA file enables
certificate and hostname verification. `sslrootcert=system` uses the bundled
Mozilla roots and requires `verify-full`, not the operating system's custom roots.
MySQL enables verified TLS by default for non-local hosts. Unverified encryption
is available only through explicit URL settings; never use it to work around a
failed verification on a production database.

Prisma's `schema`, `sslaccept`, `sslcert` CA path and `pgbouncer` parameters are
normalized before connecting; pool-size settings are ignored because a session
uses one connection. Schema names must be a single alphanumeric identifier
(underscores and `$` are supported). Unknown TLS modes and unsupported client
certificate/PKCS12 options fail instead of silently disabling verification.
Certificate paths must be absolute or relative to the process working directory.
Behind PgBouncer, `pgbouncer=true` disables named prepared statements used to
inspect result types; returned values are then displayed as text.

### Check a connection

The read-only probe uses the same drivers as the editor. It checks a connection,
a small query and the schema, with bounded timeouts. It omits credentials and
query parameters from displayed URLs and redacts passwords from errors.
`--require-tls` requires encryption before authentication and fails if a network
session is not encrypted. It does not upgrade an explicitly unverified mode into
certificate verification: use the verified URL settings above.

With a URL already set in `SOLDER_PROBE_URL` in your terminal:

```bash
cd editor
cargo run --profile ci --locked -p db --example probe -- --require-tls
```

Or pass a project folder to check detected connections without requiring TLS for
local databases:

```bash
cargo run --profile ci --locked -p db --example probe -- /path/to/project
```

For Postgres and MySQL the probe reads TLS status from the server; for Redis and
MongoDB it reports the TLS settings enforced by the driver, not a negotiated
cipher or TLS version. Checking a `mongodb+srv` URL includes its real DNS lookup;
the local tests do not replace a connection to Atlas or another hosted service.

### Queries

`cmd-enter` in a `.sql` file runs the statement under the cursor, or the
selection, and shows the rows in Results while the cursor stays in the editor.
`.redis` files run the command on the current line and `.mongodb` files the shell
call under the cursor (`db.users.find({age: {$gt: 30}}).sort({name: 1})`). A file
runs on the connection it was given; a new file gets the last connection used or
the only one that fits, and otherwise asks. The connection shows in the status
bar; click it (or run **Select connection**) to change it. **Query** on a
connection opens its scratch file, kept in `~/.config/solder/scratch`.

Completion comes from the schema: columns of the tables in the statement (with
aliases and `table.`), tables after `FROM`, `JOIN`, `INTO` and `UPDATE`, index
names after `INDEX`, and keywords. Redis files complete commands and keys, MongoDB
files collections, methods and fields.

### Browsing tables

A table or collection opened from the Database tab is browsed rather than queried:
rows load 200 at a time as you scroll, and the status line counts all that match.
Type a condition in the **WHERE** field (`cmd-f`, `enter` applies; a filter
document such as `{status: "paid"}` for MongoDB), or press `alt-f` on a cell to keep
rows with that value. Click a column header to sort by it, again for descending,
a third time for the default order (the primary key). Foreign key columns show
the table they point at; `alt-enter` on such a cell opens the row it references and
`alt-left` goes back. Browsed SQL tables stay editable.

### Table structure

**Structure** on a table in the Database tab (or **Table** on a connection, for a
new one) opens a form in place of the editors: the table's name, its columns (name,
type, default, required, key), indexes and foreign keys (with ON DELETE). Renaming a
column carries its indexes and keys along. Nothing runs while you edit.

**Review** (`cmd-s`) shows the DDL for Postgres, MySQL or SQLite, and the DDL that
undoes it. Then:

- **Apply to database** runs it. Postgres and SQLite run it in one transaction;
  MySQL commits each structure change as it runs, which the review points out. SQLite
  cannot change a column's type, nullability or default, keys or foreign keys in
  place, so those rebuild the table: a new table, the rows copied over, a swap.
- **Save as migration** writes it into the project in the format it already uses:
  Prisma (`prisma/migrations/<time>_<name>/migration.sql`), Drizzle (the next
  numbered file plus its journal entry), Supabase, up/down pairs (golang-migrate and
  similar, with the undo as the down file), dbmate (`-- migrate:up` and `down`), or
  timestamped SQL files in `migrations/`. The file opens in the editor.

### ERD

**ERD** on a connection shows its tables and views with their columns, and foreign
keys as lines: referenced tables to the left, tables pointing at them to the right,
tables without relations after them. Drag the background (or scroll) to pan,
`cmd`-scroll or `cmd-=`/`cmd--` to zoom, `cmd-0` to fit. Drag a table by its header
to move it; the layout is saved in `.solder/erd/<connection>.json`, so it can be
committed and shared. **Find a table** highlights matches and `enter` centers the
first.

Select a table to see its relations: **Data** browses its rows (so does a double
click), **Structure** opens its form, **Join** runs a query joining the two tables
of a relation. **New table** opens an empty form. Drag from a column's right edge
onto a column of another table to add a foreign key: the structure form opens with
it drafted, to review and apply or save as a migration like any other change.

**Mermaid** copies the diagram as a Mermaid `erDiagram` for docs and pull requests;
**SVG** and **PNG** save it as a file.

### Redis keys and MongoDB documents

Documents from a collection (browsed from the Database tab, or any `find` that
keeps `_id`) are edited like rows: a changed cell becomes `updateOne` with `$set`,
a deleted row `deleteOne`, an added row `insertOne` (MongoDB fills in `_id`).
Values are read as the shell reads them (`37` is a number, `{a: 1}` an object), but
a text field stays text unless you type quotes, braces or `ObjectId(...)`. MongoDB
without a replica set has no transactions, so changes run one by one and a failure
leaves the earlier ones saved, which the review says.

A Redis key opened from the Database tab shows its type and expiry above its value,
with **Expire** (seconds, or never), **Rename** and **Delete key**. Its members are
edited in the grid: hash fields and values, list items, set members, sorted set
members and scores, or a string's value (its expiry is kept). Changes run as one
MULTI/EXEC. **Key** on a connection creates a key: type its name, pick its type,
fill in the first member and apply.

### Editing rows

Rows from a plain `SELECT` on one table with a primary key can be edited in
Results (Postgres, MySQL, SQLite). Select a cell and press `enter` (or double-click)
to edit it, `enter` again to keep the value, `escape` to drop it. `shift-backspace`
sets NULL and `cmd-backspace` marks the row for deletion. Nothing is written yet:
changed cells are highlighted and deleted rows struck through.

`cmd-n` (or **Add row**) adds a row at the bottom and `cmd-d` copies the selected
one. Columns you leave alone get their defaults (shown as DEFAULT), and columns the
server fills in (serial, identity, auto_increment, SQLite's rowid) are left out of
new rows. In a foreign key column `enter` opens a picker of the rows it can point
at, searchable by key and name; `f2` types the value instead.

**Review** (`cmd-s`) shows the SQL that will run, with the values each update
replaces. **Apply in one transaction** runs it all or nothing. Every statement
targets one row by its primary key and the values the grid showed, so a row that
someone changed or deleted since it was read stops the save and nothing is written.

## HTTP requests

Requests live in `.http` files, the format VS Code's REST Client and JetBrains
use: requests separated by `###`, a method and URL, headers, a blank line, then
the body. `@name = value` defines a variable and `{{name}}` uses it; names the
file does not define come from the project's `.env`. `cmd-enter` sends the
request under the cursor and shows the status, time, size, headers and body
(JSON pretty-printed) in the Response tab of the bottom dock.

```http
@baseUrl = http://localhost:3000

### create a user
POST {{baseUrl}}/api/users
Content-Type: application/json

{"name": "Ada"}
```

The API tab (`ctrl-shift-h`) lists the routes the project serves, read from its
code: Next.js route handlers and pages API, Express, Fastify, Hono and similar
routers, FastAPI, Flask, Django, Go's `net/http`, chi and gin, axum and actix.
Operations from OpenAPI files in the project (`openapi.yaml`, `swagger.json`,
`*.openapi.json`) are listed too. Click a route to add its request to the
project's requests file (kept in `~/.config/solder/scratch`, outside the project)
with the cursor on it; **Send**, shown on hover, sends it without writing
anything. `{{baseUrl}}` points at the port of a running service from the
Services tab, else `PORT` in `.env`, else the framework's usual port.

**Import OpenAPI** turns an OpenAPI 3.0 or 3.1 file (JSON or YAML) into a `.http`
file next to it, one request per operation, with example bodies built from the
schemas and `$ref`s resolved. Swagger 2.0 is not supported.

## Local models

The AI tab (`ctrl-shift-a`) shows what this machine can give a model: the
memory a model may use (on Apple Silicon, the share macOS lets the GPU wire;
elsewhere the GPU's memory, or part of system memory) and the free disk space.

**Run benchmark** downloads llama.cpp's server (a pinned build, about 12 MB)
and a 0.8 GB test model, then times how fast it reads a prompt and writes
tokens. Writing speed is bound by memory bandwidth and reading speed by
compute, so that one measurement predicts every other model: each model in the
list shows whether it fits and about how fast it would write. The best model
that stays usable (15 tokens per second writing, 150 reading) is marked for
chat; the best one fast enough for completions (40 and 600) that fits next to
it is marked for completions.

**Install recommended**, or **Install** on any model that fits, downloads it
from Hugging Face. Every file is checked against its SHA-256 while it streams
(the server against the hash GitHub publishes for the pinned build, a model
against the one Hugging Face publishes) and only then gets its name; an
interrupted download resumes. After a download the model is measured for real,
and that measurement replaces the prediction. Installed models can be given
the chat and completion roles, or deleted.

The catalog has fifteen models from 0.8B to 120B parameters (Qwen3.5, Qwen3.6,
Qwen3.8, Qwen3 Coder, Gemma 4, gpt-oss, Nemotron 3.5), smallest first. Their
parameter counts, active parameters and cache sizes come from each file's GGUF
header, so mixture-of-experts models are predicted by the few experts a token
uses, and hybrid and sliding-window models by the cache they really keep.

Any other model can be added: type a Hugging Face repository (`org/model-GGUF`,
or `org/model-GGUF:Q8_0` for a quantization), paste a link to a `.gguf` file, or
type a path or pick a file with **File...**. Solder reads the file's header (for
Hugging Face, the first megabytes with a range request) to know its size,
parameters and cache, then lists it with the others: it can be installed,
measured and given a role, but is never recommended, since its quality is
unknown. Models LM Studio has downloaded are found and listed too, and run where
they are. Models split across several files are not supported yet.

Everything lives in `~/Library/Application Support/Solder` (on Linux,
`~/.local/share/Solder`), shared by all projects. The server listens on
`127.0.0.1` only, on a free port, with a key made for each run.

## AI providers and chat

The AI tab's **Providers** view lists local llama.cpp, Ollama, LM Studio,
Anthropic and OpenAI. Add another OpenAI-compatible service with its name,
address up to `/v1` and an optional key. Choose separate models for chat and
completions; two local models can run at once, so each keeps its own server.

Keys come from `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`, or from the provider's
key field. Saved keys use macOS Keychain or Linux Secret Service when available;
otherwise they live in a separate `keys.json` readable only by you on Unix.
Provider settings contain no keys. **Offline** disables providers outside the
local machine.

Open chat with `cmd-shift-l` (`ctrl-shift-l` on Linux and Windows). Pick a
model, type a question and press Enter. The current file or selection is attached
unless **File** is turned off; at most 24,000 characters are included. Files and
directories whose names start with `.env` are never attached, including selected
text. Files outside the project, symbolic links, Git-ignored files and build
folders are also excluded.

**Project map** is off by default. Turn it on to attach relative paths and
declaration names with line numbers for Rust, TS/TSX, JS, Go and Python. It does
not attach their bodies or let the model read other files. The map is built on
request in the background, preferring paths mentioned in the question and files
near the current file. Each request includes a fresh map, not copies of old maps.
It is capped at 12,000 bytes and 1,000 files; **limited** means some entries were
omitted. Scanning has a 20,000-entry limit and a two-second budget checked
between entries; source reads are capped at 256 KiB per file and 2 MiB per
request, with 16 declarations per file.

Add `.solderignore` at the project root to exclude additional paths from both the
map and file/selection attachments, using Git ignore syntax:

```gitignore
private/
*.pem
src/generated/*
!src/generated/types.ts
```

Only the root `.solderignore` is used. Negations cannot re-include `.env*`,
Git-ignored files or build folders. Rules are checked before every question. If
a file used earlier in the conversation is now excluded or missing, Solder asks
for **New chat** rather than re-sending that file or answers derived from it.
Unreadable or invalid rules block attachments, not silently disable exclusions.
Exclusions do not redact text you type yourself or retract previous requests.

Answers stream into the right panel, with reasoning shown separately and copy
buttons on code blocks. **Stop** or Escape cancels the request even when the
provider is waiting to send data. A local model's server starts on first use;
**New chat** clears the conversation and cancels an answer in progress.

### Inline edits

Press `cmd-i` (`ctrl-i` on Linux and Windows) in a file, or use **Show Inline
Edit** in the command palette. Describe a change and press Enter to generate a
preview using the chat model. A selection is replaced exactly; without a
selection, the whole file is the target. Save untitled files first. Use a single
selection of at most 24,000 characters, or a file within that limit.

The request contains the target and up to 2,000 characters on each side, with an
optional **Project map**, off by default. The same context exclusions apply as
in chat, including `.env*`, Git ignores and `.solderignore`. The file must be
writable and inside the project. Exclusions are checked again before applying.

Text streams without changing the document. A completed answer becomes a
side-by-side **Before / Proposed** diff. **Apply**, or `cmd-enter` (`ctrl-enter`),
applies it as one undo step without saving. **Stop** or Escape cancels an active
request; **Discard** closes the preview. Switching tabs or panes cancels the
edit. Changing the document invalidates the proposal instead of overwriting
newer changes, including edits from another split view.

Malformed, disconnected, token-limited and oversized answers cannot be applied.
This is a single-target text edit, not an agent: it cannot run commands, change
other files or save on its own.

### Completions as you type

With a model chosen under **Completions** in the Providers view (or the
**complete** toggle on a local model), a pause in typing at the end of a line
asks it for what comes next. The suggestion streams in as faint text after the
cursor; lines after the first are drawn over the rows below. Tab inserts it as
one undo step, Escape dismisses it, and typing what it suggests keeps the rest.
Moving the cursor or any other edit drops it. **as I type** turns them off.

Local models fill in the middle through llama.cpp's `/infill`: they see the code
before and after the cursor (up to 6 KB and 2 KB). A model without
fill-in-the-middle tokens, and every other provider, is asked through chat for
the missing text only, with any code fence and repeated start of the line
removed. Files `.env` or `.solderignore` keeps from AI are never sent; the check
reads only the file's own rules, not the whole project, so it costs nothing
noticeable while typing.

### Agent tasks

The **Agent** tab of the right dock (`cmd-shift-i`, `ctrl-shift-i` elsewhere)
takes a task in a sentence and works on it with the chat model, on a branch and
Git worktree of its own (`solder/agent/<task>`, from your last commit, kept in
Solder's data folder). Your checkout and uncommitted changes are not touched.

1. **Plan.** The agent explores with read-only tools (list, read, search) and
   proposes steps. Edit them, add or remove steps, or tell it what to change,
   then **Approve plan**. Nothing changes before that.
2. **Work.** It edits files, runs commands (tests, builds, linters), fixes what
   fails and ticks the plan off. Commands run in a sandbox: no network beyond this
   machine and no writes outside the worktree and temporary folders
   (`sandbox-exec` on macOS, `bwrap` on Linux). A command that needs the network
   (package caches become writable too) or the whole disk says why and waits for
   **Allow once** or **Don't run**. Without a sandbox every command waits.
3. **Review.** When it finishes it lists the changed files; click one for its
   diff against where the task started. **Merge into my branch** commits the work
   on the agent's branch and merges it into yours (Git refuses rather than
   overwrite uncommitted changes in the way); **Discard** deletes the worktree and
   the branch.

Type in the field at any time to steer it: after the plan, while it waits for a
command (which declines that command) or after it finished. **Stop** halts it
between steps. Files `.env` and `.solderignore` keep from AI stay unreadable to
it, as in the chat; a task stops after 60 steps and asks how to go on.

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
