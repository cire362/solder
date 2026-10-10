# Solder editor

The native editor behind the website in `../src`. Rust, GPU-rendered UI, no Electron and no webview: the one place a browser view is made is a page a VS Code extension opens, and only when it opens.

```
crates/
  text/     rope buffer, edits, undo history, cursor movement (no UI, fully unit-tested)
  extension/ extensions of Zed and VS Code: manifests, the two catalogs, installing,
            the sandbox that runs a Zed extension's code to get its language server,
            and the Node host that runs a VS Code extension's code
  syntax/   tree-sitter parsing and highlighting for Rust, TS/TSX, JS, JSON, CSS, Go, Python,
            C, C++, Markdown, YAML and shell, and for the languages of extensions
            (grammars in WebAssembly)
  db/       database connections: detection, drivers, statement splitting (no UI)
  rest/     HTTP: .http files, route detection, OpenAPI import, sending (no UI)
  ai/       local models: hardware, catalog, downloads, llama-server, benchmark (no UI)
  import/   settings, themes and key bindings of VS Code, Cursor, Zed and JetBrains IDEs (no UI)
  plugin/   plugin host: manifest, permissions, the WebAssembly sandbox, its budget, WASI programs and the JavaScript engine (no UI)
  plugin_sdk/  what a plugin is written against, and the messages it exchanges with the editor
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
rustup target add wasm32-unknown-unknown   # once: the plugin tests build real plugins
cargo test --workspace
```

```bash
scripts/bench.sh [file]
```

The status bar shows live input latency, frame time, memory and startup time. Toggle it with `cmd-alt-p`.

## Editing

Several cursors: `alt`-click adds one, `cmd-alt-up` and `cmd-alt-down` add
one on the line above or below, `cmd-d` selects the next place the selected
word is.

**Indent guides** are thin lines down each level of indentation the rows
on screen are inside, in steps of the file's own indent. A blank row is as
deep as the deeper of the rows with text around it. `"indent_guides":
false` in the settings turns them off.

A block is **folded** under its first line: the lines after it that are
indented deeper than it, up to the last of them with text, are taken off
the screen, and a mark after the line says they are there. This goes by
indentation alone, so it is the same in every language, and a closing
brace on its own line stays in view.

| | |
|---|---|
| `cmd-k cmd-[` | Fold the block the cursor is in |
| `cmd-k cmd-]` | Unfold at the cursor's line |
| `cmd-k cmd-0`, `cmd-k cmd-j` | Fold every block that is in no other; unfold all |
| A click between a line's number and its text | Fold or unfold there. Lines that can be folded show a mark while the pointer is over the gutter |
| A click on the mark after a folded line | Unfold |

The cursor steps over what is folded. A cursor that is put inside it (a
definition gone to, a match found, an undo) opens it, and so does an edit
of its lines; an edit elsewhere moves it with its lines. Folds are of one
view of the file and are not kept when its tab is closed. `cmd-alt-[` and
`cmd-alt-]`, which fold in other editors, move between changes here.

**Lines too long for the window** go on in the next row when wrapping is
on: `alt-z` turns it on and off for the view in front, and `"soft_wrap":
true` in the settings turns it on for every file. A row ends after the
last space that fits, and inside a word only where the word alone is
longer than a row. The rows a line goes on in are drawn as far in as the
line begins, so they are seen to belong to it. Nothing is to the side
then, and the cursor moves up and down by rows of the screen. A line is
wrapped by cells, not by what its letters measure: every character is one
cell. So a row of characters wider than a cell (Chinese, Japanese, Korean)
runs past the edge, and so does a row with inlay hints in it, which are
not counted. Home and End go to the ends of the line, not of the row. The
choice made with `alt-z` is not kept when the tab is closed.

A **selection by column** is a rectangle of cursors, one on each line it
crosses. With the mouse, hold `shift` and `alt` and click or drag: the
rectangle goes from the cursor to the pointer. Or drag with the middle
button, from where it went down. With the keys, `cmd-alt-shift` and an
arrow moves the free corner a line or a cell at a time (`ctrl-alt-shift`
on Linux and Windows). Cells are counted as they are on screen, so a tab
takes as many as it is wide, and a corner may be past the end of its line.
A line that ends before the left edge gets no cursor, unless the rectangle
has no width yet.

A file is **saved as it was written**. It is read in the encoding it is
in and saved in that one: UTF-8 with or without its mark, UTF-16 with its
mark, Windows-1251 and Windows-1252. The last two give a character for
every byte, so a file in another single-byte encoding is shown with the
wrong letters and still saved byte for byte as it was read. What a file
that is not UTF-8 is in is told by its letters: words of them are Cyrillic,
single ones among ASCII letters are Western. Where that is wrong,
**Workspace: Reopen with encoding** reads the file again as another, and
**Workspace: Save with encoding** saves it as another from then on. A
letter the encoding has no byte for is never written as another one: the
file is not saved, and a line above the text says which letter. Lines end
in the file as they did: a file with both endings keeps the one most of
its lines have. **Workspace: Use LF line endings** and **Use CRLF line
endings** change it. The status bar names the encoding and `CRLF` where
they are not UTF-8 and LF.

A file that is no text (it has a zero byte) is shown and cannot be edited,
and so is one larger than 64 MB; one larger than 512 MB is not read. The
line above the text says which and why. Edits a language server or an
extension makes to a file that is not open are written in that file's own
encoding too.

**Text that is not saved is kept aside** as it is typed, for whatever ends
the editor without asking: a crash, the power, or `cmd-q`, which asks
nothing. Within a second of a change the whole text of the file goes to
`sessions/recovery/` beside the settings; saving the file, closing its
tab, or closing the window with an answer about its changes removes it. What is found there at the next start was neither
saved nor let go, and is put back: into the tab of its file, into a new
tab where its file is gone or it never had one. It comes back as changes
that are not saved, with a line above the text that says so, and one undo
from it is the file as it is on disk. Nothing is written to the file
itself until you save. A text over 16 MB is not kept.

A tab can be **pinned** (`cmd-k shift-enter`, or **Workspace: Toggle pin
tab** in the palette): it goes before the tabs that are not, shows a pin
where they have their cross, and `cmd-w` and the middle button leave it
open. A click on the pin lets it go. Pinned tabs are remembered with the
rest of the project's tabs. `cmd-shift-t` opens again the file of the tab
closed last, with its cursors and where it was scrolled to, then the one
closed before it, up to 32 back; a file that is open already or gone from
disk is passed over. Closed tabs are remembered while the window is open,
not across a restart.

## Structure and breadcrumbs

**Show structure** (in the palette, or the **Structure** tab of the left
dock) lists the symbols of the file in front in the order they are written,
each as far in as it is inside others, with the one the cursor is in marked.
A click on one, or Enter with the arrows in the list, puts the cursor on
its name. The **breadcrumbs** in the title bar say the same of the cursor:
the way to the file from the project's folder, then the symbols it is in.

Both read one outline. It comes from the file's language server where one
lists symbols, and from the language's own outline where none does, or
where the server lists nothing for the file. An outline of that second kind
says only where each symbol begins: its symbols are one under another, none
inside another, and each reaches to where the next begins. The outline is
found again a quarter of a second after the file last changed, and not at
all while neither the panel nor the breadcrumbs are on screen. Up to 5000
symbols of a file are kept.
When a server starts later, its outline replaces the fallback without an
edit or a cursor move; changes made through another view refresh it too.

**The other places a symbol is used** are lit up once the cursor has rested
in its name for a fifth of a second: the file's server is asked, and what
it names is drawn behind the text in the color of a matching bracket. They
stay while the cursor moves among them, go at once when it leaves or the
text changes, and are not asked for per key. `"occurrence_highlights":
false` turns them off. `cmd-f12` goes to **the places that implement** what
is under the cursor (the types that are a trait or an interface): to the
one place where there is one, to a list where there are several.

## Problems

**Show problems** (in the palette, the **Problems** tab of the bottom dock,
or a click on the count of problems in the status bar) lists what the
language servers report of the project's files, by file: errors, warnings
and the rest, in the order they are in each file. A file need not be open
to be in the list. A server says what it finds in the files it read itself,
and those reports used to be dropped for every file that had no tab. A
click on a problem, or Enter on it, opens its file with the cursor on what
the server pointed at; a click on a file's row, or the left and right
arrows, folds its problems away and brings them back.

The list is what servers said last, each server's own apart: a server that
stops takes its problems with it. It is read only while its tab is in
front. Up to 500 problems of a file from one server and 2000 files are
kept. The dock does not stay open for this list alone: closing the last
terminal closes the dock as before.

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

## Git history

The Git panel's **History** shows the graph across local and remote branches.
**File history** follows the file in front through renames on the current
branch. Each reads the latest 200 commits; **Older commits** adds 200 and
**Refresh** rereads them. Click a commit to open its message and patch as a
read-only tab (the first 500 KB of a large patch).

**Blame** toggles a hash and author beside each line of the file in front, in
all of its views. It uses the current text, including unsaved changes; new
lines say **Not committed**. It is refreshed after typing pauses and when
Git status changes. Turning it off removes its work and its gutter. History
and blame use the Git CLI on background threads; neither loads at startup.

**Stash** saves tracked changes and untracked files on disk. **Stashes**
opens the list: **Apply** keeps the stash, **Pop** removes it after a successful
apply, and **Delete** asks before discarding it. Apply and pop restore the
index too. A conflict leaves the stash available. Unsaved editor text stays
in the editor and is not included: save it first to stash it. **Fetch** updates
all remotes without merging. Git uses existing credentials and reports an
error if it needs a terminal prompt. Operations run one at a time.

Before **Commit** or **Amend**, the editor scans the contents of changed staged
files for private keys and known credential formats. A match stops the commit
and names the file, line and kind; it never shows the credential. Cleaning only
the working file is not enough: stage the cleaned file too. Read failures and
staged files over 5 MB also stop the commit. This is a guard for recognizable
keys, not a promise to find every password. It applies to editor commits;
commands typed into a terminal use Git's own hooks.

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

## Debugging

Click a line number (or press `F9`) to set a breakpoint, then `F5`. The Debug
tab in the bottom dock lists what can run: the open JavaScript or TypeScript file
with Node, and every `package.json` script with the package manager its lockfile
implies (scripts in subfolders are named after them, like `api: pnpm run start`).
A Next.js `dev` script also comes as **+ browser**: the server runs under the
debugger and, once it prints its address, the page opens in Chrome in the same
session, so breakpoints stop in server and page code alike. Chrome gets a
profile of its own in the app's data folder, kept between runs; your usual
profile is never touched.

The first run downloads Microsoft's js-debug (pinned version, SHA-256 checked,
1.2 MB) into the app's data folder; Node itself is the one on your machine. When
it stops, the editor opens the file at the line and comes to the front, the call
stack and variables show next to the console, and an expression typed in the
console runs in the paused frame. `F5` continues, `F10` steps over, `F11` into,
`shift-F11` out, `shift-F5` stops everything the run started, including the
browser.

**Timeline** next to the console lists what the Node program did: requests it
served and sent (`http`, `https`, `fetch`) and the SQL it ran through `pg` and
`mysql2`, in the order they began, with status, rows and time. Queries and calls
made while serving a request sit under it, and each row opens the line of your
code that made it. A small script loaded with `--require` records them; a dev
server's own assets (`/_next/`) are left out.

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
declaration names with line numbers for Rust, TS/TSX, JS, Go, Python, C, C++ and
shell, the headings of Markdown, and extension languages that say what they
declare. It does
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

### Context servers

A context server gives the agent tools of its own: a database to query, an
issue tracker to read, documentation to look up. It is a Model Context
Protocol (MCP) server, a program Solder starts and talks to on its input and
output. Name the ones you want in `settings.json`, as Zed does:

```json
"context_servers": {
  "postgres": {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/app"],
    "env": { "PGPASSWORD": "..." }
  },
  "docs": { "command": { "path": "uvx", "args": ["some-docs-server"] }, "enabled": false }
}
```

Nothing starts with the editor. The servers start when an agent task begins,
with your shell's environment and the variables you gave, and stay for the
next task; changing an entry stops the server it was for. The agent offers
the model their tools next to its own. A server's tool runs where the server
does, not in the agent's sandbox, so the first call of each tool in a task
waits for **Allow once** and shows the tool, the server and the arguments;
after that the task may call that tool again. A server that does not start is
named in the task with its reason, and the task goes on without it. The
**context servers** view of the AI tab lists them with their tools.

A Zed extension may bring a context server (the Postgres, GitHub and
Context7 ones are in Zed's catalog). Installed, it is in the list with no
line in the settings: the extension's code says how to start it and installs
it first, through the same permissions as a language server. What such a
server needs to know is set under its name, as in Zed, and the extension says
what is missing when it is not there:

```json
"context_servers": {
  "postgres-context-server": { "settings": { "database_url": "postgresql://localhost/app" } }
}
```

`"enabled": false` under its name leaves it out, and an entry with a
`command` of your own under the same name is used in place of the extension's.

A server that is somewhere else is an address in place of a command, with
what to send along with every message to it, which is usually a key:

```json
"context_servers": {
  "issues": {
    "url": "https://mcp.example.com/mcp",
    "headers": { "Authorization": "Bearer ..." }
  }
}
```

It is reached over HTTP the way the protocol calls streamable: every message
is a request to that address, answered at once or as a stream of events. The
older way, where answers come over a second connection that stays open, and
signing in through a browser are not supported; a server that wants a key says
so in the AI tab.

What a server has to read (its resources: files, tables, pages) the agent
gets as two more tools of that server, one that lists them and one that reads
one by its address, asked for like any other. Its prompts are for you: **Use
context prompt** in the command palette lists the prompts of every server,
asks for what the chosen one has to be told, and puts what the server writes
into the agent's field, to read before sending.

What a server asks of the client (to sample a model, to list folders) is
refused, and nothing is listened for between requests, so a server that
changes its list of tools is asked again only when it next starts.

### Review before push

**Push** in the Git tab first has the chat model read what the push sends: the
commits not on the upstream yet (or on `origin/main`, `main` when there is no
upstream) and their diff, without files `.env` or `.solderignore` keep from AI and
within 60 KB (larger files are left out and named). The model reports problems
through a tool, as data: file, line, bug / risk / note, and a sentence. With
nothing found the push goes ahead; otherwise the findings show above the commit
box, each opening its file at its line, with **Push anyway** and **Cancel**. Turn
it off under **Review before push** in the AI tab's Providers view; without a
chat model Push just pushes.

## Plugins

A plugin is a folder with a `plugin.json` and a `plugin.wasm` (or, written in
JavaScript or TypeScript, a `plugin.js`) in the `plugins` folder of the app's
data folder (`~/Library/Application Support/Solder/plugins`
on macOS). **Workspace: Show plugins** in the command palette lists what is
there, with **Open folder** to get to it.

```json
{
  "name": "word-count",
  "version": "0.1.0",
  "description": "Shows how many words the file in front has.",
  "permissions": ["statusBar", "editor:read", "editor:write"],
  "events": ["open", "change", "save"],
  "commands": [{ "id": "insert", "title": "Word Count: Insert the count" }]
}
```

Nothing runs until you enable it, and the window shows what enabling allows
first. A plugin can compute inside its own memory and nothing else; the rest it
asks the editor for, and only what its manifest declares is answered:

| Permission | Allows |
|---|---|
| `statusBar` | Its text in the status bar |
| `editor:read` | The file in front and its selection |
| `editor:write` | Changing the file in front (one undo step per change) |
| `fs:read` | Files of the project, by path from its root: not outside it, not `.env`, not what `.solderignore` keeps from AI |
| `http:<host>` | Requests to that host. A redirect comes back as it is, so another host needs its own permission |

Approval is for that list and that module or script (by SHA-256). If an update
changes either, the plugin stays off and is marked **Changed** until you enable
it again.

Each plugin runs in its own WebAssembly instance (the `wasmi` interpreter, 64 MB
of memory at most) on its own thread, and talks to the editor in JSON messages,
so the editor never waits for it. Its work is timed, not counting the wait for
the editor's answers: handling a `change` (typing) gets 4 ms first; a plugin that
needs more is paused and continues once typing has stopped for 150 ms. Three
times over, or one event stopped at the limit of 10 s, marks it **Slow** in the
window and the status bar. An event that fails or is stopped ends there and the
plugin starts again from a fresh instance. (The interpreter can only be paused
by running it out of fuel, so it runs in slices of fuel sized to last about a
millisecond, and the clock is read between them.)

Plugins are written in Rust against `crates/plugin_sdk` and built for
`wasm32-unknown-unknown`; `plugins/word-count` is the example:

```rust
use solder_plugin::{Event, Plugin};

#[derive(Default)]
struct WordCount;

impl Plugin for WordCount {
    fn event(&mut self, _: Event) {
        if let Ok(editor) = solder_plugin::editor() {
            let words = editor.text.split_whitespace().count();
            solder_plugin::status(format!("{words} words"));
        }
    }
}

solder_plugin::register!(WordCount);
```

```bash
cd plugins/word-count && cargo build --release --target wasm32-unknown-unknown
```

Copy `plugin.json` and `target/wasm32-unknown-unknown/release/word_count.wasm`
(as `plugin.wasm`) into `plugins/word-count` in the data folder.

### In TypeScript or JavaScript

A plugin can be one script instead of a module: `plugin.js` next to the same
`plugin.json`. It uses the global `solder`, typed in `plugins/solder.d.ts`;
`plugins/word-count-ts` is the example:

```ts
function show(): void {
  const words = solder.editor().text.split(" ").filter((w) => w.trim() !== "").length;
  solder.status(`${words} words`);
}

solder.on("open", show);
solder.on("change", show);

solder.command("insert", () => {
  const file = solder.editor();
  if (file.path === null) return;
  solder.edit(file.path, file.selectionStart, file.selectionEnd, "here");
});
```

```bash
npx tsc -p plugins/word-count-ts   # writes plugin.js next to plugin.ts
```

The script runs in QuickJS (`crates/plugin/assets`, MIT), which is itself a
WebAssembly module inside the same sandbox, so the permissions, the 64 MB and the
time budget hold for it as for any plugin. The engine gets no files, no network
and no clock to wait on: only the lines it exchanges with the editor. So there
are no modules to import, no `setTimeout` and no `fetch`; `solder.http` is the
network, `async` functions and promises work, `console.log` writes the plugin's
log. Positions in `solder.editor()` and `solder.edit()` are JavaScript's (UTF-16
units) in the text `editor()` last returned; the editor converts.

An interpreter inside an interpreter is slow; see the table below. A script that
reads the whole file on every keystroke goes over the 4 ms on large files: its
work then waits for a pause in typing, and it ends up marked Slow. Listen to
`save` instead of `change`, or write that part in Rust or Go. Regular
expressions over long text cost the most (`split(/\s+/)` on 30 000 characters
alone takes about 150 ms); plain string methods are several times cheaper.

### In Go

A `plugin.wasm` can also be a program built for WASI: one that has a `_start`
instead of `solder_event`. It runs in the same sandbox under the same
permissions and budget, with no files and no network of its own; it reads events
as lines on its input and writes requests as lines on its output, and what it
prints otherwise is its log. `plugins/go` is the package that does this for Go,
and `plugins/word-count-go` the example:

```go
func show(solder.Event) {
	if file, err := solder.Editor(); err == nil {
		solder.Status(fmt.Sprintf("%d words", len(strings.Fields(file.Text))))
	}
}

func main() {
	solder.On("open", show)
	solder.On("change", show)
	solder.Run()
}
```

```bash
cd plugins/word-count-go && GOOS=wasip1 GOARCH=wasm go build -ldflags="-s -w" -o plugin.wasm .
```

Positions are bytes, as in the editor. A panic in a handler fails that event and
the program goes on. `time.Sleep` works and is not counted as the plugin's work.
The package reads and writes its JSON itself: `encoding/json` works through
reflection, and under the interpreter that alone took 25 ms for a 30 000
character file.

### How fast

Measured on an M4, release build, counting words on every change:

| | Rust | Go | TypeScript |
|---|---|---|---|
| Starting the plugin | 3 ms | 25 ms | 25 ms |
| A 2 000 character file | 0.2 ms | 0.5 ms | 2.4 ms |
| A 30 000 character file | 1.7 ms | 4.8 ms | 32 ms |

The registry is not there yet. Extensions of other editors do not run in this
host: VS Code extensions are Node programs with full access to the machine, and
Zed's use the WebAssembly Component Model, which this interpreter does not run.
Each kind has a host of its own, described under Extensions below.

## Extensions

Extensions made for Zed and VS Code install from their public catalogs:
Zed's own and Open VSX. Solder has no service of its own for this, and the
catalogs are asked only when the **Extensions** tab of the sidebar is opened
and when you search in it (**Workspace: Show extensions** in the command
palette opens it too).

The tab lists what is installed, then the answers of both catalogs in turn.
Select a row to see what Solder uses of that extension and what does not run
here.

| From | Solder uses | Does not run here |
|---|---|---|
| A Zed extension | Languages (highlighting, the languages inside them, and how they are typed: indentation, brackets, pairs, comments, words), snippets, themes, icon themes, its language servers, its debug adapters, its context servers | |
| A VS Code extension | Languages (colors from its TextMate grammar; comments, pairs and indentation from its language configuration), themes (JSON and the older `.tmTheme`), icon themes drawn with pictures, snippets, its debuggers, and its code, once you allow it | Icon themes drawn with a font, and what its code asks of VS Code that Solder does not have yet |

A Zed extension's language is a tree-sitter grammar compiled to WebAssembly.
It is compiled on the first file that needs it and runs in wasmtime inside
the parser, where it sees nothing but the text.

It also cannot hold the editor. Such a grammar parses on a thread of its own,
and typing waits for it no longer than a frame allows. One that has not
answered in ten seconds, which is a scanner that never returns, is given up
on: the status bar says so, and files of that language are plain text until
Solder starts again. The thread such a grammar holds cannot be taken back
before then.

The other files of a language are read too, and mean here what they mean in
Zed:

| File | What it gives |
|---|---|
| `config.toml`: `brackets`, `autoclose_before` | The pairs that close themselves, are typed over and take a line between them on Enter. `not_in` keeps a pair from closing inside a string or a comment |
| `config.toml`: `block_comment` | `cmd-/` in a language with no comment that runs to the end of a line: each line goes between the two ends |
| `config.toml`: `word_characters`, `completion_query_characters` | What a word is for moving, selecting and deleting by word, and for the word a completion goes on from |
| `config.toml`: `increase_indent_pattern`, `decrease_indent_pattern` | A line after one that matches the first is deeper; a line that matches the second goes one level back as it is typed |
| `indents.scm` | How deep the line after Enter goes, and where a closing word or tag goes when it is typed |
| `brackets.scm` | The bracket at the cursor and its other half, which may be words or tags |
| `overrides.scm` | Where a string or a comment is, for `not_in` |
| `outline.scm` | What a file declares, in the project map the AI gets |

A language built into Solder keeps the editor's own rules for all of this.

A language server comes from the extension's own code, a WebAssembly
component built against Zed's extension API (every version from 0.0.1 to 0.7
runs). Asked for the server, it looks on your PATH, asks npm or GitHub for
the newest version, downloads it into its own folder and answers with the
program to start. That code runs in a sandbox with a memory limit and a
budget for each call; its files are one folder
(`extensions/work/<id>` in the app's data folder). The server itself is a
program Solder starts with your rights, as Zed does.

Many servers are written in JavaScript and need Node.js. Yours is used when
you have one. When the machine has none, Solder downloads the newest
long-term release from nodejs.org the first time a server needs it, checks
it against the published hash and keeps it in `extensions/node` in the app's
data folder, for every extension; the status bar says so while it downloads.
It is not updated on its own: delete that folder to get a newer one.

A debug adapter comes from the extension's code too. When the open file is in
a language whose extension names a debugger (Ruby names `rdbg`), the debug
panel lists it next to the JavaScript choices, as `rdbg app.rb`. Started, the
extension is asked how to run the adapter for that file; it may install the
adapter first, with the commands its manifest declares. The adapter then
listens on a port Solder picks for it, or talks on its own input and output,
and the session is the same as any other: breakpoints, stepping, variables,
the console.

An icon theme puts a picture next to each file in the tree and on tabs, by
the file's name and ending. Press **Use** on it in the Extensions tab, or
name it in `settings.json` (`"icon_theme": "Catppuccin Mocha"`); with none
named, the tree has no pictures. The pictures are the extension's own SVG
files, read from its folder.

An icon theme of a VS Code extension works the same way, under the name the
extension shows it by. It says which picture goes with a file's name, its
ending or its language, and with a folder's name, open or closed. If it draws
some files differently on a light background, there is a second theme for
that, with ` Light` after the name. A theme drawn with the letters of a font
and not with pictures is listed among what does not run here.

A language that only a VS Code extension has is colored by the TextMate
grammar the extension brings: rules made of regular expressions, read a line
at a time, in JSON or in TextMate's own property lists. A change is read again
from its line until a line ends as it did before, so typing in a long file
reads a few lines and not all of it. A grammar may bring in another by name,
among those installed. The expressions are Oniguruma's; the few that
`fancy-regex` does not read leave their rule out, and the rest of the grammar
colors what it can. There is no tree behind such a language, so nothing that
needs one is there for it: no outline of its own, no brackets found by a
query.

How it is typed comes from the extension's language configuration: the line
and block comments, the pairs that close themselves (and where they do not, a
string or a comment, which is read off the colors), the brackets that stand a
line apart on Enter, and the two patterns that say when a line goes a level in
or out. Which files are of the language is by their endings and names; a
pattern that is more than an ending, and a first line that says so, are not
read.

A debugger of a VS Code extension works where its manifest says what the
debug adapter is: a program inside the extension, and what runs it (Node
mostly, the machine's or Solder's own). It is offered for the files of the
languages it names, even ones Solder has no grammar for, and started with the
launch its manifest suggests: `${file}`, `${workspaceFolder}` and the like are
filled in, and where VS Code would ask which program, it is the file in
front.

A debugger may also be set up in the extension's code, and many are: the
manifest names the kind of program and the code says what the adapter is.
Once that code is allowed, such a debugger is offered like the others, and
the code is started when a run is, not before. It is asked two things. Its
configuration providers go over the launch and may add to it or call it off.
Its adapter factory says where the adapter is: a program to start, a port
where one listens already, or an object in the extension's own code, which
the host puts behind a port of this machine so that the debugger reaches it
like any adapter. With no factory, the adapter is the program the manifest
names. An extension can start a run itself (`debug.startDebugging`, with a
launch given whole) and hears when runs begin and end. A launch by its name
in a file of launches, an adapter on a named pipe, and requests to the
adapter of a running session are not here yet. In an extension with no
code, a debugger whose manifest names no adapter is listed among what does
not run here.

The settings a VS Code extension declares are set in `settings.json` under
their own names, as in VS Code: `"prettier.tabWidth": 2`, or as objects inside
objects. The Extensions tab lists them with what each is now: what you set,
or what the extension says it is when not set. They are what its code will be
handed when it asks for its configuration; nothing else reads them yet.

The code of a VS Code extension is a Node program written against VS Code's
API. Unlike a Zed extension's it has no sandbox: it can do what your account
can. So an extension with code is downloaded and then waits, the tab says
**Run its code with Node.js, outside a sandbox**, and nothing of it is in place
until you press **Install**. One that was installed before, or put in the
folder by hand, keeps its themes and languages and has an **Allow** button
under **Its code**.

Each extension runs in a Node process of its own, started when what it waits
for happens (its activation events: the editor is up, a file of a language is
open, one of its commands is asked for), never before. It gets a `vscode`
module that is Solder's: what is in it works as in VS Code, and what is not
yet does nothing and is listed in the tab under **Asked for what Solder does
not have yet**, so it is plain why a feature is missing.

What is in the module today is what every extension starts from:

| An extension can | In Solder |
|---|---|
| Register commands and run its own or another extension's | As in VS Code. Of VS Code's own commands: `vscode.open`, `setContext`, `workbench.action.files.saveAll` |
| Show a message | With nothing to choose, it is in the status bar for eight seconds, in its color. With answers, or `modal`, it is a list to pick the answer from |
| Ask to pick from a list, or to type a line | The editor's own list, with the keys of the command palette. Where the extension allows several, Enter ticks a row and the first row answers with the ones ticked. What is typed is checked by the extension and asked again with what is wrong. A password is typed in stars |
| Put items in the status bar | In the `extensions` item of the bars, as text: Solder has no font for the pictures VS Code draws there. A click runs the item's command. Work in progress (`withProgress`) is said there too |
| Write to an output channel | Kept, the last 256 KB of each. **Show output** in the Extensions tab opens it in a tab, and so does the extension when it asks |
| Read and set settings | What extensions declare, with what `settings.json` says, and `editor.tabSize`. `update` writes the key to `settings.json` |
| See the workspace | A folder for each open window, the one in front first. Files through `workspace.fs` and `findFiles` |
| See open documents and the editor in front | Every file open in a tab, with its text, kept up to date as it is typed, saved and closed, and the cursor of the one in front. There is one visible editor: the file in front |
| Change text | `TextEditor.edit` and `workspace.applyEdit`, in open files and on disk, with files made, renamed and deleted. A snippet is put in as text, its places taken out |
| Use the clipboard, open a link | Yes; a link only to the web or to mail |
| Open terminals | A terminal of the dock, under the name the extension gives it, running what it names or your shell. It is made when the extension first shows it or types into it, so one only kept ready takes no room. A terminal the extension draws itself (`pty`) is a terminal of the dock too: what is typed goes to the extension key by key, and what it writes is shown |
| Provide and run tasks | **Workspace: Run extension task** in the palette lists the tasks extensions provide and runs the one chosen in a terminal, whose tab stays when it ends. The extension hears what it ended with. A task that is the extension's own code (`CustomExecution`) runs in a terminal it draws. Problem matchers are not read |
| Watch files | `createFileSystemWatcher`, for the folders of the open windows: files made, changed and deleted, but for `.git` and `node_modules` |
| Show pages (webviews) and editors of its own for kinds of files | A tab drawn by the system's browser; see below |
| Read, run and write notebooks | A tab of cells, drawn by Solder: the extension reads the file, runs the cells and writes it back; see below |
| Give language features in code (`vscode.languages`) | Completions, hovers, definitions, references, rename, formatting, code actions, document and project symbols, signature help, inlay hints, semantic colors, the places that implement a symbol, the other places it is used, and diagnostics from its collections. A code lens is above its line, where a click runs it, and among the code actions of the line (`cmd-.`) |

**Show extension views** opens the **Views** panel. Trees an extension names
are listed before its code starts; choosing one starts the approved code.
Source control groups and test controllers appear when registered. The
panel moves between docks through `layout.json` (`extension_views`) like
Files or Git. Open branches load on demand; Refresh keeps those branches
open and replaces their contents. Arrow keys move and expand, Enter opens
or runs a node, and Cmd+Enter (Ctrl+Enter on Linux) runs its first test
profile. Profile buttons also run and debug individual tests. Failed test
messages are in the row and its tooltip; output is in the extension's log.

Source control has an editable message and the command the provider gave
it, then groups of files and their commands. File-decoration providers are
asked only for visible file-tree entries. Text decorations support hex and
known theme background colors, whole-line backgrounds and text before or
after a range. UTF-16 positions are converted and ranges follow edits.

A **page** of an extension (a webview) is a tab next to the files' tabs: the
extension writes its HTML, and the page and the extension send each other
messages. It is drawn by the system's browser (WebKit on macOS, WebView2 on
Windows, WebKitGTK on Linux under X11), put where the tab's content is; no
browser exists until the first page opens, and none is made at startup. A
view an extension draws as a page is listed in **Views** and opens in a tab
when chosen. An editor an extension has for a kind of file (a picture, a
diagram) is offered as **Open with** in the file tree's menu and the
editor's, and opens the file as such a page.

A page is shut in. It can go nowhere but itself: links, new windows,
downloads and requests for the camera or the like are refused. Scripts run
only if the extension turned them on. It reads files only through an
address of its own, and only under the folders the extension named for it
(its own folder and the project's, when it named none); what it posts
reaches only the extension that made it. It gets the editor's colors and
fonts as VS Code's variables (`--vscode-editor-background` and a dozen
more, not every one VS Code has), and `vscode-dark` or `vscode-light` on
its body. `cmd-w` closes it, and the keys of the command palette and the
file finder work in it.

Not there: a page inside the sidebar itself, and pages under Wayland,
where the tab says so. Pages return through their serializer after a restart.

A **notebook** is a file an extension reads as a list of cells: text, and
code it can run. A file whose name an extension's kind of notebook is for
is offered as **Open with** in the file tree's menu and the editor's, and
opens as a tab of cells next to the files' tabs. Nothing of it is a browser
view: each cell is an editor of Solder's, as tall as its text and colored
for its language, and under a code cell is what its last run put out.

| | |
|---|---|
| `cmd-enter` | Run the cell the cursor is in |
| `shift-enter` | Run it and go to the next cell; after the last, a new one is made |
| `cmd-s` | Save: the extension writes the file from the cells as they are |
| **Run all**, **Stop** | Every code cell, in order; stop what runs |
| **Add code**, **Add text** | A cell under the one the cursor is in; code in the language of the code above it |
| The arrows and the bin in a cell's head | Move the cell up or down, remove it |

The extension does three things: it reads the file into cells and writes
them back (`registerNotebookSerializer`), and it runs the code
(`createNotebookController`), saying what each run put out as it comes.
Of what a run puts out, Solder draws words (plain text, what was written to
standard output and to standard error, an error with its trace, Markdown
and JSON as their source) and pictures (PNG, JPEG, GIF, WebP, up to 8 MB).
An output of more than 200 lines or 64 KB is cut, and says so. HTML output has a **View HTML output** button, which opens it in a separate
tab with scripts disabled. A matching `contributes.notebookRenderer` module
can draw an interactive output in that tab after the extension is allowed.
It receives the output's MIME data and can exchange messages through
`createRendererMessaging`. Only files inside that extension are served;
remote requests, navigation and downloads are blocked. Text and pictures
stay inline; browser views are created only when a result is opened.
Closing the result returns to its notebook. Closing a notebook with
changes asks, as closing a file does.

Not there for notebooks: language features inside a cell (completions and
the like), text cells shown as formatted text and not as their Markdown, a
choice between several things that can run a kind of notebook (the first
the extension made is used), dependencies between renderer modules
(`getRenderer`, `extends`), cell status bar items, edits an extension
makes to the cells itself (`NotebookEdit`), and a notebook opened by an
extension's code (`openNotebookDocument`, `showNotebookDocument`). Jupyter `.ipynb` files (format 4) open directly, without an extension. The
built-in reader preserves cell ids, metadata, attachments, outputs and
unknown fields when editing and saving. It does not start a kernel: reading
and saving a Jupyter file is independent of running its cells. A file over
64 MB is refused with a reason. Jupyter kernels and the ipywidgets protocol
are not built in: a generic renderer module does not make every Jupyter
widget or the Jupyter extension compatible. Outputs opened in a separate
tab are transient and are reopened from the notebook, not restored as pages.

The published `vscode.ipynb@1.95.3` reader from Open VSX has been checked on
a disposable CI runner: its code activated, read a sample `.ipynb` and saved
the edited cell. The census accepts an exact version and changes the sample
cell before saving, so losing edits is reported as a failure. This checks
a serializer, not a kernel or every notebook feature. Locally the same path is tested
against a catalog served by the test itself, including a serializer that
ignores edits. Published extension code is not run locally. The exact run
and its limits are in [the reader check](extension-checks/vscode-ipynb-1.95.3.md).

Live macOS check on 2026-10-10: typed into a Jupyter cell, saved and checked
the file; opened its HTML table and a renderer counter; clicked the counter
and received its extension's reply; restarted the project and saw the saved
notebook again. The renderer in that check was written here, not downloaded.

Files, split panes, cursors, scroll positions and notebook tabs are remembered
per project in `sessions/` beside the settings. Pages of extensions return
through `registerWebviewPanelSerializer`, with the last `setState` value;
a custom editor is reopened through its provider. Extensions whose code is
not allowed are not started for restoration. Missing files are skipped.
Terminal sessions are not restored. Text that was not saved is kept apart
from this (see Editing), and comes back with its tab. State is written off the UI thread, in order, once
per second when it changed and when the window closes.

Which extensions work is not claimed from the list above: it is found out.
With each release the fifty most installed extensions of Open VSX are
installed, their code is started with a made-up project to look at, and
each is asked for what it said it does, the way the editor asks (a hover,
a completion, the formatting of a file of its language). The result is a
page: what Solder takes from each without running anything, whether its
code started, what it registered, what it answered, and every part of VS
Code's API it asked for that Solder does not have. The page is the summary
and the artifact of the **Extensions of Open VSX** workflow, which can also
be run by hand. It runs the code of third parties with no sandbox, so it
runs on a CI machine with no secrets, and is not something to run on your
own:

```sh
cargo run --release -p extension --bin extension-census -- --count 50 --out extensions.md
```
Disposing a decoration type or disabling the extension removes its marks
without removing a language server's hints. Tree checkboxes, programmatic
reveal, source-control quick diffs, test coverage and cancellation, and
other CSS decoration styles are not implemented yet.

None of this costs anything while no extension's code runs: documents and
cursors are followed only once a host is up.

What an extension's manifest says its code can be asked to do is where
Solder's own commands are:

- **The palette** lists its commands under the names it gives them
  (`Git: Pull`). Choosing one starts the extension if it was waiting for
  that. A command its manifest keeps out of the palette is not there.
- **Menus.** The right button in a file opens what it put in the editor's
  menu (`editor/context`), and the menu of the file tree has what it put
  there (`explorer/context`) under Solder's own entries. Both give the
  command the file. Other menus of VS Code have no place here yet.
- **Keys.** The keys it binds are bound, the ones for this machine, with
  chords. They are over the key layout and under `keymap.json`, so a key
  of your own always wins. A key bound for when the editor has the
  keyboard is bound in the editor only.

Entries and keys have conditions (`when`), read against what Solder knows:
the language and name of the file in front (`editorLangId`,
`resourceExtname`, `resourceFilename`), whether it has a selection or can be
changed, the machine (`isMac`), and whatever extensions set with
`setContext`. A condition that asks for something Solder does not know, or
compares in a way it does not (`=~`, `in`), does not hold: an entry is left
out, never shown by mistake. To bind a key of your own to such a command:

```json
[{ "bindings": { "alt-r": ["workspace::RunExtensionCommand", { "command": "demo.run" }] } }]
```

Language features need nothing of the editor that a language server does not
already use. To the editor the host of such an extension is a language
server: it is asked in the Language Server Protocol, and the host answers
from what the extension registered. So an extension's completions are in the
same menu as a server's, its diagnostics under the same squiggles, its rename
behind the same key, and it works for a file of a language Solder has no
grammar for. This is also what `vscode-languageclient` is written against,
the library most language extensions use to talk to their own server. What
an extension registers for a feature the editor does not have (folding
ranges, document links, colors, call hierarchies) is taken and listed in the
tab with the rest of what is not here.

An extension that throws ends nothing but its own start. One that ends its
process is shown as stopped, with its last words. One that never returns stops
answering, and after 30 seconds its process is ended: the editor and the other
extensions never waited for it. What stopped is not started over and over;
**Start again** in the tab does it once. Node is the machine's, or the one
Solder downloads the first time something needs it.

A VS Code extension that has a build for each platform is installed in the
one for this machine. What it does not work without, and what it is a pack
of, are installed with it; a part of VS Code itself that it names is left
out, since no catalog has it.

An extension may also paint the completions of its server: Vue's shows a
property as a tag followed by its detail. Its answer is colored as code in
the file's language, and what you type is matched against the name in it.

The same goes for symbols. `cmd-shift-o` lists what the file in front
declares and `cmd-t` what the project does, to go to one by typing its name;
a name typed exactly comes first. They are the language server's lists: the
file's with what each symbol is inside of, the project's asked again as you
type, with the file and line of each. An extension paints them as it paints
completions (Ruby's colors the name of a class as a class is colored). A
file whose language has no server that lists symbols still has its list,
from the language's own outline.

A file can have several servers: the one Solder knows for its language and
every one that installed extensions bring for it. Their diagnostics show
together, their completions make one menu and their code actions one list;
a request with one answer (hover, definition, rename, formatting) goes to
the first server that says it gives it.

An extension can also set up a server it does not bring. Vue's own server
leaves the script of a component to the TypeScript server and needs it to
load Vue's plugin. So with the Vue extension installed, a `.vue` file is the
TypeScript server's too, that server is started with the plugin (started
again, if it ran without), and the questions Vue's server has for it pass
through the editor.

What you set for a server in `settings.json` applies to the servers of
extensions as it does to Solder's own:

```json
{
  "language_servers": {
    "vscode-html-language-server": {
      "command": "/opt/bin/my-html-server",
      "args": ["--stdio"],
      "initialization_options": { "provideFormatter": false },
      "settings": { "html": { "format": { "enable": false } } },
      "disabled": false
    }
  }
}
```

`command` and `args` replace the program the extension would start,
`initialization_options` and `settings` go on top of what it gives, and
`settings` is what the server is told and answered when it asks. The same
entry is handed to the extension when it asks for the user's settings, and
some ask by a name of their own: Vue's reads `"vue"`, not the id of its
server. A language's tab size comes from `indent_size`. A change takes effect
when the server next starts.

An extension may bring several servers that do the same work for a language:
Ruby's lists seven. Which of them start is the language's to say, written as
Zed writes it:

```json
{
  "languages": {
    "Ruby": { "language_servers": ["ruby-lsp", "!solargraph", "..."] }
  }
}
```

A name starts that server, `!name` keeps it from starting, and `"..."` stands
for all the others. Without `"..."` only the named ones start. The first is
the one asked what a single server answers, such as where a definition is.
With nothing said, a language whose extension brings alternatives starts what
Zed starts for it (`solargraph` for Ruby, `phpactor` for PHP, `elixir-ls` for
Elixir); any other starts all its servers. Open files go to the chosen servers
as soon as the settings are saved.

Three things a server draws into the text are shown for every server that has
them, an extension's or Solder's own. **Inlay hints** are the types a server
worked out and the names of parameters, in the line in a quieter color; they
are no part of the file, the cursor steps over them, and one longer than 60
characters is cut. **Semantic colors** are what the server says each word
is, over what the grammar says: a server knows a constant from a variable
where a grammar sees a name. A **code lens** is something the server offers
to do with a line (run this test, show what refers to this): its words are
above the line, in a quieter color, and a click does it. Its row has no
line number and is no part of the file: cursor movement, selections,
breakpoints and hover still refer to the lines of the file. A lens is shown if a click
can do it: its command is one the server runs itself, or one of three the
editor does for it. Places to show (`editor.action.showReferences`, and
rust-analyzer's name for the same) are listed as the references of a symbol
are. rust-analyzer's "Run" over a test or a `main` runs in a terminal of
the dock, whose tab stays to be read. A lens of any other command of the
editor it was written for (rust-analyzer's "Debug") is left out. All three are asked for a moment after
the last key, and move with the text until the answer comes. Each has a
setting, on unless turned off:

```json
{ "inlay_hints": true, "semantic_highlighting": true, "code_lens": true }
```

That is why such an extension **asks first**. It is downloaded and read, and
then waits: the tab shows what installing it allows (the servers it gets, the
commands its manifest declares), and nothing is in place until you press
**Install** there. A later version asks again only for what is new.

What installing allowed can be taken back afterwards, for that extension
alone. Under **It may** in its details, each of these has a **Refuse**
button, which turns into **Allow**: running the commands it declares,
installing packages from npm, downloading files, and each host it has
downloaded from. A refusal holds from the next thing the extension asks for;
a server it already started keeps running. Below that the tab lists what
its code did since Solder started (what it ran, installed and downloaded
from where) and what it asked for and did not get: what was refused, a
command its manifest does not declare, a server it could not get ready. So
what works is read from what happened, not from the manifest.

For an installed extension the tab has:

- **Update**, when its catalog has a newer version. They are looked for when
  the tab is opened; the button next to the search installs all of them.
- **Keep this version**: no update is offered for it.
- **Turn off**: it stays installed and its languages, snippets and servers
  are not used.
- **Remove**: deletes it and the servers it downloaded.

These decisions are kept in `extensions/state.json`.

## The layout of the window

Where the parts of the window are and how large is a file,
`~/.config/solder/layout.json`, next to `settings.json`. **Open Layout** in
the command palette opens it, written out with what is in use now. It is
applied as soon as it is saved.

```json
{
  "left": {
    "width": 390,
    "panels": ["files", "search", "git", "services", "database", "api", "ai", "extensions", "extension_views", "structure"]
  },
  "right": { "width": 380, "panels": ["chat", "agent"] },
  "bottom": { "height": 280, "panels": ["terminal", "problems", "debug", "response", "results"] },
  "title_bar": { "height": 38, "left": ["project", "breadcrumbs"], "right": [] },
  "tab_bar": { "height": 34, "place": "top" },
  "status_bar": {
    "height": 26,
    "left": ["position", "indent", "encoding", "language", "problems", "activity", "connection"],
    "right": ["extensions", "plugins", "performance"]
  },
  "hidden": [],
  "open": ["files"]
}
```

There are three docks, and each holds panels, with a tab for each: `panels`
says which a dock holds and in what order, so the chat can be on the left,
the file tree on the right and the terminals beside the code, or everything
in one dock. `terminal` is every terminal, each with a tab of its own, and
the button that opens another. `debug`, `response` (the answer to an HTTP
request) and `results` (of a query) have a tab only while they have something
to show; when the last of them closes, or the last terminal ends, its dock
shows the first panel it has left, or closes. A panel named
in `hidden` has no tab; its command and its key still open it, in the dock it
comes in, and its tab is there for as long as it shows. A panel the file does
not name is where it comes, after the ones the file names. When more tabs
are in a dock than it is wide, they go on a second row.

`open` names the panel each dock has open, one a dock; a dock with none of
its panels there is closed. Solder writes it whenever a dock is opened,
closed or turned to another panel, so the window starts the way it was left.
A dock left on the terminals, the debugger or an answer starts closed: they
have nothing to show yet. Saved by hand, it opens and closes docks like any
other change to the file. Left out, the left dock is open on its first panel.

Commands follow the panels: the chat's key opens and closes the dock the
chat is in, and the sidebar's key the left one, on its first panel. When the
file moves a panel that is showing, it shows in its new dock.

A part left out has the size it came with. A size no window can show is
brought to the nearest that fits (a side dock is 200 to 900 wide, the bottom
one 100 to 1200 high, a bar 22 to 64). A mistake in the file is said in the
status bar, like one in `settings.json`, and the layout that was right stays.

The same sizes can be set by hand: drag the border of a dock. The dock
follows the pointer, and when the border is let go its size is written into
the file, next to whatever else you wrote there, comments included. A double
click on a border puts its dock back to the size it came with. A file with a
mistake in it is not written to.

So can the panels. Drag a tab onto another tab and it goes before it, in the
same dock or another one; dropped anywhere else in a dock, it goes to the end
of that dock's row. While a tab is held, a closed dock shows a strip at its
edge of the window to drop it on. The terminals move together, by any of
their tabs. A panel moved to another dock is shown there.

The right button on a tab opens a menu: hide the panel, move it to another
dock, bring a hidden one back into this dock, and **Reset layout**. That
command, also in the command palette, puts the window back as it comes. Since
the file is what says how the window is, the file is put aside, whole, as
`layout.json.old`.

The title bar and the status bar hold items, from the left end and from the
right one: `left` and `right` say which and in what order. An item is in one
place; one the file names nowhere is on no bar, and an end the file leaves out
keeps the items it comes with.

Drag an item onto another and it goes before it. Drop it on empty space at
either end of either bar and it goes after that end's other items. An empty
end still has room to drop one on. The right button on an item opens its
menu: **Hide**, **Move to**, the hidden items to **Show**, and **Reset layout**.
The right button on empty space opens the same list of hidden items, to bring
one back there. Escape or a click outside closes the menu. Clickable items,
such as the branch, keep their actions after moving.

| Item | Says |
|---|---|
| `project` | the project's name |
| `file` | the file in front, from the project's folder |
| `breadcrumbs` | the way to the file in front from the project's folder, then the symbols the cursor is in, the outermost first; a click on the way shows the file in the tree, a click on a symbol opens the list of the file's symbols |
| `branch` | the branch; a click opens the list of branches |
| `position` | the line and column of the cursor, and how many cursors |
| `indent` | spaces or tabs, and how many |
| `encoding` | what the file is written in and `CRLF`, each only where it is not the usual (UTF-8, lines ending with LF); a click opens the list of encodings |
| `language` | the language of the file |
| `problems` | how many errors and warnings the file has |
| `activity` | a language server starting, files being read |
| `connection` | the database of a query file; a click picks another |
| `extensions` | what the code of VS Code extensions shows: its status bar items, work in progress, its last message. A click on an item runs its command |
| `plugins` | what plugins show; a click opens the Plugins window |
| `performance` | the numbers of `show_performance_hud` |

An item with nothing to say now is not drawn. `file` and `branch` are on no bar
until the file puts them on one. A layout file written before `extensions`
existed names its own items for the right end, so add it there to see what
extensions show. A mistake in a config file is always said in
the status bar, whatever it holds.

Panel tabs, bar items and common command buttons have Phosphor icons next to
their labels. The regular SVGs are embedded in the app under the MIT license;
they need no icon files or network connection at run time. They use the text
color of their control and scale with `ui_font_size`. File icons still come
from the icon theme of a Zed or VS Code extension selected with `icon_theme`.

An item of a bar is drawn as a word, an icon or both, and a bar can hold a
button for any command. Both are under `items` in the layout file, by the
item's name:

```json
{
  "status_bar": { "left": ["position", "language"], "right": [{ "button": "term" }] },
  "items": {
    "position": { "display": "icon" },
    "language": { "display": "text" },
    "term": {
      "label": "Terminal",
      "icon": "terminal-window",
      "command": "workspace::ToggleTerminal"
    }
  }
}
```

`display` is `text`, `icon` or `both`, which is how an item comes. `icon` is
one of the embedded icons by its Phosphor name (`gear`, `play`, `git-branch`,
`magnifying-glass`, and the rest of the folder `assets/icons/phosphor`).
`label` is the word it shows. `command` is what a click runs, written as in
`keymap.json`: a command's name, `["name", arguments]` for one that takes
them, or `{ "plugin": "...", "command": "..." }` for a plugin's command.

A name under `items` that is no item of the editor's is a button, and a bar
holds it as `{ "button": "name" }`. An item of the editor's own takes the same
keys: with a `label` or a `command` it shows that word and runs that command
in place of its own. An icon alone says what it is when the pointer rests on
it.

By hand, the menu of an item has **Text**, **Icon** and **Text and icon**,
**Add command button**, which lists every command and puts a button for the
chosen one at the end of that bar, and for a button **Remove button**. **Edit
icon or command** opens the file. A button with no command, an icon that is
not in the app or a command that does not exist is a mistake, said in the
status bar, and the layout that was right stays.

`place` of `tab_bar` is where the tabs of the open files are: `top`, `bottom`
or `none`. With none, files are changed by the keys and the file finder.

**Workspace: Save layout** keeps the window as it is under a new name, in
`~/.config/solder/layouts/<name>.json`, and selects it. An existing name is
left alone. **Workspace: Switch layout** opens a searchable list of saved
layouts; **Default** in that list is the original `layout.json`.

While a named layout is selected, its file is the truth: dragging, panel and
bar menus and opening or closing docks write into it, **Open layout** opens it,
and saving it by hand applies it. **Reset layout** puts that file aside as
`<name>.json.old` and gives it the default layout. Switching waits for earlier
changes to reach their file, so no change ends up in the wrong layout.

One key can switch straight to a layout, without opening the list, in
`keymap.json`:

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "alt-shift-r": ["workspace::SwitchLayout", { "name": "Review" }],
      "alt-shift-d": ["workspace::SwitchLayout", { "name": "Default" }]
    }
  }
]
```

The current choice and the choice each project was left in are kept in
`layouts.json`. A project opened again selects its saved layout. The layout
is shared by all open windows: a switch changes them together and remembers
it for every project still open. A project with no saved choice uses the
current layout. Missing or broken layout files leave the last good layout
in use and say what is wrong in the status bar.

**Workspace: Switch key layout** chooses **Default**, **VS Code**,
**JetBrains**, or a file in `~/.config/solder/keymaps/<name>.json`. The presets
cover the commands Solder has, with the keys of the current platform. The
choice is saved as `key_layout` in `settings.json` and applies to all windows.

**Workspace: Save key layout** copies the selected set under a new name and
selects it; an existing file is left alone. **Workspace: Open key layout**
opens that file for editing. For a built-in set it asks for a copy's name
first; run Open key layout again to edit the saved copy. Saving a key layout
applies it immediately. Invalid bindings or a missing file keep the last
good set and report the error in the status bar.

Bindings apply in this order: Solder's defaults, `keymap-imported.json`, the
selected set, then personal `keymap.json`. A saved copy contains only the
selected set; personal and imported bindings keep their own files.
**Workspace: Open keymap** still opens the personal file. A key can choose a
set directly, for example in `keymap.json`:

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "alt-shift-v": ["workspace::SwitchKeyLayout", { "name": "VS Code" }],
      "alt-shift-d": ["workspace::SwitchKeyLayout", { "name": "Default" }]
    }
  }
]
```

The text of the interface is set in `settings.json`, apart from the code's:

```json
{
  "ui_font_family": "Inter",
  "ui_font_size": 14,
  "ui_density": "comfortable"
}
```

`ui_font_size` is 12.5 as it comes and may be 9 to 18. The room around the
text grows and shrinks with it, and the rows of lists grow with larger text so
that a line always fits. `ui_density` is how tall those rows are: `compact`,
`default` or `comfortable`. The bars keep the heights `layout.json` gives
them.

## Settings from another editor

On a first launch (no `~/.config/solder/settings.json` yet) Solder looks for VS
Code, Cursor, Zed and JetBrains IDEs on the machine, off the UI thread, and
shows **Import settings** once if it finds one. **Workspace: Import settings** in
the command palette opens it at any time. Pick the editor, untick what should
stay behind, and press **Import**:

| | VS Code, Cursor | Zed | JetBrains |
|---|---|---|---|
| Font, font size, line height | yes | yes | font and size |
| Indent (2, 4 or 8), format on save | yes | yes | |
| Theme | from the extension that has it | from `themes` or an installed extension | |
| The editor's own keys | **VS Code keys** | | **JetBrains keys** |
| Bindings you changed | `keybindings.json` | `keymap.json` | `keymaps/*.xml` |
| Extensions Solder already covers | listed, nothing is installed | | |

An import only adds. A setting you already changed in Solder is shown as kept
and left alone; the others are written into `settings.json` key by key, so your
comments stay. Key bindings go to `keymap-imported.json`, which loads under your
own `keymap.json`; importing again replaces that file, deleting it drops them.
A binding whose command or condition has no equivalent here is counted, not
guessed at.

A theme becomes a file in `~/.config/solder/themes`, and `"theme"` in
`settings.json` names it (`system`, `dark` and `light` remain). The file gives
Solder's tokens as hex colors, so it can be edited or written by hand:

```json
{
  "name": "Night Owl",
  "appearance": "dark",
  "colors": { "bg": "#011627", "fg": "#d6deeb", "accent": "#7e57c2" },
  "syntax": { "keyword": "#c792ea", "string": "#ecc48d" },
  "terminal": { "red": "#ef5350", "bright_blue": "#82aaff" }
}
```

Tokens left out fall back to the built-in dark or light theme. From a VS Code
theme, the workbench colors and the TextMate rules are laid over those tokens;
what the theme does not say (or says in a way that would not read here, like a
border in its brightest color) is worked out from its background and text.
Snippets, Vim configs and JetBrains color schemes are not imported.

Every color in the window is one of these tokens: 21 in `colors`, 9 in `syntax`
and the 16 of `terminal`, which are the colors programs there ask for by number
(`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, and each
with `bright_` before it). A theme from VS Code or Zed brings the terminal
colors it has.

A theme's file is applied as soon as it is saved, so a theme can be written
with the window in view. To change a few tokens without a file of your own, set
them in `settings.json`, over whichever theme is in use:

```json
{
  "theme": "dark",
  "theme_overrides": {
    "accent": "#00c2a8",
    "syntax": { "comment": "#7a7a85" },
    "terminal": { "red": "#ff5f56" }
  }
}
```

A name that is no token, or a value that is no color, changes nothing and is
said in the status bar.

A theme sets shapes next to its colors, in pixels, in its file or in
`theme_overrides`:

```json
"shapes": { "panel_radius": 16, "control_radius": 8, "token_radius": 6, "border_width": 1, "spacing": 1 }
```

`control_radius` is the corners of buttons, fields, tabs and rows;
`token_radius` of what sits inside a line, like a key or an item of a menu;
`panel_radius` of windows over the editor and cards. `border_width` is the
lines between the parts of the window, 0 to 3. `spacing` is how much room
there is around things, as a number to multiply by: 1 is what Solder comes
with, 0.75 the tightest and 1.5 the airiest. It widens gaps and paddings and
leaves the text the size `ui_font_size` gives it.

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
