use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::{keyword_among, Builder};
use super::hints;
use super::ExtractionResult;
use crate::types::NodeKind;

pub struct KotlinExtractor;

impl super::Extractor for KotlinExtractor {
    fn language(&self) -> &'static str {
        "kotlin"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "kotlin");
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
            "class_declaration" | "object_declaration" => self.type_decl(node),
            "function_declaration" => self.function(node),
            "call_expression" => {
                if let Some((method, hint)) =
                    hints::resolve_kotlin_call(self.b.source, node, &self.b.var_types)
                {
                    self.b.record_typed_call(&method, hint);
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

    fn type_decl(&mut self, node: TsNode) {
        let kind = match keyword_among(node, &["interface", "enum"]).map(|k| k.kind()) {
            Some("interface") => NodeKind::Interface,
            Some("enum") => NodeKind::Enum,
            _ => NodeKind::Class,
        };
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
        let name = self.b.child_text(node, "name").unwrap_or("<fn>");
        let in_type = matches!(
            self.b.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Interface | NodeKind::Enum)
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
        let saved = std::mem::take(&mut self.b.var_types);
        hints::collect_kotlin_var_types(self.b.source, node, &mut self.b.var_types);
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
        self.b.var_types = saved;
    }
}
