# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **Tree-sitter parsing** for Rust, TypeScript/JavaScript, Python, and Go
- **SQLite index** with FTS5 full-text search
- **Cross-file reference resolution** — call edges connect callers to callees across files
- **Parallel indexing** via rayon

## Install

```bash
cargo install --path .
```

## Usage

```bash
rusty-graph init /path/to/project
rusty-graph status
rusty-graph query "MyFunction" --kind function
```

## Supported Languages

| Language | Functions | Classes/Structs | Call edges |
|----------|-----------|-----------------|------------|
| Rust     | ✓ | ✓ | ✓ |
| TypeScript/JavaScript | ✓ | ✓ | ✓ |
| Python   | ✓ | ✓ | ✓ |
| Go       | ✓ | ✓ | ✓ |

## Index Location

`.rusty-graph/rusty-graph.db` inside the project root. Override with `RUSTY_GRAPH_DIR`.

## Improvements over the original

- **Parallel extraction** — files parsed concurrently with rayon
- **blake3 hashing** — faster incremental sync than SHA-based change detection
- **Kind-aware resolution** — call edges target functions/methods only, not same-named fields
