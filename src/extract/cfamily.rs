use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::{util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

/// Extractor for C and C++. The two grammars share node kinds for the
/// constructs we care about; `cpp` toggles the few C++-only forms.
pub struct CFamilyExtractor {
    pub cpp: bool,
}

impl super::Extractor for CFamilyExtractor {
    fn language(&self) -> &'static str {
        if self.cpp {
            "cpp"
        } else {
            "c"
        }
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        let lang = if self.cpp {
            tree_sitter_cpp::LANGUAGE.into()
        } else {
            tree_sitter_c::LANGUAGE.into()
        };
        parser.set_language(&lang)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let lang_id = self.language();
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
            language: lang_id.to_string(),
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

        let mut ctx = CCtx {
            source,
            file_path: &file_path,
            lang: lang_id,
            nodes: vec![file_node.clone()],
            edges: vec![],
            unresolved: vec![],
            scope_stack: vec![file_node],
        };
        ctx.walk(tree.root_node());

        Ok(ExtractionResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
            unresolved: ctx.unresolved,
        })
    }
}

struct CCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    lang: &'static str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    scope_stack: Vec<Node>,
}

impl<'a> CCtx<'a> {
    fn text(&self, node: TsNode) -> &'a str {
        &self.source[node.byte_range()]
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

    fn push_node(&mut self, node: Node) {
        if let Some(parent) = self.scope_stack.last() {
            self.edges.push(util::contains_edge(parent, &node));
        }
        self.nodes.push(node);
    }

    fn new_node(&self, kind: NodeKind, name: &str, node: TsNode, signature: String) -> Node {
        let qualified = self.qualified(name);
        let (start_line, end_line) = self.line(node);
        Node {
            id: Node::new_id(self.file_path, &qualified),
            kind,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang.to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
        }
    }

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "function_definition" => self.extract_function(node),
            "struct_specifier" => self.extract_record(node, NodeKind::Struct),
            "union_specifier" => self.extract_record(node, NodeKind::Struct),
            "class_specifier" => self.extract_record(node, NodeKind::Class),
            "enum_specifier" => self.extract_enum(node),
            "namespace_definition" if self.lang == "cpp" => self.extract_namespace(node),
            "type_definition" => self.extract_typedef(node),
            "preproc_include" => self.extract_include(node),
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

    fn extract_function(&mut self, node: TsNode) {
        let Some(decl) = node.child_by_field_name("declarator") else {
            return;
        };
        let Some(fn_decl) = find_function_declarator(decl) else {
            return;
        };
        let Some(name_node) = fn_decl.child_by_field_name("declarator") else {
            return;
        };
        let qualified = name_node.kind() == "qualified_identifier";
        let name = declarator_name(self.source, name_node).unwrap_or("<fn>");

        let params = fn_decl
            .child_by_field_name("parameters")
            .map(|p| self.text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("type")
            .map(|t| format!("{} ", self.text(t)))
            .unwrap_or_default();
        let signature = format!("{}{}{}", ret, name, params);

        let in_record = matches!(
            self.scope_stack.last().map(|s| &s.kind),
            Some(NodeKind::Class | NodeKind::Struct)
        );
        let kind = if in_record || qualified {
            NodeKind::Method
        } else {
            NodeKind::Function
        };

        let fn_node = self.new_node(kind, name, node, signature);
        let id = fn_node.id.clone();
        self.push_node(fn_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(fn_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
        }
    }

    fn extract_record(&mut self, node: TsNode, kind: NodeKind) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node);
            return;
        };
        let name = self.text(name_node);
        let signature = format!("{} {}", kind.as_str(), name);
        let record = self.new_node(kind, name, node, signature);
        self.push_node(record.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(record);
            self.walk_children(body);
            self.scope_stack.pop();
        }
    }

    fn extract_enum(&mut self, node: TsNode) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node);
        let signature = format!("enum {}", name);
        let enum_node = self.new_node(NodeKind::Enum, name, node, signature);
        self.push_node(enum_node);
    }

    fn extract_namespace(&mut self, node: TsNode) {
        let name = node
            .child_by_field_name("name")
            .map(|n| self.text(n))
            .unwrap_or("<anonymous>");
        let signature = format!("namespace {}", name);
        let ns = self.new_node(NodeKind::Namespace, name, node, signature);
        self.push_node(ns.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(ns);
            self.walk_children(body);
            self.scope_stack.pop();
        }
    }

    fn extract_typedef(&mut self, node: TsNode) {
        let Some(decl) = node.child_by_field_name("declarator") else {
            return;
        };
        let Some(name) = declarator_name(self.source, decl) else {
            return;
        };
        let signature = self.text(node).lines().next().unwrap_or("").to_string();
        let alias = self.new_node(NodeKind::TypeAlias, name, node, signature);
        self.push_node(alias);
    }

    fn extract_include(&mut self, node: TsNode) {
        let path_text = node
            .child_by_field_name("path")
            .map(|p| self.text(p))
            .unwrap_or("");
        let (start_line, _) = self.line(node);
        let qualified = self.qualified(&format!("include:{}", path_text));
        let id = Node::new_id(self.file_path, &qualified);

        let include = Node {
            id,
            kind: NodeKind::Import,
            name: path_text.trim_matches(['<', '>', '"']).to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang.to_string(),
            start_line,
            end_line: start_line,
            signature: Some(self.text(node).trim().to_string()),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(include);
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if node.kind() == "call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                if let Some(name) = call_target(self.source, func) {
                    self.unresolved.push(UnresolvedRef::new(
                        caller_id.to_string(),
                        name.to_string(),
                        EdgeKind::Calls,
                        self.file_path.to_string(),
                    ));
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

/// Descend the declarator chain (pointers, references, parens, arrays) to the
/// innermost `function_declarator`.
fn find_function_declarator(node: TsNode) -> Option<TsNode> {
    if node.kind() == "function_declarator" {
        return Some(node);
    }
    let child = node.child_by_field_name("declarator")?;
    find_function_declarator(child)
}

/// The bare name from a declarator, following pointer/reference wrappers and
/// taking the final segment of a `Foo::bar` qualified identifier.
fn declarator_name<'a>(source: &'a str, node: TsNode) -> Option<&'a str> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
        | "operator_name" | "primitive_type" => Some(&source[node.byte_range()]),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| declarator_name(source, n)),
        _ => node
            .child_by_field_name("declarator")
            .and_then(|n| declarator_name(source, n)),
    }
}

/// The callee name from a `call_expression`'s function node, taking the final
/// segment of member or qualified access (`obj.f()`, `ns::f()`).
fn call_target<'a>(source: &'a str, node: TsNode) -> Option<&'a str> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(&source[node.byte_range()]),
        "field_expression" => node
            .child_by_field_name("field")
            .and_then(|n| call_target(source, n)),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| call_target(source, n)),
        _ => None,
    }
}
