# Contributing

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

- website: ESLint and TypeScript
- editor: `cargo fmt --check`, `cargo clippy` with warnings as errors, `cargo test`

New editor behavior comes with a test. UI behavior is tested headlessly
through GPUI's test context (see `workspace.rs` tests); features that talk
to external processes (language servers, the terminal) are tested against
real processes or the mock server in `editor/crates/solder/tests/fixtures`.

## Performance

The website promises specific numbers. Run `editor/scripts/bench.sh` before
and after changes that touch startup, rendering or typing, and put both
results in the pull request.
