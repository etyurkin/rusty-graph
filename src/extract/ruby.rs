use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::Builder;
use super::ExtractionResult;
use crate::types::{EdgeKind, NodeKind};

pub struct RubyExtractor;

impl super::Extractor for RubyExtractor {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_ruby::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "ruby");
        let mut ctx = Ctx { b: &mut b };
        ctx.walk(tree.root_node());
        Ok(b.finish())
    }
}

struct Ctx<'a, 'b> {
    b: &'a mut Builder<'b>,
}

impl Ctx<'_, '_> {
    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "module" => self.container(node, NodeKind::Module),
            "class" => self.class(node),
            "method" | "singleton_method" => self.method(node),
            "call" => {
                if let Some(m) = node.child_by_field_name("method") {
                    self.b.record_call(self.b.text(m));
                }
                self.recurse(node);
            }
            _ => self.recurse(node),
        }
    }

    fn recurse(&mut self, node: TsNode) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.walk(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn container(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<mod>");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn class(&mut self, node: TsNode) {
        let name = self.b.child_text(node, "name").unwrap_or("<class>");
        let n = self
            .b
            .make_node(NodeKind::Class, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());
        if let Some(sc) = node.child_by_field_name("superclass") {
            // superclass node wraps the parent constant
            let parent = sc.named_child(0).map(|c| self.b.text(c)).unwrap_or("");
            if !parent.is_empty() {
                self.b.enter(n.clone());
                self.b.record_ref(parent, EdgeKind::Extends);
                self.b.exit();
            }
        }
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn method(&mut self, node: TsNode) {
        let name = self.b.child_text(node, "name").unwrap_or("<fn>");
        let in_type = matches!(
            self.b.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Module)
        );
        let kind = if in_type {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }
}
