//! Architecture report: structural insight an agent can't get from grep. We
//! mine the call graph for circular dependencies (SCCs), centrality hotspots
//! ("god" symbols), likely dead code (orphans), and cross-layer coupling.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::db::Db;
use crate::testmap::is_test_node;
use crate::types::{Node, NodeKind};

#[derive(Debug, Serialize)]
pub struct Hotspot {
    pub node: Node,
    pub fan_in: usize,
    pub fan_out: usize,
}

#[derive(Debug, Serialize)]
pub struct LayerCoupling {
    pub from: String,
    pub to: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct ArchReport {
    pub nodes: usize,
    pub call_edges: usize,
    /// Strongly-connected components of size > 1 in the call graph: circular
    /// dependencies, worst (largest) first.
    pub cycles: Vec<Vec<Node>>,
    /// Most-connected symbols (fan_in + fan_out), highest first.
    pub hotspots: Vec<Hotspot>,
    /// Functions/methods nothing calls and that aren't exported/tests/entry
    /// points — candidate dead code.
    pub orphans: Vec<Node>,
    /// Symbol count per top-level layer (first path segment under the root).
    pub layers: Vec<(String, usize)>,
    /// Call coupling between distinct layers, heaviest first.
    pub cross_layer: Vec<LayerCoupling>,
    /// Layer pairs that call each other in both directions (architecture smell).
    pub layer_cycles: Vec<(String, String)>,
}

const MAX_HOTSPOTS: usize = 15;
const MAX_ORPHANS: usize = 50;

/// Top-level layer for a path: the first segment beneath `root` (e.g. `src`,
/// `tests`, `cmd`). Files directly under the root map to `(root)`.
fn layer_of(path: &str, root: &str) -> String {
    let p = path.replace('\\', "/");
    let r = root.replace('\\', "/");
    let rel = p
        .strip_prefix(&r)
        .unwrap_or(&p)
        .trim_start_matches('/')
        .to_string();
    match rel.split_once('/') {
        Some((head, _)) => head.to_string(),
        None => "(root)".to_string(),
    }
}

/// Iterative Tarjan SCC. Returns components (as node indices); singletons with
/// no self-loop are included but filtered out by the caller.
fn strongly_connected(n: usize, adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNSET: usize = usize::MAX;
    let mut index = vec![UNSET; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut out: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if index[start] != UNSET {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, child)) = work.last() {
            if child == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if child < adj[v].len() {
                work.last_mut().unwrap().1 += 1;
                let w = adj[v][child];
                if index[w] == UNSET {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    out.push(comp);
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }
    out
}

pub fn report(db: &Db, root: &str) -> Result<ArchReport> {
    let nodes = db.all_nodes()?;
    let call_edges = db.call_edges()?;

    let index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();
    let n = nodes.len();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut fan_in = vec![0usize; n];
    let mut fan_out = vec![0usize; n];
    for (src, dst) in &call_edges {
        if let (Some(&u), Some(&v)) = (index.get(src), index.get(dst)) {
            adj[u].push(v);
            fan_out[u] += 1;
            fan_in[v] += 1;
        }
    }

    // Cycles: SCCs with more than one member (true circular dependencies).
    let mut cycles: Vec<Vec<Node>> = strongly_connected(n, &adj)
        .into_iter()
        .filter(|c| c.len() > 1)
        .map(|c| c.into_iter().map(|i| nodes[i].clone()).collect())
        .collect();
    cycles.sort_by_key(|b| std::cmp::Reverse(b.len()));

    // Hotspots.
    let mut hotspots: Vec<Hotspot> = (0..n)
        .filter(|&i| fan_in[i] + fan_out[i] > 0)
        .map(|i| Hotspot {
            node: nodes[i].clone(),
            fan_in: fan_in[i],
            fan_out: fan_out[i],
        })
        .collect();
    hotspots.sort_by_key(|b| std::cmp::Reverse(b.fan_in + b.fan_out));
    hotspots.truncate(MAX_HOTSPOTS);

    // Orphans: callable, uncalled, not exported / test / entry point, and inside
    // the project (external deps aren't our dead code).
    let root_norm = root.replace('\\', "/");
    let mut orphans: Vec<Node> = (0..n)
        .filter(|&i| {
            let node = &nodes[i];
            fan_in[i] == 0
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && !node.is_exported
                && !is_test_node(node)
                && node.name != "main"
                && node.file_path.replace('\\', "/").starts_with(&root_norm)
        })
        .map(|i| nodes[i].clone())
        .collect();
    orphans.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    orphans.truncate(MAX_ORPHANS);

    // Layers.
    let mut layer_counts: HashMap<String, usize> = HashMap::new();
    for node in &nodes {
        *layer_counts
            .entry(layer_of(&node.file_path, root))
            .or_insert(0) += 1;
    }
    let mut layers: Vec<(String, usize)> = layer_counts.into_iter().collect();
    layers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Cross-layer coupling.
    let mut coupling: HashMap<(String, String), usize> = HashMap::new();
    for (src, dst) in &call_edges {
        if let (Some(&u), Some(&v)) = (index.get(src), index.get(dst)) {
            let ls = layer_of(&nodes[u].file_path, root);
            let ld = layer_of(&nodes[v].file_path, root);
            if ls != ld {
                *coupling.entry((ls, ld)).or_insert(0) += 1;
            }
        }
    }
    let mut cross_layer: Vec<LayerCoupling> = coupling
        .iter()
        .map(|((f, t), c)| LayerCoupling {
            from: f.clone(),
            to: t.clone(),
            count: *c,
        })
        .collect();
    cross_layer.sort_by(|a, b| b.count.cmp(&a.count).then(a.from.cmp(&b.from)));

    // Layer cycles: pairs coupled in both directions.
    let mut layer_cycles: Vec<(String, String)> = Vec::new();
    for (f, t) in coupling.keys() {
        if f < t && coupling.contains_key(&(t.clone(), f.clone())) {
            layer_cycles.push((f.clone(), t.clone()));
        }
    }
    layer_cycles.sort();

    Ok(ArchReport {
        nodes: n,
        call_edges: call_edges.len(),
        cycles,
        hotspots,
        orphans,
        layers,
        cross_layer,
        layer_cycles,
    })
}

impl ArchReport {
    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Architecture: {} symbols, {} call edges\n",
            self.nodes, self.call_edges
        ));

        out.push_str(&format!("\nCircular dependencies: {}\n", self.cycles.len()));
        for (i, cycle) in self.cycles.iter().take(10).enumerate() {
            let names: Vec<&str> = cycle.iter().map(|n| n.name.as_str()).collect();
            out.push_str(&format!(
                "  {}. ({}) {}\n",
                i + 1,
                cycle.len(),
                names.join(" → ")
            ));
        }

        out.push_str("\nHotspots (fan-in / fan-out):\n");
        for h in &self.hotspots {
            out.push_str(&format!(
                "  {:>4} in {:>4} out  {}\n",
                h.fan_in, h.fan_out, h.node.qualified_name
            ));
        }

        out.push_str(&format!(
            "\nPossible dead code ({} orphans):\n",
            self.orphans.len()
        ));
        for o in self.orphans.iter().take(20) {
            out.push_str(&format!(
                "  {} ({}:{})\n",
                o.qualified_name, o.file_path, o.start_line
            ));
        }

        out.push_str("\nLayers:\n");
        for (layer, count) in &self.layers {
            out.push_str(&format!("  {:<16} {}\n", layer, count));
        }
        if !self.cross_layer.is_empty() {
            out.push_str("\nCross-layer coupling:\n");
            for c in self.cross_layer.iter().take(15) {
                out.push_str(&format!("  {} → {}  ({})\n", c.from, c.to, c.count));
            }
        }
        if !self.layer_cycles.is_empty() {
            out.push_str("\n⚠️  Layer cycles (bidirectional coupling):\n");
            for (a, b) in &self.layer_cycles {
                out.push_str(&format!("  {} ⇄ {}\n", a, b));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Indexer;
    use std::sync::{Arc, Mutex};

    #[test]
    fn detects_call_cycle() {
        let dir = tempfile::tempdir().unwrap();
        // ping <-> pong is a 2-cycle.
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn ping(n: i32) { if n > 0 { pong(n - 1) } }\n\
             pub fn pong(n: i32) { if n > 0 { ping(n - 1) } }\n",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        Indexer::new(db.clone(), dir.path().to_path_buf())
            .index_all(true, true)
            .unwrap();
        let g = db.lock().unwrap();
        let rep = report(&g, dir.path().to_str().unwrap()).unwrap();
        assert!(
            rep.cycles.iter().any(|c| {
                let names: Vec<&str> = c.iter().map(|n| n.name.as_str()).collect();
                names.contains(&"ping") && names.contains(&"pong")
            }),
            "ping/pong cycle must be detected: {:?}",
            rep.cycles
        );
    }

    #[test]
    fn flags_uncalled_private_function_as_orphan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn lonely() -> i32 { 42 }\npub fn used() {}\n",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        Indexer::new(db.clone(), dir.path().to_path_buf())
            .index_all(true, true)
            .unwrap();
        let g = db.lock().unwrap();
        let rep = report(&g, dir.path().to_str().unwrap()).unwrap();
        assert!(
            rep.orphans.iter().any(|o| o.name == "lonely"),
            "uncalled private fn should be an orphan: {:?}",
            rep.orphans.iter().map(|o| &o.name).collect::<Vec<_>>()
        );
    }
}
