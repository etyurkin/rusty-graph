mod go;
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
