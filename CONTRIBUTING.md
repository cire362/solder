# Contributing

Coding agents: read [AGENTS.md](AGENTS.md) first; it adds the rules that
keep automated changes safe.

## Branches

`main` is always releasable. Work happens on short-lived branches:

- `feat/<topic>` for features, e.g. `feat/git-gutter`
- `fix/<topic>` for bug fixes
- `perf/<topic>`, `refactor/<topic>`, `docs/<topic>`, `chore/<topic>`

One branch per roadmap item or bug. Merge into `main` through a pull request.

## Commits

[Conventional Commits](https://www.conventionalcommits.org), scoped by area:

```
feat(editor): stage selected lines
fix(lsp): send didChange before completion requests
perf(editor): shape only visible rows
docs(site): document the terminal
```

Scopes: `site`, `editor`, `lsp`, `text`, `syntax`, `terminal`, `git`, `ci`.

Each commit builds and passes tests on its own. Describe why in the body
when the diff does not make it obvious.

## Checks

`scripts/check.sh` runs everything CI runs:

- website: ESLint and TypeScript, and the editor's example plugins written in
  TypeScript and, where Go is installed, in Go (`npm run plugins`)
- editor: `cargo fmt --check`, `cargo clippy` with warnings as errors, `cargo test`

The checks use the `ci` Cargo profile and the committed lockfile. This profile
keeps debug assertions and overflow checks, but disables dependency optimization
(except for the plugin interpreter and the compiler in wasmtime, which the
tests of script plugins and of extensions wait on)
and uses line-table debug information for faster builds. Normal development and
release profiles are unchanged.

GitHub Actions reports test compilation separately from test execution. Rust
dependency artifacts are cached even when tests fail, so failures do not force
another full dependency build on the next run.

New editor behavior comes with a test. UI behavior is tested headlessly
through GPUI's test context (see `workspace.rs` tests); features that talk
to external processes (language servers, the terminal) are tested against
real processes or the mock server in `editor/crates/solder/tests/fixtures`.

The plugin host is tested against real plugins, which the tests build for
WebAssembly; install the target once with
`rustup target add wasm32-unknown-unknown`. The tests of Go plugins build with
`go` (1.21 or later) and are skipped without it.

Building the editor needs `cmake` on the PATH: the WebAssembly runtime that
loads the grammars of extensions copies its C headers with it
(`brew install cmake`, `apt-get install cmake`).

Database drivers are tested against real servers in
`editor/crates/db/tests/servers.rs`. CI starts Postgres, MySQL, Redis and
MongoDB as service containers; locally each test is skipped unless its URL is
set. With Docker or OrbStack:

```bash
docker run -d --name solder-pg -e POSTGRES_PASSWORD=solder -e POSTGRES_DB=solder -p 55432:5432 postgres:17-alpine
docker run -d --name solder-mysql -e MYSQL_ROOT_PASSWORD=solder -e MYSQL_DATABASE=solder -p 53306:3306 mysql:8.4
docker run -d --name solder-redis -p 56379:6379 redis:7-alpine
docker run -d --name solder-mongo -p 57017:27017 mongo:8
export SOLDER_TEST_POSTGRES=postgres://postgres:solder@localhost:55432/solder
export SOLDER_TEST_MYSQL=mysql://root:solder@localhost:53306/solder
export SOLDER_TEST_REDIS=redis://localhost:56379
export SOLDER_TEST_MONGO=mongodb://localhost:57017/solder
```

TLS tests also run in CI. Run the same checks locally with:

```bash
bash editor/scripts/test-db-tls.sh
```

The script needs Docker and OpenSSL. It creates a one-day test CA and four
temporary servers on random loopback ports, then removes only those containers,
their volumes and certificates. Existing `solder-*` containers are untouched.
Tests check encryption, rejection of an untrusted certificate, a trusted private
CA, hostname mismatches, Postgres channel binding and Prisma URL parameters.
They use real handshakes but do not verify hosted Neon, RDS or Atlas deployments,
MongoDB SRV DNS discovery or client-certificate authentication.

## Performance

The website promises specific numbers. Run `editor/scripts/bench.sh` before
and after changes that touch startup, rendering or typing, and put both
results in the pull request.
