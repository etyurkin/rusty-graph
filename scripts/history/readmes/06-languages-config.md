# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **Tree-sitter parsing** for Rust, TS/JS, Python, Go, Java, C/C++, Emacs Lisp, Common Lisp, and Scheme
- **SQLite index** with FTS5 full-text search
- **Cross-file reference resolution**
- **MCP server** with `rusty_graph_explore`
- **Incremental sync**, **file watcher**, **deleted-file pruning**
- **Configuration** via `.rusty-graph/config.json` and `.rusty-graphignore`

## Usage

```bash
rusty-graph init /path/to/project
rusty-graph watch
rusty-graph sync
```

## Supported Languages

| Language | Functions | Classes/Structs | Call edges |
|----------|-----------|-----------------|------------|
| Rust     | ✓ | ✓ | ✓ |
| TypeScript/JavaScript | ✓ | ✓ | ✓ |
| Python   | ✓ | ✓ | ✓ |
| Go       | ✓ | ✓ | ✓ |
| Java     | ✓ | ✓ | ✓ |
| C/C++    | ✓ | ✓ | ✓ |
| Emacs Lisp | ✓ | ✓ | ✓ |
| Common Lisp | ✓ | ✓ | ✓ |
| Scheme   | ✓ | ✓ | ✓ |

## Configuration

```json
{
  "max_file_size": 1048576,
  "disabled_languages": ["scheme"],
  "extra_roots": ["vendor"]
}
```

## Improvements over the original

- **C/C++ header disambiguation** — `.h` files classified as C or C++ from content
- **Prune-on-sync** — deleted files removed from the index automatically
- **Affected-edge recomputation** — incremental sync refreshes cross-file edges for changed files only
