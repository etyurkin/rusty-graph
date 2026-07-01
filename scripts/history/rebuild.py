#!/usr/bin/env python3
"""Rewrite git history as atomic, compilable commits with progressive README."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HISTORY = Path(__file__).resolve().parent
SNAPSHOT = Path("/tmp/rusty-graph-history-snapshot")
BACKUP_HISTORY = Path("/tmp/rusty-graph-history-backup")
READMES = HISTORY / "readmes"

PROTECTED = "https://github.com/colbymchenry/codegraph"
PLACEHOLDER = "__COLBY_CODEGRAPH_URL__"

MODULES: list[str] = []


def run(cmd: list[str], *, cwd: Path = ROOT, check: bool = True) -> subprocess.CompletedProcess:
    print("+", " ".join(cmd))
    return subprocess.run(cmd, cwd=cwd, check=check, text=True, capture_output=not check)


def rebrand(text: str) -> str:
    text = text.replace(PROTECTED, PLACEHOLDER)
    repl = [
        ("CODEGRAPH_MCP_TOOLS", "RUSTY_GRAPH_MCP_TOOLS"),
        ("CODEGRAPH_DIR", "RUSTY_GRAPH_DIR"),
        ("CodeGraphServer", "RustyGraphServer"),
        ("CodeGraph index:", "RustyGraph index:"),
        ("codegraph-linux", "rusty-graph-linux"),
        ("codegraph-macos", "rusty-graph-macos"),
        ("/release/codegraph", "/release/rusty-graph"),
        ("codegraph_explore", "rusty_graph_explore"),
        ("codegraph_search", "rusty_graph_search"),
        ("codegraph_node", "rusty_graph_node"),
        ("codegraph_callers", "rusty_graph_callers"),
        ("codegraph_callees", "rusty_graph_callees"),
        ("codegraph_impact", "rusty_graph_impact"),
        ("codegraph_path", "rusty_graph_path"),
        ("codegraph_status", "rusty_graph_status"),
        ("codegraph_files", "rusty_graph_files"),
        ("codegraph_context", "rusty_graph_context"),
        ("codegraph_tests", "rusty_graph_tests"),
        ("codegraph_arch", "rusty_graph_arch"),
        (".codegraphignore", ".rusty-graphignore"),
        (".codegraph/config.json", ".rusty-graph/config.json"),
        (".codegraph/", ".rusty-graph/"),
        ('".codegraph"', '".rusty-graph"'),
        (".codegraph", ".rusty-graph"),
        ("codegraph.db", "rusty-graph.db"),
        ('name = "codegraph"', 'name = "rusty-graph"'),
        ("codegraph=info", "rusty_graph=info"),
        ('name = "codegraph"', 'name = "rusty-graph"'),
        ('about = "Local code knowledge graph', 'about = "Local code knowledge graph'),
        ("# codegraph", "# rusty-graph"),
    ]
    for old, new in repl:
        text = text.replace(old, new)
    text = text.replace("`codegraph ", "`rusty-graph ")
    text = text.replace(" codegraph ", " rusty-graph ")
    return text.replace(PLACEHOLDER, PROTECTED)


def copy_tree(src: Path, dst: Path) -> None:
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(
        src,
        dst,
        ignore=shutil.ignore_patterns("target", ".git", ".codegraph", ".rusty-graph", "session.md"),
    )
    for path in dst.rglob("*"):
        if path.is_file() and path.suffix in {".rs", ".toml", ".md", ".yml", ".sh", ".json", ".patch"}:
            path.write_text(rebrand(path.read_text()))


def write_readme(name: str) -> None:
    shutil.copy(READMES / name, ROOT / "README.md")


def cargo_toml(*extra: str) -> str:
    base = """[package]
name = "rusty-graph"
version = "0.1.0"
edition = "2021"
description = "Local code knowledge graph for AI coding agents"
license = "MIT"

[[bin]]
name = "rusty-graph"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive", "env"] }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
"""
    tail = """
[dev-dependencies]
tempfile = "3"

[profile.release]
lto = true
codegen-units = 1
strip = true
"""
    return base + "\n".join(extra) + tail


DEPS = {
    "db": "rusqlite = { version = \"0.32\", features = [\"bundled\", \"vtab\"] }\n",
    "extract_core": """
tree-sitter = "0.25"
tree-sitter-rust = "0.23"
tree-sitter-python = "0.23"
tree-sitter-go = "0.23"
tree-sitter-javascript = "0.23"
tree-sitter-typescript = "0.23"
ignore = "0.4"
blake3 = "1"
rayon = "1"
""",
    "sync": 'notify-debouncer-mini = { version = "0.7", features = [] }\n',
    "mcp": 'rmcp = { version = "1", features = ["server", "transport-io"] }\n',
    "jvm_c_lisp": """
tree-sitter-java = "0.23"
tree-sitter-c = "0.23"
tree-sitter-cpp = "0.23"
tree-sitter-elisp = "1"
tree-sitter-commonlisp = "0.4"
tree-sitter-scheme = "0.24"
""",
    "more_langs": """
tree-sitter-ruby = "0.23"
tree-sitter-c-sharp = "0.23"
tree-sitter-php = "0.24"
tree-sitter-swift = "0.7"
tree-sitter-kotlin-ng = { path = "vendor/tree-sitter-kotlin-ng" }
tree-sitter-dart = "0.2"
""",
    "serve": 'axum = "0.8.9"\n',
    "lisp_sitter": """
lisp-sitter-core = { path = "../lisp-sitter/crates/lisp-sitter-core" }
lisp-sitter-elisp = { path = "../lisp-sitter/crates/lisp-sitter-elisp" }
lisp-sitter-cl = { path = "../lisp-sitter/crates/lisp-sitter-cl" }
lisp-sitter-scheme = { path = "../lisp-sitter/crates/lisp-sitter-scheme" }
""",
}


def write_cargo(keys: list[str]) -> None:
    parts = [DEPS[k] for k in keys]
    (ROOT / "Cargo.toml").write_text(cargo_toml(*parts))


def snapshot_file(rel: str) -> None:
    src = SNAPSHOT / rel
    dst = ROOT / rel
    dst.parent.mkdir(parents=True, exist_ok=True)
    if src.is_dir():
        if dst.exists():
            shutil.rmtree(dst)
        shutil.copytree(src, dst)
    else:
        shutil.copy(src, dst)


def write_stub_main() -> None:
    lines = [f"mod {m};" for m in MODULES]
    lines.append("")
    lines.append("fn main() {}")
    (ROOT / "src/main.rs").write_text("\n".join(lines) + "\n")


def add_modules(*names: str) -> None:
    for n in names:
        if n not in MODULES:
            MODULES.append(n)
    write_stub_main()


def commit(msg: str) -> None:
    run(["git", "add", "-A"])
    run(["git", "commit", "-m", msg])


def verify_build(label: str) -> None:
    result = run(["cargo", "build", "--quiet"], check=False)
    if result.returncode != 0:
        print(result.stderr or result.stdout, file=sys.stderr)
        raise SystemExit(f"build failed: {label}")


EXTRACT_MOD_CORE = """mod go;
mod hints;
mod javascript;
mod python;
mod rust;

use anyhow::Result;
use std::path::Path;

use crate::types::{Edge, Node, UnresolvedRef};

pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedRef>,
}

impl ExtractionResult {
    pub fn empty() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            unresolved: vec![],
        }
    }
}

pub trait Extractor: Send + Sync {
    fn language(&self) -> &'static str;
    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult>;
}

pub fn extractor_for(language: &str) -> Option<Box<dyn Extractor>> {
    match language {
        "typescript" | "javascript" => Some(Box::new(javascript::JsExtractor)),
        "rust" => Some(Box::new(rust::RustExtractor)),
        "python" => Some(Box::new(python::PythonExtractor)),
        "go" => Some(Box::new(go::GoExtractor)),
        _ => None,
    }
}

pub fn language_for(language: &str) -> Option<tree_sitter::Language> {
    let lang = match language {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    };
    Some(lang)
}

pub fn parse_diagnostics(language: &str, source: &str) -> (u32, u32) {
    let Some(lang) = language_for(language) else {
        return (0, 0);
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang).is_err() {
        return (0, 0);
    }
    let Some(tree) = parser.parse(source, None) else {
        return (0, 0);
    };
    let mut errors = 0u32;
    let mut missing = 0u32;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() {
            errors += 1;
        }
        if node.is_missing() {
            missing += 1;
        }
        if !cursor.goto_first_child() {
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return (errors, missing);
                }
            }
        }
    }
}

/// Shared utilities for building node IDs and edges.
pub(crate) mod util {
    use crate::types::{Edge, EdgeKind, Node, Provenance};

    pub fn contains_edge(parent: &Node, child: &Node) -> Edge {
        let id = Edge::new_id(&parent.id, &child.id, &EdgeKind::Contains);
        Edge {
            id,
            source: parent.id.clone(),
            target: child.id.clone(),
            kind: EdgeKind::Contains,
            provenance: Provenance::TreeSitter,
            metadata: None,
        }
    }
}
"""


def write_extract_mod(stage: str) -> None:
    if stage == "core":
        (ROOT / "src/extract/mod.rs").write_text(EXTRACT_MOD_CORE)
    else:
        shutil.copy(SNAPSHOT / "src/extract/mod.rs", ROOT / "src/extract/mod.rs")


def main() -> None:
    run(["git", "checkout", "main"], check=False)
    run(["git", "branch", "-D", "history-rewrite"], check=False)

    print("Creating rebranded snapshot…")
    if BACKUP_HISTORY.exists():
        shutil.rmtree(BACKUP_HISTORY)
    shutil.copytree(HISTORY, BACKUP_HISTORY)
    copy_tree(ROOT, SNAPSHOT)

    print("Creating orphan branch…")
    run(["git", "checkout", "--orphan", "history-rewrite-tmp"])
    run(["git", "branch", "-D", "history-rewrite"], check=False)
    run(["git", "branch", "-m", "history-rewrite"])
    run(["git", "rm", "-rf", "."])
    (ROOT / "src").mkdir(parents=True, exist_ok=True)

    # 1 scaffold
    snapshot_file(".gitignore")
    gitignore = (ROOT / ".gitignore").read_text()
    if "scripts/history/" not in gitignore:
        (ROOT / ".gitignore").write_text(gitignore.rstrip() + "\nscripts/history/\n")
    write_cargo([])
    write_readme("01-scaffold.md")
    (ROOT / "src/main.rs").write_text('fn main() { eprintln!("rusty-graph"); }\n')
    commit("Initialize rusty-graph project scaffold")

    # 2 types
    snapshot_file("src/types.rs")
    add_modules("types")
    commit("Add core graph types and language detection")

    # 3 search helpers
    write_cargo(["db"])
    snapshot_file("src/fuzzy.rs")
    snapshot_file("src/embed.rs")
    add_modules("fuzzy", "embed")
    write_readme("02-database.md")
    commit("Add fuzzy matching and offline embedding helpers")

    # 4 db
    snapshot_file("src/db/mod.rs")
    add_modules("db")
    commit("Add SQLite storage layer with FTS5 search")

    # 5 config
    snapshot_file("src/config.rs")
    add_modules("config")
    commit("Add project configuration loader")

    # 6 extractors core
    write_cargo(["db", "extract_core"])
    for f in ["rust.rs", "javascript.rs", "python.rs", "go.rs", "hints.rs"]:
        snapshot_file(f"src/extract/{f}")
    write_extract_mod("core")
    add_modules("extract")
    write_readme("03-extractors.md")
    commit("Add tree-sitter extractors for Rust, TS/JS, Python, and Go")
    verify_build("core extractors")

    # 7 indexer
    snapshot_file("src/indexer.rs")
    add_modules("indexer")
    commit("Add parallel indexer with cross-file reference resolution")

    # 8 graph
    snapshot_file("src/graph.rs")
    add_modules("graph")
    write_readme("04-graph-cli.md")
    commit("Add graph traversal, explore, and impact queries")

    # 9 sync + term
    write_cargo(["db", "extract_core", "sync"])
    snapshot_file("src/sync.rs")
    snapshot_file("src/term.rs")
    add_modules("sync", "term")
    commit("Add incremental sync and debounced file watcher")

    # 10 vendor Kotlin grammar (needed before kotlin extractor)
    snapshot_file("vendor/tree-sitter-kotlin-ng")
    snapshot_file("patches/tree-sitter-kotlin-optional-class-member-semi.patch")
    snapshot_file("scripts/regenerate-kotlin-grammar.sh")
    commit("Vendor patched Kotlin tree-sitter grammar")

    # 11 all remaining extractors + full mod.rs
    for f in [
        "java.rs",
        "cfamily.rs",
        "lisp.rs",
        "ruby.rs",
        "csharp.rs",
        "php.rs",
        "swift.rs",
        "kotlin.rs",
        "dart.rs",
        "webcomponent.rs",
        "common.rs",
        "hints.rs",
        "routes.rs",
    ]:
        snapshot_file(f"src/extract/{f}")
    write_extract_mod("full")
    write_cargo(["db", "extract_core", "sync", "jvm_c_lisp", "more_langs", "lisp_sitter"])
    write_readme("06-languages-config.md")
    commit("Add Java, C/C++, Lisp, and extended language extractors")

    for f in ["src/context.rs", "src/arch.rs", "src/gitdiff.rs", "src/testmap.rs"]:
        snapshot_file(f)
    add_modules("context", "arch", "gitdiff", "testmap")
    write_readme("07-search-context.md")
    commit("Add context packs and architecture analysis")

    for f in ["src/cochange.rs", "src/export.rs", "src/serve.rs", "src/lsp.rs"]:
        snapshot_file(f)
    add_modules("cochange", "export", "serve", "lsp")
    write_cargo(["db", "extract_core", "sync", "mcp", "jvm_c_lisp", "more_langs", "serve"])
    write_readme("08-analysis-tools.md")
    commit("Add co-change mining, export, HTTP explorer, and LSP bridge")

    # CLI + MCP (full main)
    write_cargo(
        ["db", "extract_core", "sync", "mcp", "jvm_c_lisp", "more_langs", "serve", "lisp_sitter"]
    )
    snapshot_file("src/cli.rs")
    snapshot_file("src/main.rs")
    snapshot_file("src/mcp/mod.rs")
    write_readme("05-mcp.md")
    commit("Add CLI, command dispatch, and MCP server")
    verify_build("cli+mcp")

    # CI
    snapshot_file(".github/workflows/ci.yml")
    snapshot_file(".github/workflows/release.yml")
    commit("Add CI and release workflows")

    # lockfile + history tooling + final docs
    snapshot_file("Cargo.lock")
    commit("Refresh lockfile for full dependency graph")

    shutil.copytree(BACKUP_HISTORY, ROOT / "scripts/history", dirs_exist_ok=True)
    gi = (ROOT / ".gitignore").read_text().replace("scripts/history/\n", "")
    (ROOT / ".gitignore").write_text(gi)
    write_readme("09-final.md")
    commit("Document full feature set and improvements over codegraph")

    print("\nRunning tests…")
    run(["cargo", "test", "--quiet"])

    run(["git", "branch", "-M", "main"])
    print("\nHistory rewrite complete. git log --oneline")


if __name__ == "__main__":
    main()
