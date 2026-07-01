//! Shared scaffolding for tree-sitter extractors: a `Builder` that owns the
//! node/edge/ref accumulators, the scope stack, and the common helpers for
//! turning tree-sitter nodes into graph nodes. Newer extractors are written
//! against this to avoid repeating the same boilerplate per language.

use std::collections::HashMap;
use tree_sitter::Node as TsNode;

use super::hints::{self, VarTypes};
use super::util;
use super::ExtractionResult;
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct Builder<'a> {
    pub source: &'a str,
    pub file_path: &'a str,
    pub language: &'static str,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedRef>,
    pub scope_stack: Vec<Node>,
    /// Variable → type map for the function body currently being walked.
    pub var_types: VarTypes,
}

impl<'a> Builder<'a> {
    pub fn new(source: &'a str, file_path: &'a str, language: &'static str) -> Self {
        let file_node = Node {
            id: Node::new_id(file_path, file_path),
            kind: NodeKind::File,
            name: std::path::Path::new(file_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            language: language.to_string(),
            start_line: 1,
            end_line: source.lines().count() as u32,
            signature: None,
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        Self {
            source,
            file_path,
            language,
            nodes: vec![file_node.clone()],
            edges: vec![],
            unresolved: vec![],
            scope_stack: vec![file_node],
            var_types: HashMap::new(),
        }
    }

    pub fn text(&self, node: TsNode) -> &'a str {
        &self.source[node.byte_range()]
    }

    pub fn child_text(&self, node: TsNode, field: &str) -> Option<&'a str> {
        node.child_by_field_name(field).map(|n| self.text(n))
    }

    pub fn line(&self, node: TsNode) -> (u32, u32) {
        (
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
        )
    }

    pub fn qualified(&self, name: &str) -> String {
        match self.scope_stack.last() {
            Some(p) if p.kind != NodeKind::File => format!("{}::{}", p.qualified_name, name),
            _ => format!("{}::{}", self.file_path, name),
        }
    }

    /// Build a graph node anchored at the given tree-sitter node.
    pub fn make_node(
        &self,
        kind: NodeKind,
        name: &str,
        ts: TsNode,
        signature: Option<String>,
        exported: bool,
    ) -> Node {
        let qualified = self.qualified(name);
        let (start_line, end_line) = self.line(ts);
        Node {
            id: Node::new_id(self.file_path, &qualified),
            kind,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.language.to_string(),
            start_line,
            end_line,
            signature,
            docstring: None,
            visibility: None,
            is_exported: exported,
            is_async: false,
            is_static: false,
            is_abstract: false,
        }
    }

    /// Record a graph node and a `contains` edge from the current scope.
    pub fn push(&mut self, node: Node) {
        if let Some(parent) = self.scope_stack.last() {
            self.edges.push(util::contains_edge(parent, &node));
        }
        self.nodes.push(node);
    }

    pub fn enter(&mut self, node: Node) {
        self.scope_stack.push(node);
    }

    pub fn exit(&mut self) {
        self.scope_stack.pop();
    }

    /// Record an unresolved reference (call/extends/implements/imports) sourced
    /// from the current enclosing scope.
    pub fn record_ref(&mut self, target_name: &str, kind: EdgeKind) {
        if let Some(scope) = self.scope_stack.last() {
            self.unresolved.push(UnresolvedRef::new(
                scope.id.clone(),
                target_name.to_string(),
                kind,
                self.file_path.to_string(),
            ));
        }
    }

    pub fn record_call(&mut self, target_name: &str) {
        self.record_typed_call(target_name, None);
    }

    /// Record a call with an optional receiver/qualifier type hint.
    pub fn record_typed_call(&mut self, method: &str, hint: Option<String>) {
        if let Some(scope) = self.caller_scope() {
            let scope_id = scope.id.clone();
            hints::push_call(
                &mut self.unresolved,
                &scope_id,
                self.file_path,
                method,
                hint,
            );
        }
    }

    fn caller_scope(&self) -> Option<&Node> {
        self.scope_stack
            .iter()
            .rev()
            .find(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
            .or_else(|| self.scope_stack.last())
    }

    pub fn finish(self) -> ExtractionResult {
        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved: self.unresolved,
        }
    }
}

/// Return the first child token whose kind matches one of `kinds` (anonymous
/// keyword tokens like `class`/`struct`). Useful for grammars that fold several
/// declaration types into one node distinguished by a leading keyword.
pub fn keyword_among<'b>(node: TsNode<'b>, kinds: &[&str]) -> Option<TsNode<'b>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if kinds.contains(&child.kind()) {
                return Some(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}
