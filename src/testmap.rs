//! Map application symbols to the tests that exercise them. Given a symbol, we
//! walk the *caller* chain and collect any test functions we reach — so a change
//! to a symbol yields exactly the tests worth running. Pairs with `gitdiff` to
//! turn a patch into a minimal test set, cutting CI cost.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;

use crate::db::Db;
use crate::types::{Node, NodeKind};

/// Heuristic: does this node look like a test?
///
/// Tests are functions/methods that either live in a conventional test location
/// or carry a conventional test name, across the languages we index.
pub fn is_test_node(node: &Node) -> bool {
    if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }
    is_test_path(&node.file_path)
        || is_test_name(&node.name)
        || in_test_module(&node.qualified_name)
}

/// Recognize a conventional test module in a qualified name, e.g. Rust's
/// `crate::foo::tests::case` or `MyClassTest::method`.
fn in_test_module(qualified: &str) -> bool {
    let q = qualified.to_ascii_lowercase();
    q.contains("::tests::")
        || q.contains("::test::")
        || q.contains(".tests.")
        || q.contains("#[test]")
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    if p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/spec/")
        || p.contains("/__tests__/")
    {
        return true;
    }
    let base = basename(&p);
    base.starts_with("test_")
        || base.ends_with("_test.go")
        || base.ends_with("_test.py")
        || base.ends_with("_test.rb")
        || base.ends_with("_spec.rb")
        || base.ends_with("_test.rs")
        || base.ends_with(".test.ts")
        || base.ends_with(".test.tsx")
        || base.ends_with(".test.js")
        || base.ends_with(".test.jsx")
        || base.ends_with(".spec.ts")
        || base.ends_with(".spec.tsx")
        || base.ends_with(".spec.js")
        || base.ends_with("test.java")
        || base.ends_with("tests.cs")
        || base.ends_with("test.kt")
        || base.ends_with("_test.dart")
}

/// High-precision name patterns only. Loose matches like a bare `test` prefix
/// would catch helpers (`test_runner`, `latest`); we require snake-anchored
/// forms and lean on path / test-module signals for everything else.
fn is_test_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "test" || n.starts_with("test_") || n.ends_with("_test") || n.ends_with("_spec")
}

/// Tests that transitively call `node_id`, walking up to `depth` caller hops.
/// Results are unique and ordered by qualified name for stable output.
pub fn related_tests(db: &Db, node_id: &str, depth: usize) -> Result<Vec<Node>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut found: Vec<Node> = Vec::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(node_id.to_string());
    queue.push_back((node_id.to_string(), 0));

    while let Some((id, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for caller in db.callers(&id, 200)? {
            if !visited.insert(caller.id.clone()) {
                continue;
            }
            if is_test_node(&caller) {
                found.push(caller.clone());
            }
            queue.push_back((caller.id, d + 1));
        }
    }

    found.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    found.dedup_by(|a, b| a.id == b.id);
    Ok(found)
}

/// Union of tests covering any of `node_ids` (used for diff → test selection).
pub fn tests_for_nodes(db: &Db, node_ids: &[String], depth: usize) -> Result<Vec<Node>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Node> = Vec::new();
    for id in node_ids {
        for t in related_tests(db, id, depth)? {
            if seen.insert(t.id.clone()) {
                out.push(t);
            }
        }
    }
    out.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Indexer;
    use std::sync::{Arc, Mutex};

    fn node(name: &str, file: &str, kind: NodeKind) -> Node {
        Node {
            id: Node::new_id(file, name),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
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
    fn detects_tests_by_name_and_path() {
        assert!(is_test_node(&node(
            "test_run",
            "src/a.rs",
            NodeKind::Function
        )));
        assert!(is_test_node(&node(
            "run",
            "src/tests/a.rs",
            NodeKind::Function
        )));
        assert!(is_test_node(&node(
            "shouldWork",
            "a.test.ts",
            NodeKind::Function
        )));
        assert!(!is_test_node(&node("run", "src/a.rs", NodeKind::Function)));
        // Non-callable kinds are never tests.
        assert!(!is_test_node(&node(
            "TestThing",
            "src/tests/a.rs",
            NodeKind::Struct
        )));
    }

    #[test]
    fn related_tests_walks_caller_chain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn helper() -> i32 { 1 }\n\
             pub fn run() -> i32 { helper() }\n\
             pub fn test_run() { let _ = run(); }\n",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        Indexer::new(db.clone(), dir.path().to_path_buf())
            .index_all(true, true)
            .unwrap();

        let g = db.lock().unwrap();
        let helper = g.find_node_by_name("helper").unwrap();
        // helper <- run <- test_run : test reachable at depth 2.
        let tests = related_tests(&g, &helper[0].id, 6).unwrap();
        assert!(
            tests.iter().any(|t| t.name == "test_run"),
            "test_run should cover helper transitively: {:?}",
            tests.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }
}
