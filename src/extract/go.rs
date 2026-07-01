use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::{util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct GoExtractor;

impl super::Extractor for GoExtractor {
    fn language(&self) -> &'static str {
        "go"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into())?;

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
            language: "go".to_string(),
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

        let mut ctx = GoCtx {
            source,
            file_path: &file_path,
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

struct GoCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    scope_stack: Vec<Node>,
}

impl<'a> GoCtx<'a> {
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
            Some(p) if p.kind != NodeKind::File => {
                format!("{}::{}", p.qualified_name, name)
            }
            _ => format!("{}::{}", self.file_path, name),
        }
    }

    fn push_node(&mut self, node: Node) {
        if let Some(parent) = self.scope_stack.last() {
            self.edges.push(util::contains_edge(parent, &node));
        }
        self.nodes.push(node);
    }

    fn is_exported(name: &str) -> bool {
        name.chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
    }

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "function_declaration" => self.extract_function(node),
            "method_declaration" => self.extract_method(node),
            "type_declaration" => self.extract_type_decl(node),
            "import_declaration" => self.extract_import(node),
            _ => {
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
        }
    }

    fn extract_function(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<fn>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let exported = Self::is_exported(name);

        let params = node
            .child_by_field_name("parameters")
            .map(|p| self.text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("result")
            .map(|r| format!(" {}", self.text(r)))
            .unwrap_or_default();
        let signature = format!("func {}{}{}", name, params, ret);

        let fn_node = Node {
            id: id.clone(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "go".to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: self.extract_comment(node),
            visibility: if exported {
                Some("public".to_string())
            } else {
                None
            },
            is_exported: exported,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(fn_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(fn_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
        }
    }

    fn extract_method(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<method>");
        let receiver = node
            .child_by_field_name("receiver")
            .map(|r| {
                self.text(r)
                    .trim_matches(|c| c == '(' || c == ')')
                    .to_string()
            })
            .unwrap_or_default();
        let (start_line, end_line) = self.line(node);
        let qualified = format!("{}::{}::{}", self.file_path, receiver.trim(), name);
        let id = Node::new_id(self.file_path, &qualified);
        let exported = Self::is_exported(name);

        let params = node
            .child_by_field_name("parameters")
            .map(|p| self.text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("result")
            .map(|r| format!(" {}", self.text(r)))
            .unwrap_or_default();
        let signature = format!("func ({}) {}{}{}", receiver, name, params, ret);

        let method_node = Node {
            id: id.clone(),
            kind: NodeKind::Method,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "go".to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: self.extract_comment(node),
            visibility: if exported {
                Some("public".to_string())
            } else {
                None
            },
            is_exported: exported,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(method_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(method_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
        }
    }

    fn extract_type_decl(&mut self, node: TsNode) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "type_spec" {
                    let name = self.child_text(child, "name").unwrap_or("<type>");
                    let (start_line, end_line) = self.line(child);
                    let qualified = self.qualified(name);
                    let id = Node::new_id(self.file_path, &qualified);
                    let exported = Self::is_exported(name);

                    let type_val = child.child_by_field_name("type");
                    let kind = type_val
                        .map(|t| match t.kind() {
                            "struct_type" => NodeKind::Struct,
                            "interface_type" => NodeKind::Interface,
                            _ => NodeKind::TypeAlias,
                        })
                        .unwrap_or(NodeKind::TypeAlias);

                    let type_node = Node {
                        id,
                        kind,
                        name: name.to_string(),
                        qualified_name: qualified,
                        file_path: self.file_path.to_string(),
                        language: "go".to_string(),
                        start_line,
                        end_line,
                        signature: Some(format!("type {}", name)),
                        docstring: self.extract_comment(node),
                        visibility: if exported {
                            Some("public".to_string())
                        } else {
                            None
                        },
                        is_exported: exported,
                        is_async: false,
                        is_static: false,
                        is_abstract: false,
                    };
                    self.push_node(type_node);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
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
            language: "go".to_string(),
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

    fn extract_comment(&self, node: TsNode) -> Option<String> {
        let parent = node.parent()?;
        let mut cursor = parent.walk();
        let mut docs: Vec<&str> = vec![];
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.id() == node.id() {
                    if docs.is_empty() {
                        return None;
                    }
                    return Some(docs.join("\n"));
                }
                if child.kind() == "comment" {
                    docs.push(self.text(child));
                } else {
                    docs.clear();
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if node.kind() == "call_expression" {
            let callee = node
                .child_by_field_name("function")
                .map(|n| self.text(n).to_string());
            if let Some(name) = callee {
                let short = name.split('.').next_back().unwrap_or(&name).to_string();
                self.unresolved.push(UnresolvedRef::new(
                    caller_id.to_string(),
                    short,
                    EdgeKind::Calls,
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
