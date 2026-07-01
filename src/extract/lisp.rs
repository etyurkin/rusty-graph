use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use lisp_sitter_core::{is_curried_define, DefinerSet};
use lisp_sitter_cl as lisp_cl;
use lisp_sitter_elisp as lisp_elisp;
use lisp_sitter_scheme as lisp_scheme;

use super::{util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Elisp,
    CommonLisp,
    Scheme,
}

impl Dialect {
    fn id(self) -> &'static str {
        match self {
            Dialect::Elisp => "elisp",
            Dialect::CommonLisp => "commonlisp",
            Dialect::Scheme => "scheme",
        }
    }

    fn language(self) -> tree_sitter::Language {
        match self {
            Dialect::Elisp => tree_sitter_elisp::LANGUAGE.into(),
            Dialect::CommonLisp => tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
            Dialect::Scheme => tree_sitter_scheme::LANGUAGE.into(),
        }
    }

    fn definer_set(self) -> DefinerSet {
        match self {
            Dialect::Elisp => lisp_elisp::definer_set(),
            Dialect::CommonLisp => lisp_cl::definer_set(),
            Dialect::Scheme => lisp_scheme::definer_set(),
        }
    }
}

pub struct LispExtractor {
    pub dialect: Dialect,
}

impl super::Extractor for LispExtractor {
    fn language(&self) -> &'static str {
        self.dialect.id()
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&self.dialect.language())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let lang = self.dialect.id();
        let file_path = path.to_string_lossy().to_string();
        let file_node = Node {
            id: Node::new_id(&file_path, &file_path),
            kind: NodeKind::File,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            qualified_name: file_path.clone(),
            file_path: file_path.clone(),
            language: lang.to_string(),
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

        let definers = self.dialect.definer_set();
        let mut ctx = LispCtx {
            source,
            file_path: &file_path,
            language: lang,
            definers,
            file_node: file_node.clone(),
            nodes: vec![file_node],
            edges: vec![],
            unresolved: vec![],
        };

        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            ctx.visit_top_level(child);
        }

        Ok(ExtractionResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
            unresolved: ctx.unresolved,
        })
    }
}

struct LispCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    language: &'a str,
    definers: DefinerSet,
    file_node: Node,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
}

impl<'a> LispCtx<'a> {
    fn visit_top_level(&mut self, node: TsNode) {
        if node.kind() == "comment" {
            return;
        }
        let text = &self.source[node.byte_range()];
        let Some((head, name)) = self.definers.classify(text) else {
            return;
        };

        let kind = kind_for(&head, text);
        let qualified = format!("{}::{}", self.file_path, name);
        let id = Node::new_id(self.file_path, &qualified);
        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;
        let signature = text.lines().next().unwrap_or("").trim().to_string();

        let def_node = Node {
            id: id.clone(),
            kind,
            name,
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.language.to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };

        self.edges
            .push(util::contains_edge(&self.file_node, &def_node));
        self.nodes.push(def_node);

        self.walk_for_calls(node, &id);
    }

    /// Record every symbol-headed list inside a form as a potential call.
    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if matches!(node.kind(), "list" | "list_lit") {
            if let Some(head) = first_named_child(node) {
                if matches!(head.kind(), "symbol" | "sym_lit") {
                    let raw = &self.source[head.byte_range()];
                    let name = raw.trim_start_matches(['\'', '`', ',', '#']);
                    if !name.is_empty() {
                        self.unresolved.push(UnresolvedRef::new(
                            caller_id.to_string(),
                            name.to_string(),
                            EdgeKind::Calls,
                            self.file_path.to_string(),
                        ));
                    }
                }
            }
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.walk_for_calls(cursor.node(), caller_id);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
}

fn first_named_child(node: TsNode) -> Option<TsNode> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() && child.kind() != "comment" {
                return Some(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

fn kind_for(head: &str, form_text: &str) -> NodeKind {
    match head {
        "defclass" | "defstruct" | "cl-defstruct" | "define-record-type" | "define-structure"
        | "deftype" | "define-condition" => NodeKind::Struct,
        "defpackage" | "in-package" | "define-library" => NodeKind::Module,
        "defconst" | "defconstant" => NodeKind::Constant,
        "defvar" | "defvar-local" | "defcustom" | "defparameter" | "defgroup" | "defface" => {
            NodeKind::Variable
        }
        "define" | "define-values" => {
            if is_curried_define(form_text) {
                NodeKind::Function
            } else {
                NodeKind::Variable
            }
        }
        _ => NodeKind::Function,
    }
}
