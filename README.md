# rusty-graph

A Rust reimplementation of the [codegraph](https://github.com/colbymchenry/codegraph) idea — a local code knowledge graph for AI coding agents.

Inspired by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph); rewritten in Rust for a single static binary with no Node.js runtime.

## Features

- **17+ languages** including Ruby, C#, PHP, Swift, Kotlin, Dart, and Svelte/Vue
- **Framework route recognition** — Express, FastAPI, Spring, Rails, Next.js, and more
- **Semantic + fuzzy search** — FTS, trigram fuzzy matching, and offline embeddings
- **Centrality ranking** — PageRank over the call graph
- **Token-budgeted context packs** — `rusty-graph context "<task>" --budget N`

## Usage

```bash
rusty-graph query "validate token"
rusty-graph context "how are orders validated" --budget 8000
```

## Improvements over the original

- **No model download** — hashed bag-of-subtokens embeddings run fully offline
- **Route nodes** — HTTP endpoints indexed as first-class symbols linked to handlers
- **Import-hint resolution** — qualified names and import aliases improve cross-file call linking
