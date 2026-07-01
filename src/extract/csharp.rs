use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::common::Builder;
use super::hints;
use super::ExtractionResult;
use crate::types::{EdgeKind, NodeKind};

pub struct CSharpExtractor;

impl super::Extractor for CSharpExtractor {
    fn language(&self) -> &'static str {
        "csharp"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut b = Builder::new(source, &file_path, "csharp");
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
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                self.container(node, NodeKind::Namespace)
            }
            "class_declaration" | "record_declaration" => self.type_decl(node, NodeKind::Class),
            "struct_declaration" => self.type_decl(node, NodeKind::Struct),
            "interface_declaration" => self.type_decl(node, NodeKind::Interface),
            "enum_declaration" => self.type_decl(node, NodeKind::Enum),
            "method_declaration" | "constructor_declaration" => self.method(node),
            "invocation_expression" => {
                if let Some((method, hint)) =
                    hints::resolve_csharp_invocation(self.b.source, node, &self.b.var_types)
                {
                    self.b.record_typed_call(&method, hint);
                }
                self.recurse(node);
            }
            "object_creation_expression" => {
                let ty = node.child_by_field_name("type").map(|t| {
                    let t = self.b.text(t);
                    t.rsplit('.').next().unwrap_or(t).to_string()
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

    fn modifiers(node: TsNode, src: &str) -> String {
        let mut mods = String::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let c = cursor.node();
                if c.kind() == "modifier" {
                    mods.push_str(&src[c.byte_range()]);
                    mods.push(' ');
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        mods
    }

    fn container(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<ns>");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), true);
        self.b.push(n.clone());
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn type_decl(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.b.child_text(node, "name").unwrap_or("<type>");
        let mods = Self::modifiers(node, self.b.source);
        let exported = mods.contains("public");
        let n = self
            .b
            .make_node(kind, name, node, Some(name.to_string()), exported);
        self.b.push(n.clone());

        // base_list: extends/implements
        if let Some(bases) = node.child_by_field_name("bases").or_else(|| {
            node.children(&mut node.walk())
                .find(|c| c.kind() == "base_list")
        }) {
            self.b.enter(n.clone());
            for base in bases.named_children(&mut bases.walk()) {
                let bt = self.b.text(base);
                self.b.record_ref(bt, EdgeKind::Implements);
            }
            self.b.exit();
        }

        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
    }

    fn method(&mut self, node: TsNode) {
        let name = self.b.child_text(node, "name").unwrap_or("<fn>");
        let mods = Self::modifiers(node, self.b.source);
        let exported = mods.contains("public");
        let n = self.b.make_node(
            NodeKind::Method,
            name,
            node,
            Some(name.to_string()),
            exported,
        );

        self.b.push(n.clone());
        let saved = std::mem::take(&mut self.b.var_types);
        hints::collect_csharp_var_types(self.b.source, node, &mut self.b.var_types);
        self.b.enter(n);
        self.recurse(node);
        self.b.exit();
        self.b.var_types = saved;
    }
}
