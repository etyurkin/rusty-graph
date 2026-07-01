# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **Tree-sitter parsing** for Rust, TypeScript/JavaScript, Python, and Go
- **SQLite index** with FTS5 full-text search
- **Cross-file reference resolution**
- **MCP server** (stdio transport) exposing `rusty_graph_explore` + optional tools
- **Incremental sync** and **file watcher**

## Install

```bash
cargo install --path .
```

## Usage

```bash
rusty-graph init /path/to/project
rusty-graph explore "calculateTotal"
rusty-graph mcp
```

## MCP Configuration

```json
{
  "mcpServers": {
    "rusty-graph": {
      "command": "rusty-graph",
      "args": ["mcp", "--path", "/path/to/project"]
    }
  }
}
```

Optional tools via `RUSTY_GRAPH_MCP_TOOLS`:

```json
{
  "env": {
    "RUSTY_GRAPH_MCP_TOOLS": "rusty_graph_search,rusty_graph_callers,rusty_graph_impact"
  }
}
```

## Improvements over the original

- **rmcp** — native Rust MCP server, no TypeScript SDK
- **Opt-in tool surface** — only `rusty_graph_explore` enabled by default; extras require explicit opt-in
