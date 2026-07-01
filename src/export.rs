//! Export the knowledge graph to interoperable formats:
//!   - `json` — full nodes+edges, for ad-hoc tooling.
//!   - `dot`  — Graphviz, for rendering.
//!   - `csv`  — node/edge tables, for Gephi/pandas.
//!   - `lsif` — a Language Server Index Format subset (metaData, project,
//!     documents, ranges, hover), for code-intel ecosystems.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde_json::json;

use crate::db::Db;

pub fn export(db: &Db, format: &str) -> Result<String> {
    match format {
        "json" => export_json(db),
        "dot" => export_dot(db),
        "csv" => export_csv(db),
        "lsif" => export_lsif(db),
        other => bail!("unknown export format '{other}' (expected json|dot|csv|lsif)"),
    }
}

fn export_json(db: &Db) -> Result<String> {
    let nodes = db.all_nodes()?;
    let edges: Vec<_> = db
        .all_edges_typed()?
        .into_iter()
        .map(|(s, t, k)| json!({ "source": s, "target": t, "kind": k }))
        .collect();
    let doc = json!({ "nodes": nodes, "edges": edges });
    Ok(serde_json::to_string_pretty(&doc)?)
}

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn export_dot(db: &Db) -> Result<String> {
    let nodes = db.all_nodes()?;
    let edges = db.all_edges_typed()?;
    let mut out = String::from("digraph rusty-graph {\n  rankdir=LR;\n  node [shape=box];\n");
    for n in &nodes {
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\\n{}\"];\n",
            dot_escape(&n.id),
            dot_escape(&n.name),
            dot_escape(n.kind.as_str()),
        ));
    }
    for (s, t, k) in &edges {
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
            dot_escape(s),
            dot_escape(t),
            dot_escape(k),
        ));
    }
    out.push_str("}\n");
    Ok(out)
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn export_csv(db: &Db) -> Result<String> {
    let nodes = db.all_nodes()?;
    let edges = db.all_edges_typed()?;
    let mut out = String::new();
    out.push_str("# nodes\n");
    out.push_str("id,kind,name,qualified_name,file_path,start_line,end_line\n");
    for n in &nodes {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            csv_escape(&n.id),
            csv_escape(n.kind.as_str()),
            csv_escape(&n.name),
            csv_escape(&n.qualified_name),
            csv_escape(&n.file_path),
            n.start_line,
            n.end_line,
        ));
    }
    out.push_str("\n# edges\nsource,target,kind\n");
    for (s, t, k) in &edges {
        out.push_str(&format!(
            "{},{},{}\n",
            csv_escape(s),
            csv_escape(t),
            csv_escape(k)
        ));
    }
    Ok(out)
}

/// Emit a newline-delimited LSIF document. We cover the def-less subset that is
/// universally useful and unambiguous: project → documents → ranges, with a
/// hover (signature) per ranged symbol. Each line is an independent JSON object.
fn export_lsif(db: &Db) -> Result<String> {
    let nodes = db.all_nodes()?;
    let mut lines: Vec<String> = Vec::new();
    let mut next_id: u64 = 0;
    let mut id = || {
        next_id += 1;
        next_id
    };

    let meta_id = id();
    lines.push(
        json!({
            "id": meta_id, "type": "vertex", "label": "metaData",
            "version": "0.6.0", "positionEncoding": "utf-16",
            "toolInfo": { "name": "codegraph" }
        })
        .to_string(),
    );
    let project_id = id();
    lines.push(
        json!({"id": project_id, "type": "vertex", "label": "project", "kind": "multi"})
            .to_string(),
    );

    // Group nodes by file so each becomes one document with its ranges.
    let mut by_file: BTreeMap<String, Vec<&crate::types::Node>> = BTreeMap::new();
    for n in &nodes {
        by_file.entry(n.file_path.clone()).or_default().push(n);
    }

    let mut document_ids: Vec<u64> = Vec::new();
    for (file, file_nodes) in &by_file {
        let doc_id = id();
        document_ids.push(doc_id);
        let lang = file_nodes
            .first()
            .map(|n| n.language.clone())
            .unwrap_or_default();
        lines.push(
            json!({
                "id": doc_id, "type": "vertex", "label": "document",
                "uri": format!("file://{}", file), "languageId": lang
            })
            .to_string(),
        );

        let mut range_ids: Vec<u64> = Vec::new();
        for n in file_nodes {
            let range_id = id();
            range_ids.push(range_id);
            let start_line = n.start_line.saturating_sub(1);
            let end_line = n.end_line.saturating_sub(1);
            lines.push(
                json!({
                    "id": range_id, "type": "vertex", "label": "range",
                    "start": {"line": start_line, "character": 0},
                    "end": {"line": end_line, "character": 0}
                })
                .to_string(),
            );
            // Attach a hover (the signature) via a resultSet, the standard shape.
            let rs_id = id();
            lines.push(json!({"id": rs_id, "type": "vertex", "label": "resultSet"}).to_string());
            lines.push(
                json!({"id": id(), "type": "edge", "label": "next", "outV": range_id, "inV": rs_id})
                    .to_string(),
            );
            let hover_id = id();
            let signature = n.signature.clone().unwrap_or_else(|| n.name.clone());
            lines.push(
                json!({
                    "id": hover_id, "type": "vertex", "label": "hoverResult",
                    "result": {"contents": [{"language": n.language, "value": signature}]}
                })
                .to_string(),
            );
            lines.push(
                json!({"id": id(), "type": "edge", "label": "textDocument/hover", "outV": rs_id, "inV": hover_id})
                    .to_string(),
            );
        }
        // document contains its ranges.
        if !range_ids.is_empty() {
            lines.push(
                json!({"id": id(), "type": "edge", "label": "contains", "outV": doc_id, "inVs": range_ids})
                    .to_string(),
            );
        }
    }

    // project contains its documents.
    if !document_ids.is_empty() {
        lines.push(
            json!({"id": id(), "type": "edge", "label": "contains", "outV": project_id, "inVs": document_ids})
                .to_string(),
        );
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Indexer;
    use std::sync::{Arc, Mutex};

    fn graph() -> Arc<Mutex<Db>> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn helper() -> i32 { 1 }\npub fn run() -> i32 { helper() }\n",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        Indexer::new(db.clone(), dir.path().to_path_buf())
            .index_all(true, true)
            .unwrap();
        db
    }

    #[test]
    fn json_export_has_nodes_and_edges() {
        let db = graph();
        let g = db.lock().unwrap();
        let out = export(&g, "json").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "run"));
        assert!(v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "calls"));
    }

    #[test]
    fn dot_export_is_a_digraph() {
        let db = graph();
        let g = db.lock().unwrap();
        let out = export(&g, "dot").unwrap();
        assert!(out.starts_with("digraph rusty-graph {"));
        assert!(out.contains("->"));
    }

    #[test]
    fn csv_export_has_both_sections() {
        let db = graph();
        let g = db.lock().unwrap();
        let out = export(&g, "csv").unwrap();
        assert!(out.contains("# nodes"));
        assert!(out.contains("# edges"));
    }

    #[test]
    fn lsif_lines_are_all_valid_json_with_metadata_first() {
        let db = graph();
        let g = db.lock().unwrap();
        let out = export(&g, "lsif").unwrap();
        let mut lines = out.lines();
        let first: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(first["label"], "metaData");
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("every LSIF line is valid JSON");
        }
        assert!(out.contains("\"label\":\"document\""));
        assert!(out.contains("\"label\":\"range\""));
    }

    #[test]
    fn unknown_format_errors() {
        let db = graph();
        let g = db.lock().unwrap();
        assert!(export(&g, "xml").is_err());
    }
}
