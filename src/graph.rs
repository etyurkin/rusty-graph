use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::db::Db;
use crate::types::Node;

pub struct Graph {
    db: Arc<Mutex<Db>>,
}

impl Graph {
    pub fn new(db: Arc<Mutex<Db>>) -> Self {
        Self { db }
    }

    /// BFS to find impact (all callers transitively) of a node.
    pub fn impact(&self, node_id: &str, max_depth: usize) -> Result<Vec<ImpactNode>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut result: Vec<ImpactNode> = vec![];

        queue.push_back((node_id.to_string(), 0));
        visited.insert(node_id.to_string());

        while let Some((id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let callers = db.callers(&id, 100)?;
            for caller in callers {
                if !visited.contains(&caller.id) {
                    visited.insert(caller.id.clone());
                    result.push(ImpactNode {
                        node: caller.clone(),
                        depth: depth + 1,
                    });
                    queue.push_back((caller.id, depth + 1));
                }
            }
        }
        Ok(result)
    }

    /// Find the call path between two named symbols using BFS.
    pub fn call_path(&self, from_id: &str, to_id: &str) -> Result<Option<Vec<Node>>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<Vec<String>> = VecDeque::new();

        queue.push_back(vec![from_id.to_string()]);
        visited.insert(from_id.to_string());

        while let Some(path) = queue.pop_front() {
            let current = path.last().unwrap().clone();
            if current == to_id {
                let nodes: Vec<Node> = path
                    .iter()
                    .filter_map(|id| db.get_node(id).ok().flatten())
                    .collect();
                return Ok(Some(nodes));
            }

            let callees = db.callees(&current, 50)?;
            for callee in callees {
                if !visited.contains(&callee.id) {
                    visited.insert(callee.id.clone());
                    let mut new_path = path.clone();
                    new_path.push(callee.id);
                    queue.push_back(new_path);
                }

                // Safety valve: cap BFS at 10k nodes
                if visited.len() > 10_000 {
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    /// Maximum source lines included per symbol in an explore result.
    /// Keeps MCP responses from ballooning for large functions.
    const MAX_SOURCE_LINES: usize = 60;

    /// Maximum blast-radius entries returned.
    const MAX_BLAST_RADIUS: usize = 30;

    /// Explore: given a query, return relevant nodes with source, call paths, blast radius.
    pub fn explore(
        &self,
        query: &str,
        _project_root: &str,
        source_map: &SourceMap,
    ) -> Result<ExploreResult> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());

        // Search for matching symbols
        let nodes = db.search_nodes(query, None, 20)?;
        if nodes.is_empty() {
            return Ok(ExploreResult {
                query: query.to_string(),
                symbols: vec![],
                blast_radius: vec![],
                stale_files: vec![],
            });
        }

        drop(db);

        let mut symbols: Vec<SymbolInfo> = vec![];
        let mut blast_radius: Vec<ImpactNode> = vec![];

        for node in &nodes {
            // Gather callers and callees
            let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
            let callers = db.callers(&node.id, 10)?;
            let callees = db.callees(&node.id, 10)?;
            drop(db);

            // Collect blast radius (top-level: direct callers)
            for caller in &callers {
                blast_radius.push(ImpactNode {
                    node: caller.clone(),
                    depth: 1,
                });
            }

            // Read source lines, capped to avoid unbounded MCP payloads.
            let end_capped = node.end_line.min(
                node.start_line
                    .saturating_add(Self::MAX_SOURCE_LINES as u32 - 1),
            );
            let source_lines = source_map.get_lines(
                &node.file_path,
                node.start_line as usize,
                end_capped as usize,
            );

            symbols.push(SymbolInfo {
                node: node.clone(),
                source_lines,
                callers,
                callees,
            });
        }

        // Deduplicate and cap blast radius.
        blast_radius.sort_by(|a, b| a.node.id.cmp(&b.node.id));
        blast_radius.dedup_by(|a, b| a.node.id == b.node.id);
        blast_radius.truncate(Self::MAX_BLAST_RADIUS);

        // Flag files whose on-disk content no longer matches what was indexed,
        // so an agent knows to re-read them instead of trusting stale snippets.
        let mut stale_files: Vec<String> =
            BTreeSet::from_iter(symbols.iter().map(|s| s.node.file_path.clone()))
                .into_iter()
                .filter(|path| {
                    let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
                    file_is_stale(&db, path)
                })
                .collect();
        stale_files.sort();

        Ok(ExploreResult {
            query: query.to_string(),
            symbols,
            blast_radius,
            stale_files,
        })
    }
}

/// Compute PageRank centrality over a directed graph. An edge `(u, v)` means
/// "u calls v", so rank flows toward widely-called symbols — the utilities and
/// core APIs that matter most. Returns scores normalized so the maximum is 1.0
/// (or all-zero for an empty graph), suitable for blending into search ranking.
pub fn pagerank(
    node_ids: &[String],
    edges: &[(String, String)],
    damping: f64,
    iterations: usize,
) -> Vec<(String, f64)> {
    let n = node_ids.len();
    if n == 0 {
        return vec![];
    }
    let index: HashMap<&str, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    let mut out_degree = vec![0usize; n];
    let mut incoming: Vec<Vec<usize>> = vec![vec![]; n];
    for (src, dst) in edges {
        if let (Some(&u), Some(&v)) = (index.get(src.as_str()), index.get(dst.as_str())) {
            out_degree[u] += 1;
            incoming[v].push(u);
        }
    }

    let base = (1.0 - damping) / n as f64;
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..iterations {
        // Mass from dangling nodes (no out-edges) is redistributed uniformly.
        let dangling: f64 = (0..n)
            .filter(|&i| out_degree[i] == 0)
            .map(|i| rank[i])
            .sum();
        let mut next = vec![base + damping * dangling / n as f64; n];
        for v in 0..n {
            let mut acc = 0.0;
            for &u in &incoming[v] {
                acc += rank[u] / out_degree[u] as f64;
            }
            next[v] += damping * acc;
        }
        rank = next;
    }

    let max = rank.iter().cloned().fold(0.0_f64, f64::max);
    let scale = if max > 0.0 { 1.0 / max } else { 0.0 };
    node_ids
        .iter()
        .cloned()
        .zip(rank.into_iter().map(|r| r * scale))
        .collect()
}

/// True if `path`'s current on-disk content hash differs from what the index
/// recorded (or the file is gone/unreadable). Mirrors the indexer's blake3 hash.
fn file_is_stale(db: &Db, path: &str) -> bool {
    let indexed = match db.get_file_hash(path) {
        Ok(Some(h)) => h,
        Ok(None) => return false, // not part of the index; nothing to be stale against
        Err(_) => return false,
    };
    match std::fs::read(path) {
        Ok(bytes) => blake3::hash(&bytes).to_hex().to_string() != indexed,
        Err(_) => true, // file removed since indexing
    }
}

#[derive(Debug)]
pub struct ImpactNode {
    pub node: Node,
    pub depth: usize,
}

#[derive(Debug)]
pub struct SymbolInfo {
    pub node: Node,
    pub source_lines: Vec<(usize, String)>,
    pub callers: Vec<Node>,
    pub callees: Vec<Node>,
}

#[derive(Debug)]
pub struct ExploreResult {
    pub query: String,
    pub symbols: Vec<SymbolInfo>,
    pub blast_radius: Vec<ImpactNode>,
    pub stale_files: Vec<String>,
}

impl ExploreResult {
    /// Format as the text output that agents consume.
    pub fn format(&self) -> String {
        let mut out = String::new();

        if self.symbols.is_empty() {
            out.push_str(&format!("No symbols found for query: {}\n", self.query));
            return out;
        }

        // Group symbols by file, keeping file order deterministic.
        let mut by_file: BTreeMap<String, Vec<&SymbolInfo>> = BTreeMap::new();
        for sym in &self.symbols {
            by_file
                .entry(sym.node.file_path.clone())
                .or_default()
                .push(sym);
        }

        for (file, syms) in &by_file {
            out.push_str(&format!("\n=== {} ===\n", file));
            for sym in syms {
                out.push_str(&format!(
                    "\n[{}] {} (lines {}-{})\n",
                    sym.node.kind.as_str(),
                    sym.node.qualified_name,
                    sym.node.start_line,
                    sym.node.end_line,
                ));
                if let Some(sig) = &sym.node.signature {
                    out.push_str(&format!("  signature: {}\n", sig));
                }
                if let Some(doc) = &sym.node.docstring {
                    out.push_str(&format!("  doc: {}\n", doc.lines().next().unwrap_or("")));
                }
                // Line-numbered source
                for (line_no, line) in &sym.source_lines {
                    out.push_str(&format!("{}\t{}\n", line_no, line));
                }

                if !sym.callers.is_empty() {
                    out.push_str("  callers:\n");
                    for c in &sym.callers {
                        out.push_str(&format!("    - {} ({})\n", c.qualified_name, c.file_path));
                    }
                }
                if !sym.callees.is_empty() {
                    out.push_str("  calls:\n");
                    for c in &sym.callees {
                        out.push_str(&format!("    - {} ({})\n", c.qualified_name, c.file_path));
                    }
                }
            }
        }

        if !self.blast_radius.is_empty() {
            out.push_str("\n--- Blast Radius ---\n");
            for impact in &self.blast_radius {
                out.push_str(&format!(
                    "  [depth {}] {}\n",
                    impact.depth, impact.node.qualified_name,
                ));
            }
        }

        if !self.stale_files.is_empty() {
            out.push_str("\n⚠️  Stale files (re-read directly):\n");
            for f in &self.stale_files {
                out.push_str(&format!("  - {}\n", f));
            }
        }

        out
    }
}

/// Cached file contents for generating line-numbered source snippets.
pub struct SourceMap {
    cache: Mutex<HashMap<String, Vec<String>>>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_lines(&self, file_path: &str, start: usize, end: usize) -> Vec<(usize, String)> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let lines = cache.entry(file_path.to_string()).or_insert_with(|| {
            std::fs::read_to_string(file_path)
                .map(|s| s.lines().map(String::from).collect())
                .unwrap_or_default()
        });

        let start = start.saturating_sub(1).min(lines.len());
        let end = end.clamp(start, lines.len());
        lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| (start + i + 1, line.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Edge, EdgeKind, FileRecord, NodeKind, Provenance};

    fn node(name: &str, file: &str) -> Node {
        let qualified = format!("{file}::{name}");
        Node {
            id: Node::new_id(file, &qualified),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: file.to_string(),
            language: "rust".to_string(),
            start_line: 1,
            end_line: 1,
            signature: None,
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        }
    }

    #[test]
    fn source_map_returns_requested_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.txt");
        std::fs::write(&f, "line1\nline2\nline3\n").unwrap();
        let sm = SourceMap::new();
        let p = f.to_string_lossy().to_string();
        let lines = sm.get_lines(&p, 1, 2);
        assert_eq!(
            lines,
            vec![(1, "line1".to_string()), (2, "line2".to_string())]
        );
    }

    #[test]
    fn source_map_does_not_panic_when_range_exceeds_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.txt");
        std::fs::write(&f, "only\ntwo\n").unwrap();
        let sm = SourceMap::new();
        let p = f.to_string_lossy().to_string();
        // start_line past EOF must not panic (regression test for slice bug).
        assert!(sm.get_lines(&p, 10, 12).is_empty());
    }

    #[test]
    fn explore_format_orders_files_alphabetically() {
        let sym = |n: &str, file: &str| SymbolInfo {
            node: node(n, file),
            source_lines: vec![],
            callers: vec![],
            callees: vec![],
        };
        // Insert b before a; format() must still emit a.rs first.
        let result = ExploreResult {
            query: "q".to_string(),
            symbols: vec![sym("B", "b.rs"), sym("A", "a.rs")],
            blast_radius: vec![],
            stale_files: vec![],
        };
        let out = result.format();
        let ai = out.find("a.rs").expect("a.rs present");
        let bi = out.find("b.rs").expect("b.rs present");
        assert!(ai < bi, "a.rs should be listed before b.rs:\n{out}");
    }

    #[test]
    fn explore_format_reports_no_symbols() {
        let result = ExploreResult {
            query: "missing".to_string(),
            symbols: vec![],
            blast_radius: vec![],
            stale_files: vec![],
        };
        assert!(result.format().contains("No symbols found"));
    }

    #[test]
    fn impact_walks_transitive_callers() {
        let db = Db::open_memory().unwrap();
        db.upsert_file(&FileRecord {
            id: "f".to_string(),
            path: "a.rs".to_string(),
            language: "rust".to_string(),
            content_hash: "h".to_string(),
            size: 0,
            last_indexed: 0,
        })
        .unwrap();

        let a = node("a", "a.rs");
        let b = node("b", "a.rs");
        let c = node("c", "a.rs");
        for n in [&a, &b, &c] {
            db.upsert_node(n).unwrap();
        }
        // b calls a; c calls b  =>  impact(a) = {b, c}
        let edge = |s: &Node, t: &Node| Edge {
            id: Edge::new_id(&s.id, &t.id, &EdgeKind::Calls),
            source: s.id.clone(),
            target: t.id.clone(),
            kind: EdgeKind::Calls,
            provenance: Provenance::TreeSitter,
            metadata: None,
        };
        db.upsert_edge(&edge(&b, &a)).unwrap();
        db.upsert_edge(&edge(&c, &b)).unwrap();

        let graph = Graph::new(Arc::new(Mutex::new(db)));
        let impacts = graph.impact(&a.id, 5).unwrap();
        let names: HashSet<&str> = impacts.iter().map(|i| i.node.name.as_str()).collect();
        assert!(names.contains("b"));
        assert!(names.contains("c"));
    }

    #[test]
    fn call_path_finds_route_between_symbols() {
        let db = Db::open_memory().unwrap();
        db.upsert_file(&FileRecord {
            id: "f".to_string(),
            path: "a.rs".to_string(),
            language: "rust".to_string(),
            content_hash: "h".to_string(),
            size: 0,
            last_indexed: 0,
        })
        .unwrap();
        let a = node("a", "a.rs");
        let b = node("b", "a.rs");
        let c = node("c", "a.rs");
        for n in [&a, &b, &c] {
            db.upsert_node(n).unwrap();
        }
        // a calls b; b calls c  =>  path(a, c) = [a, b, c]
        let edge = |s: &Node, t: &Node| Edge {
            id: Edge::new_id(&s.id, &t.id, &EdgeKind::Calls),
            source: s.id.clone(),
            target: t.id.clone(),
            kind: EdgeKind::Calls,
            provenance: Provenance::TreeSitter,
            metadata: None,
        };
        db.upsert_edge(&edge(&a, &b)).unwrap();
        db.upsert_edge(&edge(&b, &c)).unwrap();

        let graph = Graph::new(Arc::new(Mutex::new(db)));
        let path = graph
            .call_path(&a.id, &c.id)
            .unwrap()
            .expect("a path exists");
        let names: Vec<&str> = path.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);

        // No edge back from c to a.
        assert!(graph.call_path(&c.id, &a.id).unwrap().is_none());
    }

    #[test]
    fn explore_flags_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "pub fn widget() {}\n").unwrap();
        let fpath = f.to_string_lossy().to_string();

        let db = Db::open_memory().unwrap();
        db.upsert_file(&FileRecord {
            id: Node::new_id(&fpath, &fpath),
            path: fpath.clone(),
            language: "rust".to_string(),
            content_hash: "stale-hash-that-wont-match".to_string(),
            size: 0,
            last_indexed: 0,
        })
        .unwrap();
        let n = node("widget", &fpath);
        db.upsert_node(&n).unwrap();

        let graph = Graph::new(Arc::new(Mutex::new(db)));
        let result = graph
            .explore("widget", dir.path().to_str().unwrap(), &SourceMap::new())
            .unwrap();
        assert!(
            result.stale_files.contains(&fpath),
            "file whose hash differs from the index must be flagged stale: {:?}",
            result.stale_files
        );
    }

    #[test]
    fn explore_caps_source_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.rs");
        // Write a function with 200 lines — more than MAX_SOURCE_LINES.
        let mut src = "pub fn big() {\n".to_string();
        for i in 0..200 {
            src.push_str(&format!("    let _ = {};\n", i));
        }
        src.push_str("}\n");
        std::fs::write(&f, &src).unwrap();
        let fpath = f.to_string_lossy().to_string();

        let db = Db::open_memory().unwrap();
        db.upsert_file(&FileRecord {
            id: Node::new_id(&fpath, &fpath),
            path: fpath.clone(),
            language: "rust".to_string(),
            content_hash: "h".to_string(),
            size: src.len() as u64,
            last_indexed: 0,
        })
        .unwrap();
        let mut n = node("big", &fpath);
        n.start_line = 1;
        n.end_line = 202;
        db.upsert_node(&n).unwrap();

        let graph = Graph::new(Arc::new(Mutex::new(db)));
        let result = graph
            .explore("big", dir.path().to_str().unwrap(), &SourceMap::new())
            .unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert!(
            result.symbols[0].source_lines.len() <= Graph::MAX_SOURCE_LINES,
            "source lines must be capped: got {}",
            result.symbols[0].source_lines.len()
        );
    }

    #[test]
    fn impact_respects_depth_limit() {
        let db = Db::open_memory().unwrap();
        db.upsert_file(&FileRecord {
            id: "f".to_string(),
            path: "a.rs".to_string(),
            language: "rust".to_string(),
            content_hash: "h".to_string(),
            size: 0,
            last_indexed: 0,
        })
        .unwrap();
        let a = node("a", "a.rs");
        let b = node("b", "a.rs");
        let c = node("c", "a.rs");
        for n in [&a, &b, &c] {
            db.upsert_node(n).unwrap();
        }
        let edge = |s: &Node, t: &Node| Edge {
            id: Edge::new_id(&s.id, &t.id, &EdgeKind::Calls),
            source: s.id.clone(),
            target: t.id.clone(),
            kind: EdgeKind::Calls,
            provenance: Provenance::TreeSitter,
            metadata: None,
        };
        db.upsert_edge(&edge(&b, &a)).unwrap();
        db.upsert_edge(&edge(&c, &b)).unwrap();

        let graph = Graph::new(Arc::new(Mutex::new(db)));
        // Depth 1 only reaches direct callers of a (b), not c.
        let impacts = graph.impact(&a.id, 1).unwrap();
        let names: HashSet<&str> = impacts.iter().map(|i| i.node.name.as_str()).collect();
        assert!(names.contains("b"));
        assert!(!names.contains("c"));
    }
}
