use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use super::{hints, util, ExtractionResult};
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};

pub struct RustExtractor;

impl super::Extractor for RustExtractor {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;

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
            language: "rust".to_string(),
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

        let mut ctx = RustCtx {
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

struct RustCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
    scope_stack: Vec<Node>,
    /// Variable → type-name map for the function body currently being walked,
    /// used to infer the receiver type of `var.method()` calls.
    var_types: hints::VarTypes,
}

impl<'a> RustCtx<'a> {
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

    fn is_pub(&self, node: TsNode) -> bool {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                if self.text(cursor.node()) == "pub" {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        false
    }

    fn is_async_fn(&self, node: TsNode) -> bool {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                if self.text(cursor.node()) == "async" {
                    return true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        false
    }

    fn extract_doc_comment(&self, node: TsNode) -> Option<String> {
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
                match child.kind() {
                    "line_comment" | "block_comment" => {
                        let t = self.text(child);
                        if t.starts_with("///") || t.starts_with("/**") {
                            docs.push(t);
                        } else {
                            docs.clear();
                        }
                    }
                    _ => docs.clear(),
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }

    fn walk(&mut self, node: TsNode) {
        match node.kind() {
            "function_item" => self.extract_fn(node, NodeKind::Function),
            "impl_item" => self.extract_impl(node),
            "struct_item" => self.extract_struct(node),
            "enum_item" => self.extract_enum(node),
            "trait_item" => self.extract_trait(node),
            "mod_item" => self.extract_mod(node),
            "use_declaration" => self.extract_use(node),
            "type_item" => self.extract_type_alias(node),
            "const_item" => self.extract_const(node),
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

    fn extract_fn(&mut self, node: TsNode, kind: NodeKind) {
        let name = self.child_text(node, "name").unwrap_or("<fn>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let is_pub = self.is_pub(node);
        let is_async = self.is_async_fn(node);

        let params = node
            .child_by_field_name("parameters")
            .map(|p| self.text(p))
            .unwrap_or("()");
        let ret = node
            .child_by_field_name("return_type")
            .map(|r| format!(" -> {}", self.text(r)))
            .unwrap_or_default();
        let signature = format!("fn {}{}{}", name, params, ret);

        let fn_node = Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(signature),
            docstring: self.extract_doc_comment(node),
            visibility: if is_pub {
                Some("pub".to_string())
            } else {
                None
            },
            is_exported: is_pub,
            is_async,
            is_static: false,
            is_abstract: false,
        };

        self.push_node(fn_node.clone());

        // Walk body for calls, with a fresh variable→type environment so
        // `var.method()` calls can be resolved by receiver type.
        if let Some(body) = node.child_by_field_name("body") {
            let saved = std::mem::take(&mut self.var_types);
            self.build_var_types(node);
            self.scope_stack.push(fn_node);
            self.walk_for_calls(body, &id);
            self.scope_stack.pop();
            self.var_types = saved;
        }
    }

    /// Populate `self.var_types` from a function's parameters and `let`
    /// bindings so receiver types can be inferred for method calls.
    fn build_var_types(&mut self, fn_node: TsNode) {
        self.var_types.clear();
        if let Some(params) = fn_node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for child in params.children(&mut cursor) {
                if child.kind() != "parameter" {
                    continue;
                }
                let pat = child.child_by_field_name("pattern");
                let ty = child.child_by_field_name("type");
                if let (Some(pat), Some(ty)) = (pat, ty) {
                    if pat.kind() == "identifier" {
                        if let Some(t) = hints::clean_type_name(self.text(ty)) {
                            self.var_types.insert(self.text(pat).to_string(), t);
                        }
                    }
                }
            }
        }
        if let Some(body) = fn_node.child_by_field_name("body") {
            self.collect_let_types(body);
        }
    }

    fn collect_let_types(&mut self, node: TsNode) {
        if node.kind() == "let_declaration" {
            if let Some(pat) = node.child_by_field_name("pattern") {
                if pat.kind() == "identifier" {
                    let name = self.text(pat).to_string();
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| hints::clean_type_name(self.text(t)))
                        .or_else(|| {
                            node.child_by_field_name("value")
                                .and_then(|v| self.infer_type_from_value(v))
                        });
                    if let Some(ty) = ty {
                        self.var_types.insert(name, ty);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.collect_let_types(child);
        }
    }

    /// Best-effort type of an initializer expression: `Type::new(..)`,
    /// `Type { .. }`, or `&expr` (recursing through the reference).
    fn infer_type_from_value(&self, value: TsNode) -> Option<String> {
        match value.kind() {
            "call_expression" => {
                let func = value.child_by_field_name("function")?;
                self.resolve_callee(func).1.map(hints::strip_authority)
            }
            "struct_expression" => value
                .child_by_field_name("name")
                .and_then(|n| hints::clean_type_name(self.text(n))),
            "reference_expression" => value
                .child_by_field_name("value")
                .and_then(|v| self.infer_type_from_value(v)),
            _ => None,
        }
    }

    fn extract_impl(&mut self, node: TsNode) {
        // impl Foo or impl Trait for Foo
        let type_name = node
            .child_by_field_name("type")
            .map(|n| self.text(n))
            .unwrap_or("<impl>");
        let trait_name = node.child_by_field_name("trait").map(|n| self.text(n));

        let (start_line, end_line) = self.line(node);
        let impl_label = if let Some(ref tr) = trait_name {
            format!("impl {} for {}", tr, type_name)
        } else {
            format!("impl {}", type_name)
        };
        let qualified = self.qualified(&impl_label);
        let id = Node::new_id(self.file_path, &qualified);

        // If impl Trait for Type, record implements edge
        if let Some(ref tr) = trait_name {
            // find the struct/enum node for type_name and add implements edge
            self.unresolved.push(UnresolvedRef::new(
                Node::new_id(self.file_path, &self.qualified(type_name)),
                tr.to_string(),
                EdgeKind::Implements,
                self.file_path.to_string(),
            ));
        }

        // Create a namespace node for the impl block
        let impl_node = Node {
            id: id.clone(),
            kind: NodeKind::Namespace,
            name: impl_label.clone(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(impl_label),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(impl_node.clone());
        self.scope_stack.push(impl_node);

        // Walk methods
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "function_item" {
                        // Check if it has self param → method
                        let is_method = child
                            .child_by_field_name("parameters")
                            .map(|p| {
                                let text = self.text(p);
                                text.contains("self")
                            })
                            .unwrap_or(false);
                        self.extract_fn(
                            child,
                            if is_method {
                                NodeKind::Method
                            } else {
                                NodeKind::Function
                            },
                        );
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_struct(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<struct>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let is_pub = self.is_pub(node);

        let struct_node = Node {
            id: id.clone(),
            kind: NodeKind::Struct,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(format!("struct {}", name)),
            docstring: self.extract_doc_comment(node),
            visibility: if is_pub {
                Some("pub".to_string())
            } else {
                None
            },
            is_exported: is_pub,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(struct_node.clone());
        self.scope_stack.push(struct_node);

        // Fields
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "field_declaration" {
                        let fname = self.child_text(child, "name").unwrap_or("<field>");
                        let (fs, fe) = self.line(child);
                        let fq = self.qualified(fname);
                        let fid = Node::new_id(self.file_path, &fq);
                        let field = Node {
                            id: fid,
                            kind: NodeKind::Field,
                            name: fname.to_string(),
                            qualified_name: fq,
                            file_path: self.file_path.to_string(),
                            language: "rust".to_string(),
                            start_line: fs,
                            end_line: fe,
                            signature: Some(self.text(child).to_string()),
                            docstring: None,
                            visibility: None,
                            is_exported: false,
                            is_async: false,
                            is_static: false,
                            is_abstract: false,
                        };
                        self.push_node(field);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_enum(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<enum>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let is_pub = self.is_pub(node);

        let enum_node = Node {
            id: id.clone(),
            kind: NodeKind::Enum,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(format!("enum {}", name)),
            docstring: self.extract_doc_comment(node),
            visibility: if is_pub {
                Some("pub".to_string())
            } else {
                None
            },
            is_exported: is_pub,
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
                    if child.kind() == "enum_variant" {
                        let vname = self.child_text(child, "name").unwrap_or("<variant>");
                        let (vs, ve) = self.line(child);
                        let vq = self.qualified(vname);
                        let vid = Node::new_id(self.file_path, &vq);
                        let variant = Node {
                            id: vid,
                            kind: NodeKind::EnumMember,
                            name: vname.to_string(),
                            qualified_name: vq,
                            file_path: self.file_path.to_string(),
                            language: "rust".to_string(),
                            start_line: vs,
                            end_line: ve,
                            signature: Some(self.text(child).to_string()),
                            docstring: self.extract_doc_comment(child),
                            visibility: None,
                            is_exported: false,
                            is_async: false,
                            is_static: false,
                            is_abstract: false,
                        };
                        self.push_node(variant);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_trait(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<trait>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let is_pub = self.is_pub(node);

        let trait_node = Node {
            id: id.clone(),
            kind: NodeKind::Trait,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(format!("trait {}", name)),
            docstring: self.extract_doc_comment(node),
            visibility: if is_pub {
                Some("pub".to_string())
            } else {
                None
            },
            is_exported: is_pub,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(trait_node.clone());
        self.scope_stack.push(trait_node);

        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() == "function_signature_item" || child.kind() == "function_item"
                    {
                        let fname = self.child_text(child, "name").unwrap_or("<method>");
                        let (fs, fe) = self.line(child);
                        let fq = self.qualified(fname);
                        let fid = Node::new_id(self.file_path, &fq);
                        let params = child
                            .child_by_field_name("parameters")
                            .map(|p| self.text(p))
                            .unwrap_or("()");
                        let ret = child
                            .child_by_field_name("return_type")
                            .map(|r| format!(" -> {}", self.text(r)))
                            .unwrap_or_default();
                        let method = Node {
                            id: fid,
                            kind: NodeKind::Method,
                            name: fname.to_string(),
                            qualified_name: fq,
                            file_path: self.file_path.to_string(),
                            language: "rust".to_string(),
                            start_line: fs,
                            end_line: fe,
                            signature: Some(format!("fn {}{}{}", fname, params, ret)),
                            docstring: self.extract_doc_comment(child),
                            visibility: None,
                            is_exported: false,
                            is_async: false,
                            is_static: false,
                            is_abstract: child.kind() == "function_signature_item",
                        };
                        self.push_node(method);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        self.scope_stack.pop();
    }

    fn extract_mod(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<mod>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);

        let mod_node = Node {
            id,
            kind: NodeKind::Module,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(format!("mod {}", name)),
            docstring: self.extract_doc_comment(node),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(mod_node.clone());

        if let Some(body) = node.child_by_field_name("body") {
            self.scope_stack.push(mod_node);
            self.walk(body);
            self.scope_stack.pop();
        }
    }

    fn extract_use(&mut self, node: TsNode) {
        let text = self
            .text(node)
            .trim_start_matches("use ")
            .trim_end_matches(';')
            .to_string();
        let (start_line, _) = self.line(node);
        let qualified = self.qualified(&format!("use:{}", &text[..text.len().min(64)]));
        let id = Node::new_id(self.file_path, &qualified);

        let use_node = Node {
            id: id.clone(),
            kind: NodeKind::Import,
            name: text.clone(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line: start_line,
            signature: Some(format!("use {}", text)),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(use_node);
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
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(self.text(node).to_string()),
            docstring: self.extract_doc_comment(node),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        self.push_node(type_node);
    }

    fn extract_const(&mut self, node: TsNode) {
        let name = self.child_text(node, "name").unwrap_or("<const>");
        let (start_line, end_line) = self.line(node);
        let qualified = self.qualified(name);
        let id = Node::new_id(self.file_path, &qualified);
        let is_pub = self.is_pub(node);

        let const_node = Node {
            id,
            kind: NodeKind::Constant,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: self.file_path.to_string(),
            language: "rust".to_string(),
            start_line,
            end_line,
            signature: Some(self.text(node).trim_end_matches(';').to_string()),
            docstring: self.extract_doc_comment(node),
            visibility: if is_pub {
                Some("pub".to_string())
            } else {
                None
            },
            is_exported: is_pub,
            is_async: false,
            is_static: true,
            is_abstract: false,
        };
        self.push_node(const_node);
    }

    fn push_call(&mut self, caller_id: &str, raw_method: &str, hint: Option<String>) {
        hints::push_call(
            &mut self.unresolved,
            caller_id,
            self.file_path,
            raw_method,
            hint,
        );
    }

    fn record_call(&mut self, caller_id: &str, name: &str) {
        self.push_call(caller_id, name, None);
    }

    /// Reduce a callee expression node to `(bare_method, receiver_type_hint)`.
    /// The hint is `!Type` for an explicit `Type::method()` qualifier
    /// (authoritative), `Self` for `self`/`Self`, or an inferred variable type
    /// (advisory). `None` falls back to name-only resolution.
    fn resolve_callee(&self, func: TsNode) -> (String, Option<String>) {
        match func.kind() {
            "field_expression" => {
                let method = func
                    .child_by_field_name("field")
                    .map(|n| self.text(n))
                    .unwrap_or("")
                    .to_string();
                let hint = func
                    .child_by_field_name("value")
                    .and_then(|v| self.receiver_type(v));
                (method, hint)
            }
            "scoped_identifier" => {
                let full = self.text(func);
                let mut segs: Vec<&str> = full.split("::").collect();
                let method = segs.pop().unwrap_or("").to_string();
                let hint = segs.last().and_then(|q| hints::qualifier_hint(q));
                (method, hint)
            }
            "generic_function" => func
                .child_by_field_name("function")
                .map(|inner| self.resolve_callee(inner))
                .unwrap_or_else(|| (self.text(func).to_string(), None)),
            _ => (self.text(func).to_string(), None),
        }
    }

    /// Infer the type of a call receiver expression: `self`/`Self`, or a known
    /// local variable. Complex receivers (chains, field access) yield `None`.
    fn receiver_type(&self, node: TsNode) -> Option<String> {
        match node.kind() {
            "self" => Some("Self".to_string()),
            "identifier" => {
                let t = self.text(node);
                if t == "self" {
                    Some("Self".to_string())
                } else {
                    self.var_types.get(t).cloned()
                }
            }
            _ => None,
        }
    }

    fn walk_for_calls(&mut self, node: TsNode, caller_id: &str) {
        match node.kind() {
            "call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    let (method, hint) = self.resolve_callee(func);
                    self.push_call(caller_id, &method, hint);
                }
            }
            // Macro arguments (`format!`, `json!`, `println!`, …) are an
            // unparsed token tree, so calls inside them never become
            // `call_expression` nodes. Scan the token tree heuristically.
            "macro_invocation" => {
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        if cursor.node().kind() == "token_tree" {
                            self.walk_macro_calls(cursor.node(), caller_id);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                return; // token tree holds no real expression nodes to recurse
            }
            _ => {}
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

    /// Heuristically find call-like patterns inside a macro's token tree: an
    /// `identifier` immediately followed by a parenthesized token tree (the
    /// argument list). Recurses into nested token trees.
    fn walk_macro_calls(&mut self, token_tree: TsNode, caller_id: &str) {
        let mut cursor = token_tree.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            if child.kind() == "identifier" {
                if let Some(sib) = child.next_sibling() {
                    if sib.kind() == "token_tree" && self.text(sib).starts_with('(') {
                        let name = self.text(child).to_string();
                        self.record_call(caller_id, &name);
                    }
                }
            }
            if child.kind() == "token_tree" {
                self.walk_macro_calls(child, caller_id);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Extractor;

    fn calls(src: &str) -> Vec<String> {
        let result = RustExtractor
            .extract(Path::new("a.rs"), src)
            .expect("extract");
        result
            .unresolved
            .iter()
            .filter(|r| r.kind == EdgeKind::Calls)
            .map(|r| r.target_name.clone())
            .collect()
    }

    fn call_refs(src: &str) -> Vec<(String, Option<String>)> {
        let result = RustExtractor
            .extract(Path::new("a.rs"), src)
            .expect("extract");
        result
            .unresolved
            .iter()
            .filter(|r| r.kind == EdgeKind::Calls)
            .map(|r| (r.target_name.clone(), r.receiver_hint.clone()))
            .collect()
    }

    #[test]
    fn captures_plain_call() {
        let names = calls("fn helper() {}\nfn run() { helper() }\n");
        assert!(names.contains(&"helper".to_string()));
    }

    #[test]
    fn captures_call_inside_format_macro() {
        // Regression: calls buried in macro args were invisible to the resolver.
        let names = calls(
            "fn esc(s: &str) -> String { s.to_string() }\n\
             fn run() { let _ = format!(\"{}\", esc(\"x\")); }\n",
        );
        assert!(
            names.contains(&"esc".to_string()),
            "call inside format! must be captured: {names:?}"
        );
    }

    #[test]
    fn captures_method_call_via_receiver() {
        // `self.foo()` / `obj.foo()` must resolve to the bare name `foo`.
        let names = calls(
            "struct S;\n\
             impl S {\n  fn foo(&self) {}\n  fn run(&self) { self.foo(); }\n}\n",
        );
        assert!(
            names.contains(&"foo".to_string()),
            "receiver method call should resolve to short name: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains('.')),
            "no receiver-qualified names should leak through: {names:?}"
        );
    }

    #[test]
    fn captures_nested_call_inside_macro() {
        let names = calls(
            "fn inner() -> i32 { 1 }\n\
             fn outer(x: i32) -> i32 { x }\n\
             fn run() { println!(\"{}\", outer(inner())); }\n",
        );
        assert!(names.contains(&"outer".to_string()), "{names:?}");
        assert!(names.contains(&"inner".to_string()), "{names:?}");
    }

    #[test]
    fn qualified_assoc_call_carries_authoritative_hint() {
        let refs = call_refs(
            "struct Connection;\n\
             impl Connection { fn open() {} }\n\
             fn run() { Connection::open(); }\n",
        );
        assert!(
            refs.iter()
                .any(|(name, hint)| name == "open" && hint.as_deref() == Some("!Connection")),
            "Type::method() should carry an authoritative receiver hint: {refs:?}"
        );
    }

    #[test]
    fn self_method_carries_self_hint() {
        let refs = call_refs(
            "struct S;\n\
             impl S { fn foo(&self) {} fn run(&self) { self.foo(); } }\n",
        );
        assert!(
            refs.iter()
                .any(|(name, hint)| name == "foo" && hint.as_deref() == Some("Self")),
            "self.method() should carry a Self hint: {refs:?}"
        );
    }

    #[test]
    fn var_method_carries_inferred_hint() {
        let refs = call_refs(
            "struct Db;\n\
             impl Db { fn new() -> Self { Db } fn open(&self) {} }\n\
             fn run() { let db = Db::new(); db.open(); }\n",
        );
        assert!(
            refs.iter()
                .any(|(name, hint)| name == "open" && hint.as_deref() == Some("Db")),
            "inferred receiver type should narrow method resolution: {refs:?}"
        );
    }

    #[test]
    fn clean_type_strips_references_and_generics() {
        assert_eq!(
            hints::clean_type_name("&mut Connection"),
            Some("Connection".to_string())
        );
        assert_eq!(
            hints::clean_type_name("std::path::PathBuf"),
            Some("PathBuf".to_string())
        );
        assert_eq!(hints::clean_type_name("i32"), None);
    }

    #[test]
    fn qualifier_hint_marks_concrete_types_authoritative() {
        assert_eq!(
            hints::qualifier_hint("Connection"),
            Some("!Connection".to_string())
        );
        assert_eq!(hints::qualifier_hint("Self"), Some("Self".to_string()));
        assert_eq!(hints::qualifier_hint("T"), None);
    }
}
