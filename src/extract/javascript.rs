use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Language, Node as TsNode, Parser};

use super::{hints, util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct JsExtractor;

impl super::Extractor for JsExtractor {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let lang = get_language(path);
        let mut parser = Parser::new();
        parser.set_language(&lang)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", path.display()))?;

        let file_path = path.to_string_lossy().to_string();
        let mut ctx = ExtractCtx {
            source,
            file_path: &file_path,
            lang_name: detect_lang_name(path),
            nodes: vec![],
            edges: vec![],
            unresolved: vec![],
            scope_stack: vec![],
            var_types: HashMap::new(),
        };

        // Create a file node
        let file_node = Node {
            id: Node::new_id(&file_path, &file_path),
            kind: NodeKind::File,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            qualified_name: file_path.clone(),
            file_path: file_path.clone(),
            language: ctx.lang_name.to_string(),
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
        ctx.nodes.push(file_node.clone());
        ctx.scope_stack.push(file_node);

        ctx.walk(tree.root_node());

        Ok(ExtractionResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
            unresolved: ctx.unresolved,
        })
    }
}

fn get_language(path: &Path) -> Language {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "ts" | "tsx" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    }
}

fn detect_lang_name(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        _ => "javascript",
    }
}

struct ExtractCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    lang_name: &'static str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    /// Current nesting: last element is innermost container
    scope_stack: Vec<Node>,
    var_types: hints::VarTypes,
}

impl<'a> ExtractCtx<'a> {
    fn node_text(&self, node: TsNode) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn child_text(&self, node: TsNode, field: &str) -> Option<&'a str> {
        node.child_by_field_name(field).map(|n| self.node_text(n))
    }

    fn line(&self, node: TsNode) -> (u32, u32) {
        (
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
        )
    }

    fn current_scope(&self) -> Option<&Node> {
        self.scope_stack.last()
    }

    fn qualified(&self, name: &str) -> String {
        match self.scope_stack.last() {
            Some(parent) if parent.kind != NodeKind::File => {
                format!("{}::{}", parent.qualified_name, name)
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

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "function_declaration" | "function" => {
                self.extract_function(node, false);
            }
            "arrow_function" => {
                // Arrow functions assigned to a variable are captured at the variable level
            }
            "lexical_declaration" | "variable_declaration" => {
                self.extract_variable_decl(node);
            }
            "class_declaration" | "class" => {
                self.extract_class(node);
                return; // class body handled inside extract_class
            }
            "interface_declaration" => {
                self.extract_interface(node);
                return;
            }
            "type_alias_declaration" => {
                self.extract_type_alias(node);
            }
            "enum_declaration" => {
                self.extract_enum(node);
                return;
            }
            "export_statement" => {
                self.extract_export(node);
                return;
            }
            "import_declaration" => {
                self.extract_import(node);
            }
            "call_expression" => {
                self.extract_call(node);
            }
            _ => {}
        }

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

    fn extract_function(&mut self, node: TsNode, is_method: bool) {
        let name = self
            .child_text(node, "name")
            .or_else(|| self.child_text(node, "property_identifier"))
            .unwrap_or("<anonymous>");

        let is_async = node
            .child(0)
            .map(|c| self.node_text(c) == "async")
            .unwrap_or(false);

        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let signature = self.build_function_signature(node, name);
        let docstring = self.extract_jsdoc(node);

        let func_node = Node {
            id: id.clone(),
            kind: if is_method {
                NodeKind::Method
            } else {
                NodeKind::Function
            },
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring,
            visibility: None,
            is_exported: false,
            is_async,
            is_static: false,
            is_abstract: false,
        };

        self.push_node(func_node.clone());

        // Walk body for calls
        let saved = std::mem::take(&mut self.var_types);
        hints::collect_js_var_types(self.source, node, &mut self.var_types);
        self.scope_stack.push(func_node);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_for_calls(body, &id);
        }
        self.scope_stack.pop();
        self.var_types = saved;
    }

    fn extract_class(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<anon>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let class_node = Node {
            id: id.clone(),
            kind: NodeKind::Class,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(format!("class {}", name)),
            docstring: self.extract_jsdoc(node),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };

        self.push_node(class_node.clone());
        self.scope_stack.push(class_node);

        // Extract superclass
        if let Some(heritage) = node.child_by_field_name("heritage") {
            let super_name = self
                .node_text(heritage)
                .trim_start_matches("extends ")
                .trim();
            if !super_name.is_empty() {
                self.unresolved.push(UnresolvedRef::new(
                    id.clone(),
                    super_name.to_string(),
                    EdgeKind::Extends,
                    self.file_path.to_string(),
                ));
            }
        }

        // Walk class body
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_class_body(body);
        }

        self.scope_stack.pop();
    }

    fn walk_class_body(&mut self, node: TsNode) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "method_definition" => self.extract_method(child),
                    "public_field_definition" | "field_definition" => self.extract_field(child),
                    _ => {}
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn extract_method(&mut self, node: TsNode) {
        let name = self
            .child_text(node, "name")
            .or_else(|| self.child_text(node, "property_identifier"))
            .unwrap_or("<anon>");

        let is_static = node
            .child(0)
            .map(|c| self.node_text(c) == "static")
            .unwrap_or(false);
        let is_async = node
            .children(&mut node.walk())
            .any(|c| self.node_text(c) == "async");

        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let signature = self.build_function_signature(node, name);

        let method_node = Node {
            id: id.clone(),
            kind: NodeKind::Method,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: self.extract_jsdoc(node),
            visibility: None,
            is_exported: false,
            is_async,
            is_static,
            is_abstract: false,
        };

        self.push_node(method_node.clone());

        let saved = std::mem::take(&mut self.var_types);
        hints::collect_js_var_types(self.source, node, &mut self.var_types);
        self.scope_stack.push(method_node);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_for_calls(body, &id);
        }
        self.scope_stack.pop();
        self.var_types = saved;
    }

    fn extract_field(&mut self, node: TsNode) {
        let name = self
            .child_text(node, "name")
            .or_else(|| self.child_text(node, "property_identifier"))
            .unwrap_or("<field>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let field_node = Node {
            id,
            kind: NodeKind::Field,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: None,
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(field_node);
    }

    fn extract_interface(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<iface>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let iface_node = Node {
            id: id.clone(),
            kind: NodeKind::Interface,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(format!("interface {}", name)),
            docstring: self.extract_jsdoc(node),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(iface_node.clone());

        // extends
        if let Some(heritage) = node.child_by_field_name("extends") {
            let mut cursor = heritage.walk();
            if cursor.goto_first_child() {
                loop {
                    let ext = cursor.node();
                    if ext.kind() == "type_identifier" {
                        self.unresolved.push(UnresolvedRef::new(
                            id.clone(),
                            self.node_text(ext).to_string(),
                            EdgeKind::Extends,
                            self.file_path.to_string(),
                        ));
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    fn extract_type_alias(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<type>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let type_node = Node {
            id,
            kind: NodeKind::TypeAlias,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(format!("type {}", name)),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(type_node);
    }

    fn extract_enum(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<enum>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let enum_node = Node {
            id: id.clone(),
            kind: NodeKind::Enum,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: self.lang_name.to_string(),
            start_line,
            end_line,
            signature: Some(format!("enum {}", name)),
            docstring: self.extract_jsdoc(node),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(enum_node.clone());
        self.scope_stack.push(enum_node);

        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "enum_assignment" || child.kind() == "property_identifier" {
                        let member_name = self.node_text(child).trim_end_matches(',').trim();
                        let (ms, me) = self.line(child);
                        let mq = self.qualified(member_name);
                        let mid = Node::new_id(self.file_path, &mq);
                        let member = Node {
                            id: mid,
                            kind: NodeKind::EnumMember,
                            name: member_name.to_string(),
                            qualified_name: mq,
                            file_path: self.file_path.to_string(),
                            language: self.lang_name.to_string(),
                            start_line: ms,
                            end_line: me,
                            signature: None,
                            docstring: None,
                            visibility: None,
                            is_exported: false,
                            is_async: false,
                            is_static: false,
                            is_abstract: false,
                        };
                        self.push_node(member);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_variable_decl(&mut self, node: TsNode) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "variable_declarator" {
                    let name = self.child_text(child, "name").unwrap_or("<var>");
                    // If value is arrow function or function expression, treat as function
                    let is_func = child
                        .child_by_field_name("value")
                        .map(|v| matches!(v.kind(), "arrow_function" | "function"))
                        .unwrap_or(false);

                    let (start_line, end_line) = self.line(child);
                    let qualified = self.qualified(name);
                    let id = Node::new_id(self.file_path, &qualified);

                    let (kind, sig) = if is_func {
                        (NodeKind::Function, format!("function {}", name))
                    } else {
                        (NodeKind::Variable, format!("const {}", name))
                    };

                    let var_node = Node {
                        id: id.clone(),
                        kind,
                        name: name.to_string(),
                        qualified_name: qualified,
                        file_path: self.file_path.to_string(),
                        language: self.lang_name.to_string(),
                        start_line,
                        end_line,
                        signature: Some(sig),
                        docstring: None,
                        visibility: None,
                        is_exported: false,
                        is_async: false,
                        is_static: false,
                        is_abstract: false,
                    };
                    self.push_node(var_node.clone());

                    if is_func {
                        if let Some(value) = child.child_by_field_name("value") {
                            self.scope_stack.push(var_node);
                            if let Some(body) = value.child_by_field_name("body") {
                                self.walk_for_calls(body, &id);
                            }
                            self.scope_stack.pop();
                        }
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn extract_export(&mut self, node: TsNode) {
        // Walk children to extract the actual declaration
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

    fn extract_import(&mut self, node: TsNode) {
        let source = node.child_by_field_name("source").map(|n| {
            self.node_text(n)
                .trim_matches(|c| c == '\'' || c == '"')
                .to_string()
        });

        if let Some(src) = source {
            let (start_line, _) = self.line(node);
            let qualified = self.qualified(&format!("import:{}", src));
            let id = Node::new_id(self.file_path, &qualified);

            let import_node = Node {
                id: id.clone(),
                kind: NodeKind::Import,
                name: src.clone(),
                qualified_name: qualified,
                file_path: self.file_path.to_string(),
                language: self.lang_name.to_string(),
                start_line,
                end_line: start_line,
                signature: Some(format!("import from \"{}\"", src)),
                docstring: None,
                visibility: None,
                is_exported: false,
                is_async: false,
                is_static: false,
                is_abstract: false,
            };
            self.push_node(import_node);

            self.unresolved.push(UnresolvedRef::new(
                id,
                src,
                EdgeKind::Imports,
                self.file_path.to_string(),
            ));
        }
    }

    fn extract_call(&mut self, node: TsNode) {
        if let Some(scope) = self.current_scope() {
            if matches!(
                scope.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::File
            ) {
                if let Some(func) = node.child_by_field_name("function") {
                    let (method, hint) =
                        hints::resolve_js_callee(self.source, func, &self.var_types);
                    let scope_id = scope.id.clone();
                    hints::push_call(
                        &mut self.unresolved,
                        &scope_id,
                        self.file_path,
                        &method,
                        hint,
                    );
                }
            }
        }
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        if node.kind() == "call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                let (method, hint) =
                    hints::resolve_js_callee(self.source, func, &self.var_types);
                hints::push_call(
                    &mut self.unresolved,
                    caller_id,
                    self.file_path,
                    &method,
                    hint,
                );
            }
        }
        // `new Foo(...)` — record an instantiation edge to the constructed class.
        if node.kind() == "new_expression" {
            if let Some(ctor) = node.child_by_field_name("constructor") {
                let name = self.node_text(ctor);
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

    fn build_function_signature(&self, node: TsNode, name: &str) -> String {
        let params = node
            .child_by_field_name("parameters")
            .map(|p| self.node_text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("return_type")
            .map(|r| format!(": {}", self.node_text(r)))
            .unwrap_or_default();
        format!("function {}{}{}", name, params, ret)
    }

    fn extract_jsdoc(&self, node: TsNode) -> Option<String> {
        // Look for a preceding comment sibling
        let parent = node.parent()?;
        let mut cursor = parent.walk();
        let mut prev_comment: Option<String> = None;
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.id() == node.id() {
                    return prev_comment;
                }
                if child.kind() == "comment" {
                    let text = self.node_text(child);
                    if text.starts_with("/**") {
                        prev_comment = Some(text.to_string());
                    } else {
                        prev_comment = None;
                    }
                } else if !matches!(child.kind(), "comment" | "\n") {
                    prev_comment = None;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }
}
