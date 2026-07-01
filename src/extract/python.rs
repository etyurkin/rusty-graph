use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::{hints, util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct PythonExtractor;

impl super::Extractor for PythonExtractor {
    fn language(&self) -> &'static str {
        "python"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;

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
            language: "python".to_string(),
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

        let mut ctx = PyCtx {
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

struct PyCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    scope_stack: Vec<Node>,
    var_types: hints::VarTypes,
}

impl<'a> PyCtx<'a> {
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

    fn extract_docstring(&self, body: TsNode) -> Option<String> {
        // First statement in body may be a string literal (docstring)
        let mut cursor = body.walk();
        if cursor.goto_first_child() {
            let child = cursor.node();
            if child.kind() == "expression_statement" {
                if let Some(expr) = child.child(0) {
                    if matches!(expr.kind(), "string" | "concatenated_string") {
                        return Some(
                            self.text(expr)
                                .trim_matches(|c| c == '"' || c == '\'')
                                .to_string(),
                        );
                    }
                }
            }
        }
        None
    }

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "function_definition" => self.extract_function(node, false),
            "class_definition" => self.extract_class(node),
            "import_statement" | "import_from_statement" => self.extract_import(node),
            "call" => self.extract_call(node),
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

    fn extract_function(&mut self, node: TsNode, is_method: bool) {
        let name = self.child_text(node, "name").unwrap_or("<fn>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let is_async = node
            .child(0)
            .map(|c| self.text(c) == "async")
            .unwrap_or(false);

        let params = node
            .child_by_field_name("parameters")
            .map(|p| self.text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("return_type")
            .map(|r| format!(" -> {}", self.text(r)))
            .unwrap_or_default();
        let signature = format!("def {}{}{}", name, params, ret);

        let docstring = node
            .child_by_field_name("body")
            .and_then(|b| self.extract_docstring(b));

        let fn_node = Node {
            id: id.clone(),
            kind: if is_method {
                NodeKind::Method
            } else {
                NodeKind::Function
            },
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "python".to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring,
            visibility: if name.starts_with('_') {
                Some("private".to_string())
            } else {
                None
            },
            is_exported: !name.starts_with('_'),
            is_async,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(fn_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            let saved = std::mem::take(&mut self.var_types);
            hints::collect_python_var_types(self.source, node, &mut self.var_types);
            self.scope_stack.push(fn_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
            self.var_types = saved;
        }
    }

    fn extract_class(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<class>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        // Extract base classes
        let bases: Vec<String> = node
            .child_by_field_name("superclasses")
            .map(|supers| {
                let text = self.text(supers);
                text.trim_matches(|c| c == '(' || c == ')')
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let docstring = node
            .child_by_field_name("body")
            .and_then(|b| self.extract_docstring(b));

        let class_node = Node {
            id: id.clone(),
            kind: NodeKind::Class,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "python".to_string(),
            start_line,
            end_line,
            signature: Some(format!("class {}", name)),
            docstring,
            visibility: None,
            is_exported: !name.starts_with('_'),
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(class_node.clone());

        for base in bases {
            self.unresolved.push(UnresolvedRef::new(
                id.clone(),
                base,
                EdgeKind::Extends,
                self.file_path.to_string(),
            ));
        }

        self.scope_stack.push(class_node);

        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "function_definition" {
                        self.extract_function(child, true);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_import(&mut self, node: TsNode) {
        let text = self.text(node).to_string();
        let (start_line, _) = self.line(node);
        let qualified = self.qualified(&format!("import:{}", &text[..text.len().min(64)]));
        let id = Node::new_id(self.file_path, &qualified);

        let import_node = Node {
            id: id.clone(),
            kind: NodeKind::Import,
            name: text.clone(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "python".to_string(),
            start_line,
            end_line: start_line,
            signature: Some(text.clone()),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(import_node);

        // Record unresolved import
        let module = if node.kind() == "import_from_statement" {
            node.child_by_field_name("module_name")
                .map(|m| self.text(m).to_string())
        } else {
            node.child_by_field_name("name")
                .map(|m| self.text(m).to_string())
        };
        if let Some(module_name) = module {
            self.unresolved.push(UnresolvedRef::new(
                id,
                module_name,
                EdgeKind::Imports,
                self.file_path.to_string(),
            ));
        }
    }

    fn extract_call(&mut self, node: TsNode) {
        if let Some(scope) = self.scope_stack.last() {
            if let Some(func) = node.child_by_field_name("function") {
                let (method, hint) =
                    hints::resolve_python_callee(self.source, func, &self.var_types);
                hints::push_call(
                    &mut self.unresolved,
                    &scope.id,
                    self.file_path,
                    &method,
                    hint,
                );
            }
        }
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if node.kind() == "call" {
            if let Some(func) = node.child_by_field_name("function") {
                let (method, hint) =
                    hints::resolve_python_callee(self.source, func, &self.var_types);
                hints::push_call(
                    &mut self.unresolved,
                    caller_id,
                    self.file_path,
                    &method,
                    hint,
                );
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
