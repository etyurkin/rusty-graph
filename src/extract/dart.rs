use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::Builder;
use super::ExtractionResult;
use crate::types::NodeKind;

pub struct DartExtractor;

impl super::Extractor for DartExtractor {
    fn language(&self) -> &'static str {
        "dart"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "dart");
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
            "class_declaration" | "mixin_declaration" => self.class(node),
            "enum_declaration" => self.named(node, NodeKind::Enum),
            "function_declaration" | "method_declaration" => self.function(node),
            "call_expression" => {
                let name = node.child_by_field_name("function").and_then(|f| {
                    if f.kind() == "identifier" {
                        Some(self.b.text(f).to_string())
                    } else {
                        f.named_children(&mut f.walk())
                            .filter(|c| c.kind() == "identifier")
                            .last()
                            .map(|c| self.b.text(c).to_string())
                    }
                });
                if let Some(name) = name {
                    if !name.is_empty() {
                        self.b.record_call(&name);
                    }
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

    fn class(&mut self, node: TsNode) {
        self.named(node, NodeKind::Class);
    }

    fn named(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<type>");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn function(&mut self, node: TsNode) {
        let name = sig_name(self.b, node).unwrap_or("<fn>");
        let in_type = matches!(
            self.b.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Enum)
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

/// Dart wraps the name inside `signature > (method_signature >) function_signature`.
/// Find the first `function_signature` descendant and return its `name` child.
fn sig_name<'a>(b: &Builder<'a>, node: TsNode) -> Option<&'a str> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(b.text(n));
    }
    let sig = node.child_by_field_name("signature")?;
    let fsig = find_kind(sig, "function_signature")?;
    fsig.child_by_field_name("name").map(|n| b.text(n))
}

fn find_kind<'b>(node: TsNode<'b>, kind: &str) -> Option<TsNode<'b>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_kind(child, kind) {
            return Some(found);
        }
    }
    None
}
