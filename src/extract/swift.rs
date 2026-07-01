use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::{keyword_among, Builder};
use super::ExtractionResult;
use crate::types::NodeKind;

pub struct SwiftExtractor;

impl super::Extractor for SwiftExtractor {
    fn language(&self) -> &'static str {
        "swift"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_swift::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "swift");
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
            "class_declaration" => self.type_decl(node),
            "protocol_declaration" => self.named_type(node, NodeKind::Protocol),
            "function_declaration" | "init_declaration" => self.function(node),
            "call_expression" => {
                let name = self.call_target(node).map(str::to_string);
                if let Some(name) = name {
                    self.b.record_call(&name);
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

    fn call_target(&self, node: TsNode) -> Option<&str> {
        let first = node.named_child(0)?;
        match first.kind() {
            "simple_identifier" => Some(self.b.text(first)),
            "navigation_expression" => {
                // a.b() -> capture the suffix identifier
                let suffix = first
                    .named_children(&mut first.walk())
                    .filter(|c| c.kind() == "navigation_suffix")
                    .last()?;
                suffix.named_child(0).map(|n| self.b.text(n))
            }
            _ => None,
        }
    }

    fn type_decl(&mut self, node: TsNode) {
        // `class_declaration` folds class/struct/enum/extension/actor; the
        // leading keyword token disambiguates.
        let kind = match keyword_among(node, &["struct", "enum", "protocol", "extension", "actor"])
            .map(|k| k.kind())
        {
            Some("struct") => NodeKind::Struct,
            Some("enum") => NodeKind::Enum,
            Some("protocol") => NodeKind::Protocol,
            _ => NodeKind::Class,
        };
        self.named_type(node, kind);
    }

    fn named_type(&mut self, node: TsNode, kind: NodeKind) {
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
        let name = node
            .child_by_field_name("name")
            .filter(|n| n.kind() == "simple_identifier")
            .map(|n| self.b.text(n))
            .or_else(|| {
                node.named_children(&mut node.walk())
                    .find(|c| c.kind() == "simple_identifier")
                    .map(|c| self.b.text(c))
            })
            .unwrap_or("<fn>");
        let in_type = matches!(
            self.b.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Struct | NodeKind::Enum | NodeKind::Protocol)
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
