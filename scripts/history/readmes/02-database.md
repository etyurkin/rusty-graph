# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **SQLite index** with WAL mode, FTS5 full-text search, and prepared statements
- **Core graph model** — nodes (functions, classes, structs, …) and edges (calls, contains, imports, …)

## Install

```bash
cargo install --path .
```

## Architecture

```
tree-sitter parsers  →  extractors  →  SQLite (.rusty-graph/rusty-graph.db)
                                          ├── nodes
                                          ├── edges
                                          ├── files (content hashes)
                                          └── nodes_fts (FTS5)
```

## Improvements over the original

- Bundled SQLite via `rusqlite` — no separate database install
- FTS5 virtual table kept in sync on every index pass
