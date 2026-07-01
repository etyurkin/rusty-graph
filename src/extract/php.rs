use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::Builder;
use super::ExtractionResult;
use crate::types::{EdgeKind, NodeKind};

pub struct PhpExtractor;

impl super::Extractor for PhpExtractor {
    fn language(&self) -> &'static str {
        "php"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "php");
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
            "class_declaration" => self.type_decl(node, NodeKind::Class),
            "interface_declaration" => self.type_decl(node, NodeKind::Interface),
            "trait_declaration" => self.type_decl(node, NodeKind::Class),
            "enum_declaration" => self.type_decl(node, NodeKind::Enum),
            "method_declaration" => self.function(node, NodeKind::Method),
            "function_definition" => self.function(node, NodeKind::Function),
            "function_call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    self.b.record_call(self.b.text(f));
                }
                self.recurse(node);
            }
            "member_call_expression" | "scoped_call_expression" => {
                if let Some(n) = node.child_by_field_name("name") {
                    self.b.record_call(self.b.text(n));
                }
                self.recurse(node);
            }
            "object_creation_expression" => {
                let ty = node
                    .named_children(&mut node.walk())
                    .find(|c| matches!(c.kind(), "name" | "qualified_name"))
                    .map(|c| {
                        let t = self.b.text(c);
                        t.rsplit('\\').next().unwrap_or(t).to_string()
                    });
                if let Some(ty) = ty {
                    self.b.record_ref(&ty, EdgeKind::Instantiates);
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

    fn type_decl(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<type>");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "base_clause" => {
                    self.b.enter(n.clone());
                    for base in child.named_children(&mut child.walk()) {
                        let t = self.b.text(base);
                        self.b.record_ref(t, EdgeKind::Extends);
                    }
                    self.b.exit();
                }
                "class_interface_clause" => {
                    self.b.enter(n.clone());
                    for iface in child.named_children(&mut child.walk()) {
                        let t = self.b.text(iface);
                        self.b.record_ref(t, EdgeKind::Implements);
                    }
                    self.b.exit();
                }
                _ => {}
            }
        }

        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn function(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<fn>");
        let exported = !has_modifier(node, self.b.source, "private")
            && !has_modifier(node, self.b.source, "protected");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), exported);
        self.b.push(n.clone());
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }
}

fn has_modifier(node: TsNode, src: &str, want: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.kind() == "visibility_modifier" && src[c.byte_range()].contains(want));
    found
}
