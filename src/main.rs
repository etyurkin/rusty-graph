mod arch;
mod cli;
mod cochange;
mod config;
mod context;
mod db;
mod embed;
mod export;
mod extract;
mod fuzzy;
mod gitdiff;
mod graph;
mod indexer;
mod lsp;
mod mcp;
mod serve;
mod sync;
mod term;
mod testmap;
mod types;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;

use cli::{Cli, Command};
use db::Db;
use graph::{Graph, SourceMap};
use indexer::Indexer;

const DB_DIR: &str = ".rusty-graph";
const DB_FILE: &str = "rusty-graph.db";

/// Directory that holds the index database. Defaults to `<project>/.rusty-graph`,
/// overridable with the `RUSTY_GRAPH_DIR` environment variable (useful to keep the
/// index out of the working tree or on faster storage).
fn db_dir(project: &std::path::Path) -> PathBuf {
    match std::env::var_os("RUSTY_GRAPH_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => project.join(DB_DIR),
    }
}

fn db_path(project: &std::path::Path) -> PathBuf {
    db_dir(project).join(DB_FILE)
}

fn open_db(project: &std::path::Path) -> Result<Arc<Mutex<Db>>> {
    let path = db_path(project);
    let db = Db::open(&path)
        .with_context(|| format!("Failed to open database at {}", path.display()))?;
    Ok(Arc::new(Mutex::new(db)))
}

/// Open an index that must already exist. Read/query commands use this so they
/// fail with a clear message instead of silently creating an empty database.
fn open_existing_db(project: &std::path::Path) -> Result<Arc<Mutex<Db>>> {
    let path = db_path(project);
    if !path.exists() {
        anyhow::bail!(
            "No index found at {}. Run `rusty-graph init` or `rusty-graph index` first.",
            path.display()
        );
    }
    open_db(project)
}

fn resolve_project(path: PathBuf) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("Path does not exist: {}", path.display()))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rusty_graph=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init { path } => {
            let project = resolve_project(path)?;
            let db = open_db(&project)?;
            let indexer = Indexer::new(db, project.clone());
            let stats = indexer.index_all(true, false)?;
            println!(
                "Initialized: {} files, {} nodes, {} edges",
                stats.files, stats.nodes, stats.edges
            );
        }

        Command::Uninit { path } => {
            let project = resolve_project(path)?;
            let dir = db_dir(&project);
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
                println!("Removed index at {}", dir.display());
            } else {
                println!("No index found at {}", dir.display());
            }
        }

        Command::Index { path, force, quiet } => {
            let project = resolve_project(path)?;
            let db = open_db(&project)?;
            let indexer = Indexer::new(db, project);
            let stats = indexer.index_all(force, quiet)?;
            if !quiet {
                println!(
                    "Indexed: {} files, {} nodes, {} edges",
                    stats.files, stats.nodes, stats.edges
                );
            }
        }

        Command::Sync { path } => {
            let project = resolve_project(path)?;
            let db = open_db(&project)?;
            let indexer = Indexer::new(db, project);
            let stats = indexer.sync()?;
            println!("Synced: {} files updated", stats.files);
        }

        Command::Watch { path } => {
            let project = resolve_project(path)?;
            let db = open_db(&project)?;

            // Bring the index up to date before watching.
            let indexer = Indexer::new(db.clone(), project.clone());
            let stats = indexer.sync()?;
            println!(
                "Synced {} changed files; watching {} for changes (Ctrl-C to stop)…",
                stats.files,
                project.display()
            );

            let _watcher = sync::FileWatcher::start(project.clone(), db)?;
            tokio::signal::ctrl_c().await?;
            println!("Stopping watcher.");
        }

        Command::Status { path, health, json } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let g = db.lock().unwrap_or_else(|e| e.into_inner());
            let stats = g.stats()?;

            if json {
                let mut payload = serde_json::json!({
                    "schema_version": g.schema_version()?,
                    "files": stats.file_count,
                    "nodes": stats.node_count,
                    "edges": stats.edge_count,
                });
                if health {
                    payload["languages"] = serde_json::to_value(g.language_breakdown()?)?;
                    payload["kinds"] = serde_json::to_value(g.kind_breakdown()?)?;
                    payload["unresolved_refs"] = serde_json::json!(g.unresolved_count()?);
                    payload["routes"] = serde_json::json!(g.edge_count_by_kind("contains")? > 0);
                    payload["embeddings"] = serde_json::json!(g.embedding_count()?);
                    payload["parse_issues"] = serde_json::to_value(g.files_with_parse_issues()?)?;
                }
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!(
                    "{} {} files, {} nodes, {} edges (schema v{})",
                    term::bold("RustyGraph index:"),
                    stats.file_count,
                    stats.node_count,
                    stats.edge_count,
                    g.schema_version()?,
                );
                if health {
                    println!("\n{}", term::bold("Languages:"));
                    for (lang, n) in g.language_breakdown()? {
                        println!("  {:<12} {}", lang, n);
                    }
                    println!("\n{}", term::bold("Kinds:"));
                    for (kind, n) in g.kind_breakdown()? {
                        println!("  {:<12} {}", kind, n);
                    }
                    let unresolved = g.unresolved_count()?;
                    println!(
                        "\n{} {} unresolved references, {} embeddings",
                        term::bold("Resolution:"),
                        unresolved,
                        g.embedding_count()?,
                    );
                    let issues = g.files_with_parse_issues()?;
                    if issues.is_empty() {
                        println!("{} all files parsed cleanly", term::green("Parse health:"));
                    } else {
                        println!("{}", term::yellow("Parse issues (errors/missing):"));
                        for (path, errors, missing) in issues {
                            println!("  {}  {}/{}", path, errors, missing);
                        }
                    }
                }
            }
        }

        Command::Query {
            search,
            path,
            kind,
            limit,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let nodes = db.lock().unwrap_or_else(|e| e.into_inner()).smart_search(
                &search,
                kind.as_deref(),
                limit,
            )?;

            if json {
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            } else {
                for node in &nodes {
                    println!(
                        "{} {} {} {}:{}",
                        term::kind(node.kind.as_str()),
                        node.qualified_name,
                        term::dim("—"),
                        node.file_path,
                        node.start_line
                    );
                    if let Some(sig) = &node.signature {
                        println!("  {}", term::dim(sig));
                    }
                }
                if nodes.is_empty() {
                    println!("No results for \"{}\"", search);
                }
            }
        }

        Command::Context {
            query,
            path,
            budget,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let source_map = SourceMap::new();
            let pack = context::build(&db, &source_map, &query, budget)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pack)?);
            } else {
                println!("{}", pack.format());
            }
        }

        Command::Tests {
            symbol,
            path,
            depth,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let g = db.lock().unwrap_or_else(|e| e.into_inner());
            let nodes = g.find_node_by_name(&symbol)?;
            if nodes.is_empty() {
                println!("Symbol not found: {}", symbol);
            } else {
                let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
                let tests = testmap::tests_for_nodes(&g, &ids, depth)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&tests)?);
                } else if tests.is_empty() {
                    println!("No tests cover {}", symbol);
                } else {
                    println!("{} test(s) cover {}:", tests.len(), symbol);
                    for t in &tests {
                        println!("  {} ({}:{})", t.qualified_name, t.file_path, t.start_line);
                    }
                }
            }
        }

        Command::Arch { path, json } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let g = db.lock().unwrap_or_else(|e| e.into_inner());
            let report = arch::report(&g, &project.to_string_lossy())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.format());
            }
        }

        Command::Export { format, path } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let g = db.lock().unwrap_or_else(|e| e.into_inner());
            println!("{}", export::export(&g, &format)?);
        }

        Command::Cochange {
            path,
            min,
            since,
            json,
        } => {
            let project = resolve_project(path)?;
            let report = cochange::analyze(&project, since.as_deref(), min)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.format());
            }
        }

        Command::Definition {
            file,
            line,
            column,
            path,
        } => {
            let project = resolve_project(path)?;
            let cfg = config::Config::load(&project);
            let abs = project.join(&file);
            let lang = types::detect_language(&abs)
                .with_context(|| format!("unsupported file type: {}", file))?;
            let command = cfg.lsp.get(lang).cloned().with_context(|| {
                format!(
                    "no LSP server configured for '{}'. Add one to .rusty-graph/config.json under \"lsp\".",
                    lang
                )
            })?;
            let mut client = lsp::LspClient::start(&command, &project)?;
            match client.definition(&abs, line.saturating_sub(1), column.saturating_sub(1))? {
                Some((uri, line0)) => {
                    println!(
                        "{}:{}",
                        uri.strip_prefix("file://").unwrap_or(&uri),
                        line0 + 1
                    );
                }
                None => println!("No definition found"),
            }
        }

        Command::Explore { query, path } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let source_map = SourceMap::new();
            let graph = Graph::new(db);
            let root = project.to_string_lossy().to_string();
            let result = graph.explore(&query, &root, &source_map)?;
            println!("{}", result.format());
        }

        Command::Node { symbol, path } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let db_guard = db.lock().unwrap_or_else(|e| e.into_inner());
            let nodes = db_guard.find_node_by_name(&symbol)?;
            let source_map = SourceMap::new();

            if nodes.is_empty() {
                println!("Symbol not found: {}", symbol);
            }
            for node in &nodes {
                println!(
                    "[{}] {} ({}:{}-{})",
                    node.kind.as_str(),
                    node.qualified_name,
                    node.file_path,
                    node.start_line,
                    node.end_line
                );
                if let Some(sig) = &node.signature {
                    println!("  {}", sig);
                }
                for (ln, line) in source_map.get_lines(
                    &node.file_path,
                    node.start_line as usize,
                    node.end_line as usize,
                ) {
                    println!("{}\t{}", ln, line);
                }
                let callers = db_guard.callers(&node.id, 10)?;
                if !callers.is_empty() {
                    println!("  callers:");
                    for c in &callers {
                        println!("    - {}", c.qualified_name);
                    }
                }
                println!();
            }
        }

        Command::Callers {
            symbol,
            path,
            limit,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let db_guard = db.lock().unwrap_or_else(|e| e.into_inner());
            let nodes = db_guard.find_node_by_name(&symbol)?;
            let mut all_callers = vec![];
            for node in &nodes {
                all_callers.extend(db_guard.callers(&node.id, limit)?);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&all_callers)?);
            } else {
                for c in &all_callers {
                    println!("{} ({}:{})", c.qualified_name, c.file_path, c.start_line);
                }
            }
        }

        Command::Callees {
            symbol,
            path,
            limit,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let db_guard = db.lock().unwrap_or_else(|e| e.into_inner());
            let nodes = db_guard.find_node_by_name(&symbol)?;
            let mut all_callees = vec![];
            for node in &nodes {
                all_callees.extend(db_guard.callees(&node.id, limit)?);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&all_callees)?);
            } else {
                for c in &all_callees {
                    println!("{} ({}:{})", c.qualified_name, c.file_path, c.start_line);
                }
            }
        }

        Command::Path {
            from,
            to,
            path,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let (from_nodes, to_nodes) = {
                let g = db.lock().unwrap_or_else(|e| e.into_inner());
                (g.find_node_by_name(&from)?, g.find_node_by_name(&to)?)
            };
            if from_nodes.is_empty() {
                println!("Symbol not found: {}", from);
            } else if to_nodes.is_empty() {
                println!("Symbol not found: {}", to);
            } else {
                let graph = Graph::new(db);
                let found = graph.call_path(&from_nodes[0].id, &to_nodes[0].id)?;
                match found {
                    Some(nodes) if json => {
                        println!("{}", serde_json::to_string_pretty(&nodes)?);
                    }
                    Some(nodes) => {
                        println!("Call path {} → {}:", from, to);
                        for (i, n) in nodes.iter().enumerate() {
                            println!("  {}{}", "  ".repeat(i), n.qualified_name);
                        }
                    }
                    None if json => println!("null"),
                    None => println!("No call path from {} to {}", from, to),
                }
            }
        }

        Command::Impact {
            symbol,
            path,
            depth,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let db_guard = db.lock().unwrap_or_else(|e| e.into_inner());
            let nodes = db_guard.find_node_by_name(&symbol)?;
            drop(db_guard);

            let graph = Graph::new(db);
            for node in &nodes {
                let impacts = graph.impact(&node.id, depth)?;
                if json {
                    let out: Vec<_> = impacts
                        .iter()
                        .map(|i| serde_json::json!({"depth": i.depth, "node": i.node}))
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else {
                    println!("Impact of {} (depth {}):", node.qualified_name, depth);
                    for imp in &impacts {
                        println!("  [{}] {}", imp.depth, imp.node.qualified_name);
                    }
                }
            }
        }

        Command::Files {
            path,
            max_depth,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let files = db
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .all_file_hashes()?;
            let root = project.to_string_lossy().to_string();
            let within_depth = |p: &str| -> bool {
                let rel = p.strip_prefix(&root).unwrap_or(p).trim_start_matches('/');
                rel.matches('/').count() <= max_depth
            };
            let paths: Vec<&String> = files
                .iter()
                .map(|(p, _)| p)
                .filter(|p| within_depth(p))
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&paths)?);
            } else {
                for p in &paths {
                    println!("{}", p);
                }
            }
        }

        Command::Diff {
            base,
            path,
            depth,
            tests,
            json,
        } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let report = gitdiff::diff_impact(&project, db.clone(), &base, depth)?;

            let covering = if tests {
                let g = db.lock().unwrap_or_else(|e| e.into_inner());
                let ids: Vec<String> = report.symbols.iter().map(|s| s.node.id.clone()).collect();
                testmap::tests_for_nodes(&g, &ids, 6)?
            } else {
                vec![]
            };

            if json {
                let mut out = serde_json::json!({
                    "base": report.base,
                    "changed_files": report.changed_files,
                    "symbols": report.symbols.iter().map(|s| serde_json::json!({
                        "node": s.node,
                        "blast_radius": s.blast_radius,
                    })).collect::<Vec<_>>(),
                });
                if tests {
                    out["tests"] = serde_json::to_value(&covering)?;
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("{}", report.format());
                if tests {
                    println!("\n--- Tests to run ({}) ---", covering.len());
                    for t in &covering {
                        println!("  {} ({})", t.qualified_name, t.file_path);
                    }
                }
            }
        }

        Command::Serve { path, port } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            serve::run(db, project, port).await?;
        }

        Command::Mcp { path, tools } => {
            let project = resolve_project(path)?;
            let db = open_existing_db(&project)?;
            let extra_tools: Vec<String> = tools
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            mcp::run_mcp_server(db, project, extra_tools).await?;
        }
    }

    Ok(())
}
