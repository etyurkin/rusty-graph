# rusty-graph

A Rust reimplementation of [codegraph](https://github.com/colbymchenry/codegraph) — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph), which pioneered the pattern: parse with tree-sitter, store symbols and relationships in SQLite, expose the graph over MCP so agents understand code structure in one call instead of dozens of file reads. rusty-graph keeps that architecture and extends it in Rust.

Parses your codebase with tree-sitter, stores nodes and edges in SQLite, and exposes the graph over MCP so agents can understand code structure in one call instead of dozens of file reads.

## Features

- **Tree-sitter parsing** for Rust, TypeScript/JavaScript, Python, Go, Java, C/C++, and the Lisp family (Emacs Lisp, Common Lisp, Scheme)
- **SQLite index** with FTS5 full-text search
- **Cross-file reference resolution** — call edges connect callers to callees across files
- **MCP server** (stdio transport) exposing `rusty_graph_explore` + optional tools
- **Incremental sync** — only re-indexes changed files (content-hash based), prunes deleted files, and recomputes affected cross-file edges
- **File watcher** — `rusty-graph watch` runs a 2s-debounced auto-sync, batching each burst into a single resolve pass and honouring `.gitignore`/`.rusty-graphignore`
- **Staleness aware** — `explore` flags symbols whose source has changed on disk since indexing, so agents know when to re-read
- **Token-budgeted context packs** — `rusty-graph context "<task>" --budget N` returns the smallest ranked set of symbols + snippets (with their call-graph dependencies) that fits a budget
- **Semantic + fuzzy search** — search blends FTS, trigram fuzzy matching, and local offline embeddings (no model download)
- **Centrality ranking** — PageRank over the call graph ranks results and sizes the explorer
- **Architecture report** — `rusty-graph arch` finds circular dependencies (SCCs), hotspots, likely dead code, and cross-layer coupling
- **Test-impact mapping** — `rusty-graph tests <symbol>` and `rusty-graph diff --tests` list tests that cover a change
- **Git-diff awareness** — `rusty-graph diff <ref>` maps changed lines → impacted symbols → blast radius
- **Temporal coupling** — `rusty-graph cochange` mines git history for files that change together
- **Graph export** — `rusty-graph export --format {json,dot,csv,lsif}` for Graphviz, Gephi/pandas, and LSIF tooling
- **HTTP explorer** — `rusty-graph serve` exposes a JSON API + interactive graph
- **Optional LSP bridge** — `rusty-graph definition <file> <line> <col>` via a configured language server
- **External roots** — index deps/stdlib via `extra_roots` so calls resolve past the project boundary
- **Zero config** — works out of the box; optional `.rusty-graph/config.json` and `.rusty-graphignore` for tuning

## Improvements over [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph)

| Area | Original (TypeScript) | rusty-graph |
|------|----------------------|-------------|
| Runtime | Node.js + npm install | Single ~9 MB static binary, parsers linked in |
| Indexing | Sequential file walks | Parallel extraction with rayon |
| Change detection | SHA hashing | blake3 (faster incremental sync) |
| Search | FTS-focused | FTS + trigram fuzzy + offline embeddings + PageRank |
| Resolution | Name-based call linking | Kind-aware targets, import hints, same-file preference |
| Languages | Core set | + Lisp via lisp-sitter, Kotlin, Dart, Svelte/Vue, route recognition |
| Agent tools | MCP explore + search | Context packs, arch report, diff blast radius, test mapping, co-change mining |
| Explorer | CLI + MCP | + HTTP UI with cycle/impact overlays, LSIF export |
| Lisp | — | Semantic classification via [lisp-sitter](https://github.com/etyurkin/lisp-sitter) |

## Install

### Prebuilt binaries (recommended)

Download the latest release for your platform from
[GitHub Releases](https://github.com/etyurkin/rusty-graph/releases), make the binary executable, and put it on your `PATH`:

| Asset | Platform |
|-------|----------|
| `rusty-graph-linux-x86_64` | Linux x86_64 (static musl) |
| `rusty-graph-macos-x86_64` | macOS Intel |
| `rusty-graph-macos-aarch64` | macOS Apple Silicon |

```bash
chmod +x rusty-graph-*
sudo mv rusty-graph-* /usr/local/bin/rusty-graph
```

### Install from source

```bash
cargo install --git https://github.com/etyurkin/rusty-graph --tag v1.0.1
```

Or clone and build locally:

```bash
git clone https://github.com/etyurkin/rusty-graph.git
cd rusty-graph
cargo install --path .
```

`lisp-sitter` is fetched automatically as a git dependency — no sibling checkout required.

> **Note:** `cargo install rusty-graph` from [crates.io](https://crates.io) is not supported yet because the vendored Kotlin grammar is a path dependency. Use GitHub releases or `cargo install --git` instead.

## Usage

```bash
rusty-graph init /path/to/project
rusty-graph status
rusty-graph query "MyFunction" --kind function
rusty-graph explore "calculateTotal"
rusty-graph context "how are orders validated" --budget 8000
rusty-graph arch
rusty-graph diff main --tests
rusty-graph watch
rusty-graph mcp
```

See `rusty-graph --help` for the full command list.

## MCP Configuration

```json
{
  "mcpServers": {
    "rusty-graph": {
      "command": "rusty-graph",
      "args": ["mcp", "--path", "/path/to/project"],
      "env": {
        "RUSTY_GRAPH_MCP_TOOLS": "rusty_graph_search,rusty_graph_callers,rusty_graph_impact,rusty_graph_context"
      }
    }
  }
}
```

## Architecture

```
rusty-graph mcp
    │
    │  stdio (JSON-RPC)
    ▼
MCP Server (rmcp)
    │
    ├── rusty_graph_explore   ← primary tool
    └── optional tools (search, callers, impact, …)
    │
    ▼
SQLite (.rusty-graph/rusty-graph.db)
    ├── nodes, edges, files, nodes_fts
    ▲
tree-sitter + lisp-sitter parsers
```

## Supported Languages

| Language | Functions | Classes/Structs | Call edges |
|----------|-----------|-----------------|------------|
| Rust | ✓ | ✓ | ✓ |
| TypeScript/JavaScript | ✓ | ✓ | ✓ |
| Python | ✓ | ✓ | ✓ |
| Go | ✓ | ✓ | ✓ |
| Java | ✓ | ✓ | ✓ |
| C/C++ | ✓ | ✓ | ✓ |
| Emacs Lisp / Common Lisp / Scheme | ✓ | ✓ | ✓ |
| Ruby / C# / PHP / Swift / Kotlin / Dart | ✓ | ✓ | ✓ |
| Svelte / Vue | ✓ | ✓ | ✓ |

## Framework Route Recognition

HTTP endpoints from Express, FastAPI, Spring, Rails, Next.js, Django, and other common frameworks are indexed as `route` nodes linked to handlers.

## Migrating from codegraph

rusty-graph supersedes the older `codegraph-rust` port. If you used [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph) (TypeScript), indexes are **not** compatible — re-index your project:

```bash
rusty-graph init /path/to/project
```

| codegraph (TypeScript) | rusty-graph |
|------------------------|-------------|
| `.codegraph/codegraph.db` | `.rusty-graph/rusty-graph.db` |
| `CODEGRAPH_DIR` | `RUSTY_GRAPH_DIR` |
| `codegraph_explore` (MCP) | `rusty_graph_explore` |
| `codegraph` binary | `rusty-graph` binary |

## Index Location

`.rusty-graph/rusty-graph.db` inside the project root. Override with `RUSTY_GRAPH_DIR`.

## Configuration

Optional `.rusty-graph/config.json`:

```json
{
  "max_file_size": 1048576,
  "disabled_languages": ["scheme"],
  "extra_roots": ["vendor"],
  "lsp": { "rust": "rust-analyzer" }
}
```

`.h` headers are auto-detected as C or C++ from content.

## License

MIT
