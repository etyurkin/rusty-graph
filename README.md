# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **Architecture report** — `rusty-graph arch` finds circular dependencies, hotspots, likely dead code
- **Git-diff awareness** — `rusty-graph diff main` maps changed lines → symbols → blast radius
- **Test-impact mapping** — `rusty-graph tests <symbol>` and `rusty-graph diff main --tests`
- **Temporal coupling** — `rusty-graph cochange` mines git history for files that change together
- **Graph export** — JSON, DOT, CSV, LSIF
- **HTTP explorer** — `rusty-graph serve` with interactive graph UI
- **LSP bridge** — `rusty-graph definition` for go-to-definition via language servers

## Usage

```bash
rusty-graph arch
rusty-graph diff main --tests
rusty-graph cochange --min 3
rusty-graph export --format dot > graph.dot
rusty-graph serve --port 7878
```

## Improvements over the original

- **Test-impact from caller chain** — walks the call graph upward to find covering tests
- **LSIF export** — interoperates with code-intel tooling
- **Interactive HTTP explorer** — rank-sized nodes, cycle overlays, path tracing
