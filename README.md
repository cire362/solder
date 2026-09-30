# Solder

A native IDE for fullstack teams, and its website.

```
src/      website (Next.js): landing, docs, blog, pricing
editor/   the editor itself (Rust, GPU-rendered UI); see editor/README.md
scripts/  checks shared by both
```

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

What is built and what is next: [editor/ROADMAP.md](editor/ROADMAP.md).

## Before you push

```bash
scripts/check.sh
```

The workflow is in [CONTRIBUTING.md](CONTRIBUTING.md).
