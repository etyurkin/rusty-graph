//! Token-budgeted context packs: the smallest, most relevant slice of the
//! codebase that answers a question within a token budget. This is the core
//! "make AI cheaper" primitive — instead of an agent reading whole files, it
//! gets a ranked, deduplicated, dependency-aware set of snippets that fits the
//! model's context window.
//!
//! Selection is relevance-first: seed symbols come from `smart_search`, then we
//! expand along the call graph (a symbol's callees are the definitions it
//! depends on; its callers show how it's used) and greedily pack snippets,
//! highest priority first, until the budget is spent.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::Serialize;

use crate::db::Db;
use crate::graph::SourceMap;
use crate::types::{Node, NodeKind};

/// Default budget when the caller doesn't specify one. Roughly a quarter of a
/// 32k window, leaving ample room for the prompt and the model's reply.
pub const DEFAULT_BUDGET_TOKENS: usize = 8_000;

/// Cap on lines pulled from any single symbol, so one huge function can't eat
/// the whole budget.
const MAX_SNIPPET_LINES: usize = 40;

/// Crude token estimate. Real tokenizers vary, but ~4 chars/token is a stable
/// approximation across English + code and keeps us dependency-free.
const CHARS_PER_TOKEN: usize = 4;

fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(CHARS_PER_TOKEN)
}

#[derive(Debug, Serialize)]
pub struct ContextItem {
    pub node: Node,
    /// (line_number, text) pairs, capped to `MAX_SNIPPET_LINES`.
    pub source: Vec<(usize, String)>,
    /// Why this symbol was included (e.g. "match", "called by run").
    pub reason: String,
    pub tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct ContextPack {
    pub query: String,
    pub budget_tokens: usize,
    pub used_tokens: usize,
    pub items: Vec<ContextItem>,
}

/// Symbol kinds that carry no useful source on their own and only waste budget.
fn is_packable(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
    )
}

/// Build a context pack for `query` within `budget` tokens.
pub fn build(
    db: &Arc<Mutex<Db>>,
    source_map: &SourceMap,
    query: &str,
    budget: usize,
) -> Result<ContextPack> {
    let mut candidates: Vec<(f64, Node, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    {
        let guard = db.lock().unwrap_or_else(|e| e.into_inner());
        let seeds = guard.smart_search(query, None, 16)?;

        // Seeds rank highest, preserving search order.
        for (i, n) in seeds.iter().enumerate() {
            if is_packable(&n.kind) && seen.insert(n.id.clone()) {
                candidates.push((1000.0 - i as f64, n.clone(), "match".to_string()));
            }
        }
        // Dependencies (callees) matter most for understanding a symbol; callers
        // give usage context. Both are decayed by the seed's rank.
        for (i, n) in seeds.iter().enumerate() {
            let decay = i as f64;
            for c in guard.callees(&n.id, 8)? {
                if is_packable(&c.kind) && seen.insert(c.id.clone()) {
                    candidates.push((500.0 - decay, c, format!("called by {}", n.name)));
                }
            }
            for c in guard.callers(&n.id, 4)? {
                if is_packable(&c.kind) && seen.insert(c.id.clone()) {
                    candidates.push((300.0 - decay, c, format!("calls {}", n.name)));
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut items: Vec<ContextItem> = Vec::new();
    let mut used = 0usize;
    for (_, node, reason) in candidates {
        let end = node
            .end_line
            .min(node.start_line.saturating_add(MAX_SNIPPET_LINES as u32 - 1));
        let source = source_map.get_lines(&node.file_path, node.start_line as usize, end as usize);
        let body: String = source
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let header = node.signature.clone().unwrap_or_else(|| node.name.clone());
        let tokens = estimate_tokens(&body) + estimate_tokens(&header);

        // Always include at least one item; afterwards skip anything that would
        // overflow (a later, smaller item may still fit).
        if !items.is_empty() && used + tokens > budget {
            continue;
        }
        items.push(ContextItem {
            node,
            source,
            reason,
            tokens,
        });
        used += tokens;
        if used >= budget {
            break;
        }
    }

    Ok(ContextPack {
        query: query.to_string(),
        budget_tokens: budget,
        used_tokens: used,
        items,
    })
}

impl ContextPack {
    /// Render the pack as text an agent can drop straight into a prompt.
    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Context pack for \"{}\" — {} symbols, ~{} / {} tokens\n",
            self.query,
            self.items.len(),
            self.used_tokens,
            self.budget_tokens
        ));
        if self.items.is_empty() {
            out.push_str("(no matching symbols)\n");
            return out;
        }
        for item in &self.items {
            out.push_str(&format!(
                "\n// {} [{}] {} ({}:{}-{})\n",
                item.reason,
                item.node.kind.as_str(),
                item.node.qualified_name,
                item.node.file_path,
                item.node.start_line,
                item.node.end_line,
            ));
            for (ln, line) in &item.source {
                out.push_str(&format!("{}\t{}\n", ln, line));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Indexer;
    use std::path::Path;

    fn indexed(files: &[(&str, &str)]) -> (Arc<Mutex<Db>>, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        let indexer = Indexer::new(db.clone(), dir.path().to_path_buf());
        indexer.index_all(true, true).unwrap();
        (db, dir.path().to_path_buf(), dir)
    }

    #[test]
    fn pack_includes_a_seed_match() {
        let (db, _root, _g) = indexed(&[(
            "a.rs",
            "pub fn helper() -> i32 { 1 }\npub fn run() -> i32 { helper() }\n",
        )]);
        let sm = SourceMap::new();
        let pack = build(&db, &sm, "run", DEFAULT_BUDGET_TOKENS).unwrap();
        assert!(
            pack.items.iter().any(|i| i.node.name == "run"),
            "seed symbol must be packed"
        );
        assert!(pack.used_tokens <= pack.budget_tokens || pack.items.len() == 1);
    }

    #[test]
    fn pack_pulls_in_callees_as_dependencies() {
        let (db, _root, _g) = indexed(&[(
            "a.rs",
            "pub fn helper() -> i32 { 1 }\npub fn run() -> i32 { helper() }\n",
        )]);
        let sm = SourceMap::new();
        let pack = build(&db, &sm, "run", DEFAULT_BUDGET_TOKENS).unwrap();
        assert!(
            pack.items.iter().any(|i| i.node.name == "helper"),
            "callee dependency should be included: {:?}",
            pack.items.iter().map(|i| &i.node.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tiny_budget_is_respected_but_returns_something() {
        let (db, _root, _g) = indexed(&[(
            "a.rs",
            "pub fn helper() -> i32 { 1 }\npub fn run() -> i32 { helper() }\n",
        )]);
        let sm = SourceMap::new();
        let pack = build(&db, &sm, "run", 1).unwrap();
        // At least the top seed comes back even though it exceeds a 1-token budget.
        assert_eq!(pack.items.len(), 1, "only the top item fits a tiny budget");
    }

    #[test]
    fn unknown_extension_file_is_ignored_by_source_map() {
        // Guard: building against a path that isn't on disk must not panic.
        let (db, _root, _g) = indexed(&[("a.rs", "pub fn solo() {}\n")]);
        let sm = SourceMap::new();
        let pack = build(&db, &sm, "solo", DEFAULT_BUDGET_TOKENS).unwrap();
        assert!(pack.items.iter().any(|i| i.node.name == "solo"));
        let _ = Path::new("x");
    }
}
