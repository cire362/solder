# Solder editor

The native editor behind the website in `../src`. Rust, GPU-rendered UI, no Electron and no webview.

```
crates/
  text/     rope buffer, edits, undo history, cursor movement (no UI, fully unit-tested)
  syntax/   tree-sitter parsing and highlighting for Rust, TS/TSX, JS, JSON, CSS, Go, Python
  db/       database connections: detection, drivers, statement splitting (no UI)
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
SQLite opened read-only, write commands blocked for Redis and MongoDB).

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
