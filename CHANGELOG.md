# Changelog

All notable changes to rusty-graph are documented here.

## [1.0.1] - 2026-07-01

### Added

- MIT `LICENSE` file
- `CHANGELOG.md`
- Install docs: GitHub release binaries, `cargo install --git`, migration from codegraph

### Changed

- `lisp-sitter-*` dependencies switched from local path to pinned git deps (clone-and-build works anywhere)
- `Cargo.toml` metadata: `repository`, `homepage`, `keywords`
- CI/release workflows no longer clone lisp-sitter as a sibling directory

## [1.0.0] - 2026-07-01

First stable release of rusty-graph — a Rust reimplementation of
[colbymchenry/codegraph](https://github.com/colbymchenry/codegraph).

### Features

- Tree-sitter indexing for 17+ languages (Rust, TS/JS, Python, Go, Java, C/C++, Lisp family, Ruby, C#, PHP, Swift, Kotlin, Dart, Svelte/Vue)
- SQLite graph store with FTS5, fuzzy search, and offline embeddings
- Cross-file call resolution with kind-aware matching and import hints
- CLI: explore, query, context packs, arch report, git-diff impact, test mapping, co-change mining, export, HTTP explorer, LSP bridge
- MCP server (`rusty_graph_explore` + opt-in tools)
- Incremental sync, file watcher, framework route recognition
- Static release binaries for Linux (musl) and macOS (x64 + arm64)
