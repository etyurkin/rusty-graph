# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **Tree-sitter parsing** for Rust, TypeScript/JavaScript, Python, and Go
- **SQLite index** with FTS5 full-text search
- **Cross-file reference resolution**
- **Graph traversal** — explore, callers/callees, blast-radius impact, call-path search
- **Incremental sync** — content-hash based; only re-indexes changed files
- **File watcher** — debounced auto-sync on save

## Install

```bash
cargo install --path .
```

## Usage

```bash
rusty-graph init /path/to/project
rusty-graph status
rusty-graph query "MyFunction"
rusty-graph explore "calculateTotal"
rusty-graph callers "handleRequest"
rusty-graph impact "processOrder" --depth 5
rusty-graph path "handleRequest" "writeToDb"
rusty-graph sync
rusty-graph watch
```

## Improvements over the original

- **Deterministic explore output** — stable file ordering (BTreeMap) for reproducible agent context
- **Watcher resolves edges** — incremental updates rebuild call graph and FTS, not just raw nodes
- **Single-process design** — no daemon socket complexity
