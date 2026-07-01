//! Git-diff awareness: map the lines changed since a git ref to the symbols that
//! contain them, then to their blast radius (transitive callers). This answers
//! "what does this change affect?" — the highest-value question for review and
//! for agents reasoning about a patch.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};

use crate::db::Db;
use crate::graph::Graph;
use crate::types::Node;

pub struct ImpactedSymbol {
    pub node: Node,
    pub blast_radius: Vec<Node>,
}

pub struct DiffReport {
    pub base: String,
    pub changed_files: Vec<String>,
    pub symbols: Vec<ImpactedSymbol>,
}

/// Compute the symbols touched by the diff against `base` and their callers up
/// to `depth`. `project_root` must be the canonical project path so file paths
/// line up with what was indexed.
pub fn diff_impact(
    project_root: &Path,
    db: Arc<Mutex<Db>>,
    base: &str,
    depth: usize,
) -> Result<DiffReport> {
    let changed = changed_files(project_root, base)?;

    let graph = Graph::new(db.clone());
    let mut symbols: Vec<ImpactedSymbol> = vec![];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (rel, lines) in &changed {
        let abs = project_root.join(rel);
        let path = abs.to_string_lossy().to_string();
        let nodes = {
            let guard = db.lock().unwrap_or_else(|e| e.into_inner());
            guard.nodes_in_file(&path)?
        };
        for node in nodes {
            if node.kind == crate::types::NodeKind::File {
                continue;
            }
            let touched = lines
                .iter()
                .any(|&l| l >= node.start_line && l <= node.end_line);
            if !touched || !seen.insert(node.id.clone()) {
                continue;
            }
            let blast_radius = graph
                .impact(&node.id, depth)?
                .into_iter()
                .map(|i| i.node)
                .collect();
            symbols.push(ImpactedSymbol { node, blast_radius });
        }
    }

    Ok(DiffReport {
        base: base.to_string(),
        changed_files: changed.into_iter().map(|(f, _)| f).collect(),
        symbols,
    })
}

/// `(relative_path, changed_new_side_line_numbers)` for each file that differs
/// from `base`. Uses `-U0` so hunk headers describe exactly the changed lines.
fn changed_files(project_root: &Path, base: &str) -> Result<Vec<(String, Vec<u32>)>> {
    let names = run_git(project_root, &["diff", "--name-only", base])?;
    let mut out = vec![];
    for rel in names.lines().filter(|l| !l.trim().is_empty()) {
        let patch = run_git(project_root, &["diff", "-U0", base, "--", rel])?;
        out.push((rel.to_string(), changed_lines(&patch)));
    }
    Ok(out)
}

/// Parse `@@ -a,b +c,d @@` hunk headers, returning the new-side line numbers.
fn changed_lines(patch: &str) -> Vec<u32> {
    let mut lines = vec![];
    for hunk in patch.lines().filter(|l| l.starts_with("@@")) {
        // @@ -old +new @@ ; we want the +new span.
        if let Some(plus) = hunk.split('+').nth(1) {
            let span = plus.split([' ', '@']).next().unwrap_or("");
            let mut parts = span.split(',');
            let start: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let count: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            for l in start..start + count.max(1) {
                lines.push(l);
            }
        }
    }
    lines
}

fn run_git(project_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .output()
        .context("failed to run git (is it installed and on PATH?)")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

impl DiffReport {
    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Changes vs {}: {} files, {} impacted symbols\n",
            self.base,
            self.changed_files.len(),
            self.symbols.len()
        ));
        for sym in &self.symbols {
            out.push_str(&format!(
                "\n[{}] {} ({}:{}-{})\n",
                sym.node.kind.as_str(),
                sym.node.qualified_name,
                sym.node.file_path,
                sym.node.start_line,
                sym.node.end_line,
            ));
            if sym.blast_radius.is_empty() {
                out.push_str("  no known callers\n");
            } else {
                out.push_str(&format!(
                    "  affects {} caller(s):\n",
                    sym.blast_radius.len()
                ));
                for c in &sym.blast_radius {
                    out.push_str(&format!("    - {} ({})\n", c.qualified_name, c.file_path));
                }
            }
        }
        out
    }
}
