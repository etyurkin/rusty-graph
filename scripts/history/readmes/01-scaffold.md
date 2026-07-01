# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

**Influence:** [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph) pioneered the pattern: parse a codebase with tree-sitter, store symbols and relationships in SQLite, expose the graph over MCP so agents can understand structure in one call instead of dozens of file reads. rusty-graph keeps that architecture but rewrites it in Rust for a single static binary with no Node.js runtime.

## Status

Early scaffold — core types and database layer next.

## Planned improvements over the original

- **Single static binary** — no Node.js/npm install, ~9 MB release build with parsers linked in
- **Parallel indexing** — rayon-backed extraction across files
- **Faster change detection** — blake3 content hashing instead of SHA
- **Broader language coverage** — Lisp family via lisp-sitter, Kotlin, Dart, Svelte/Vue, and more
- **Richer agent tooling** — token-budgeted context packs, architecture reports, git-diff blast radius, test-impact mapping
