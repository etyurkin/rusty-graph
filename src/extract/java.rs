use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::{hints, util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct JavaExtractor;

impl super::Extractor for JavaExtractor {
    fn language(&self) -> &'static str {
        "java"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

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
            language: "java".to_string(),
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

        let mut ctx = JavaCtx {
            source,
            file_path: &file_path,
            nodes: vec![file_node.clone()],
            edges: vec![],
            unresolved: vec![],
            scope_stack: vec![file_node],
            var_types: HashMap::new(),
        };
        ctx.walk(tree.root_node());

        Ok(ExtractionResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
            unresolved: ctx.unresolved,
        })
    }
}

struct JavaCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    scope_stack: Vec<Node>,
    var_types: hints::VarTypes,
}

impl<'a> JavaCtx<'a> {
    fn text(&self, node: TsNode) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn child_text(&self, node: TsNode, field: &str) -> Option<&'a str> {
        node.child_by_field_name(field).map(|n| self.text(n))
    }

    fn line(&self, node: TsNode) -> (u32, u32) {
        (
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
        )
    }

    fn qualified(&self, name: &str) -> String {
        match self.scope_stack.last() {
            Some(p) if p.kind != NodeKind::File => format!("{}::{}", p.qualified_name, name),
            _ => format!("{}::{}", self.file_path, name),
        }
    }

    fn modifiers(node: TsNode, source: &'a str) -> &'a str {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "modifiers" {
                return &source[child.byte_range()];
            }
        }
        ""
    }

    fn push_node(&mut self, node: Node) {
        if let Some(parent) = self.scope_stack.last() {
            self.edges.push(util::contains_edge(parent, &node));
        }
        self.nodes.push(node);
    }

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "class_declaration" => self.extract_type(node, NodeKind::Class),
            "interface_declaration" => self.extract_type(node, NodeKind::Interface),
            "enum_declaration" => self.extract_type(node, NodeKind::Enum),
            "record_declaration" => self.extract_type(node, NodeKind::Struct),
            "method_declaration" | "constructor_declaration" => self.extract_method(node),
            "import_declaration" => self.extract_import(node),
            _ => self.walk_children(node),
        }
    }

    fn walk_children(&mut self, node: TsNode) {
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

    fn extract_type(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.child_text(node, "name").unwrap_or("<type>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let mods = Self::modifiers(node, self.source);
        let exported = mods.contains("public");

        let type_node = Node {
            id,
            kind: kind.clone(),
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "java".to_string(),
            start_line,
            end_line,
            signature: Some(format!("{} {}", kind.as_str(), name)),
            docstring: self.extract_javadoc(node),
            visibility: visibility_of(mods),
            is_exported: exported,
            is_async: false,
            is_static: mods.contains("static"),
            is_abstract: mods.contains("abstract") || kind == NodeKind::Interface,
        };
        self.push_node(type_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(type_node);
            self.walk_children(body);
            self.scope_stack.pop();
        }
    }

    fn extract_method(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<method>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let mods = Self::modifiers(node, self.source);
        let exported = mods.contains("public");

        let params = self.child_text(node, "parameters").unwrap_or("()");
        let ret = self
            .child_text(node, "type")
            .map(|t| format!("{} ", t))
            .unwrap_or_default();
        let in_type = matches!(
            self.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::Struct)
        );
        let kind = if in_type {
            NodeKind::Method
        } else {
            NodeKind::Function
        };

        let method_node = Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "java".to_string(),
            start_line,
            end_line,
            signature: Some(format!("{}{}{}", ret, name, params)),
            docstring: self.extract_javadoc(node),
            visibility: visibility_of(mods),
            is_exported: exported,
            is_async: false,
            is_static: mods.contains("static"),
            is_abstract: mods.contains("abstract"),
        };
        self.push_node(method_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            let saved = std::mem::take(&mut self.var_types);
            hints::collect_java_var_types(self.source, node, &mut self.var_types);
            self.scope_stack.push(method_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
            self.var_types = saved;
        }
    }

    fn extract_import(&mut self, node: TsNode) {
        let text = self.text(node).to_string();
        let (start_line, _) = self.line(node);
        let qualified = self.qualified(&format!("import:{}", &text[..text.len().min(64)]));
        let id = Node::new_id(self.file_path, &qualified);

        let import_node = Node {
            id,
            kind: NodeKind::Import,
            name: text.clone(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "java".to_string(),
            start_line,
            end_line: start_line,
            signature: Some(text),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(import_node);
    }

    fn extract_javadoc(&self, node: TsNode) -> Option<String> {
        let prev = node.prev_sibling()?;
        if prev.kind() == "block_comment" || prev.kind() == "comment" {
            let text = self.text(prev);
            if text.starts_with("/**") {
                return Some(text.to_string());
            }
        }
        None
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if node.kind() == "method_invocation" {
            if let Some((name, hint)) = hints::resolve_java_invocation(self.source, node, &self.var_types) {
                hints::push_call(
                    &mut self.unresolved,
                    caller_id,
                    self.file_path,
                    &name,
                    hint,
                );
            }
        }
        // `new Foo(...)` — record an instantiation of the constructed type.
        if node.kind() == "object_creation_expression" {
            if let Some(ty) = node.child_by_field_name("type") {
                let name = self.text(ty);
                let name = name.rsplit('.').next().unwrap_or(name);
                self.unresolved.push(UnresolvedRef::new(
                    caller_id.to_string(),
                    name.to_string(),
                    EdgeKind::Instantiates,
                    self.file_path.to_string(),
                ));
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

fn visibility_of(mods: &str) -> Option<String> {
    if mods.contains("public") {
        Some("public".to_string())
    } else if mods.contains("protected") {
        Some("protected".to_string())
    } else if mods.contains("private") {
        Some("private".to_string())
    } else {
        None
    }
}
