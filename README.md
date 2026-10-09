# Solder

An experimental native IDE for fullstack teams, and the website that describes it.

## Status: experiment

Solder is a personal R&D project, not a product to depend on. Nothing is released, APIs change without notice, and whole subsystems are rewritten when a better shape appears.

Most of the code here is written by coding agents (Claude Code and others) under my direction. I set the architecture, the constraints and the acceptance criteria, review every diff before it lands, and keep the rules agents follow in [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md). The experiment is to see how far an agent-driven workflow carries a large, performance-sensitive Rust codebase, and where it stops working.

Current scale: about 77k lines of Rust across 12 crates with 306 unit tests, plus a Next.js site, with CI checking both halves.

```
src/      website (Next.js, Tailwind): landing, docs, blog, pricing
editor/   the editor itself (Rust, GPU-rendered UI via GPUI); see editor/README.md
scripts/  checks shared by both
```

## What the editor does today

- Rope buffer with undo history, multi-cursor, IME and clipboard
- Tree-sitter highlighting for Rust, TS/TSX, JS, JSON, CSS, Go and Python, plus grammars that extensions bring
- Command palette, fuzzy file finder, find and replace, project-wide search on ripgrep's engine
- Language servers, including the ones Zed extensions ship, with user settings passed through to them
- Extensions installed from the Zed and VS Code catalogs, from inside the editor
- Plugins as WebAssembly and WASI programs with a permission model and an execution budget
- Built-in panels for databases, HTTP requests and local models

What is done and what is next: [editor/ROADMAP.md](editor/ROADMAP.md).

## Website

```bash
npm install
npm run dev
```

Open http://localhost:3000.

## Editor

```bash
cd editor
cargo run --release -p solder -- path/to/project
```

Release build matters here: the numbers the site quotes (cold start, memory on a large repo, keystroke-to-pixel latency) are measured on it. The status bar shows them live, toggle it with `cmd-alt-p`.

## Before you push

```bash
scripts/check.sh
```

The workflow is in [CONTRIBUTING.md](CONTRIBUTING.md).
