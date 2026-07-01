//! Shared helpers for type-directed call extraction across languages.

use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::types::{EdgeKind, UnresolvedRef};

pub type VarTypes = HashMap<String, String>;

pub fn node_text<'a>(source: &'a str, node: TsNode) -> &'a str {
    &source[node.byte_range()]
}

/// Hint for an explicit path qualifier `Q::method()` / `Q.method()`. `Self` is
/// advisory; a concrete `Uppercase` type is authoritative (`!Type`).
pub fn qualifier_hint(q: &str) -> Option<String> {
    let q = q.trim();
    if q == "Self" {
        return Some("Self".to_string());
    }
    if q.len() > 1 && q.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some(format!("!{q}"))
    } else {
        None
    }
}

pub fn strip_authority(hint: String) -> String {
    hint.strip_prefix('!').map(str::to_string).unwrap_or(hint)
}

/// Reduce a type expression to a bare type name.
pub fn clean_type_name(text: &str) -> Option<String> {
    let mut t = text.trim();
    loop {
        if let Some(r) = t.strip_prefix('&') {
            t = r.trim();
        } else if let Some(r) = t.strip_prefix("mut ") {
            t = r.trim();
        } else if let Some(r) = t.strip_prefix("dyn ") {
            t = r.trim();
        } else if t.starts_with('\'') {
            t = t
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest.trim())
                .unwrap_or("");
        } else {
            break;
        }
    }
    let t = t.split('<').next().unwrap_or(t).trim();
    let seg = t.rsplit("::").next().unwrap_or(t).trim();
    let seg = seg.rsplit('.').next().unwrap_or(seg).trim();
    if seg.len() > 1 && seg.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some(seg.to_string())
    } else {
        None
    }
}

pub fn short_call_name(raw: &str) -> Option<String> {
    let name = raw.split('<').next().unwrap_or(raw);
    let short = name
        .rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(name)
        .trim();
    if short
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        Some(short.to_string())
    } else {
        None
    }
}

pub fn push_call(
    out: &mut Vec<UnresolvedRef>,
    caller_id: &str,
    file_path: &str,
    raw_method: &str,
    hint: Option<String>,
) {
    let Some(short) = short_call_name(raw_method) else {
        return;
    };
    let mut uref = UnresolvedRef::new(
        caller_id.to_string(),
        short,
        EdgeKind::Calls,
        file_path.to_string(),
    );
    uref.receiver_hint = hint;
    out.push(uref);
}

pub fn receiver_from_identifier(name: &str, var_types: &VarTypes) -> Option<String> {
    if name == "self" || name == "this" {
        Some("Self".to_string())
    } else {
        var_types.get(name).cloned()
    }
}

pub fn resolve_js_callee(
    source: &str,
    func: TsNode,
    var_types: &VarTypes,
) -> (String, Option<String>) {
    match func.kind() {
        "member_expression" => {
            let method = func
                .child_by_field_name("property")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_default();
            let hint = func
                .child_by_field_name("object")
                .and_then(|o| resolve_js_receiver(source, o, var_types));
            (method, hint)
        }
        "identifier" => (node_text(source, func).to_string(), None),
        _ => {
            let full = node_text(source, func);
            let short = short_call_name(full).unwrap_or_else(|| full.to_string());
            (short, None)
        }
    }
}

fn resolve_js_receiver(source: &str, node: TsNode, var_types: &VarTypes) -> Option<String> {
    match node.kind() {
        "this" => Some("Self".to_string()),
        "identifier" => {
            let name = node_text(source, node);
            receiver_from_identifier(name, var_types).or_else(|| qualifier_hint(name))
        }
        "member_expression" => node
            .child_by_field_name("property")
            .and_then(|p| qualifier_hint(node_text(source, p))),
        _ => None,
    }
}

pub fn resolve_java_invocation(
    source: &str,
    node: TsNode,
    var_types: &VarTypes,
) -> Option<(String, Option<String>)> {
    let name = node.child_by_field_name("name").map(|n| node_text(source, n))?;
    let hint = node
        .child_by_field_name("object")
        .and_then(|o| resolve_java_receiver(source, o, var_types));
    Some((name.to_string(), hint))
}

fn resolve_java_receiver(source: &str, node: TsNode, var_types: &VarTypes) -> Option<String> {
    match node.kind() {
        "this" => Some("Self".to_string()),
        "identifier" => {
            let name = node_text(source, node);
            receiver_from_identifier(name, var_types).or_else(|| qualifier_hint(name))
        }
        "field_access" | "scoped_type_identifier" | "type_identifier" => node
            .child_by_field_name("field")
            .or_else(|| node.child_by_field_name("name"))
            .and_then(|n| qualifier_hint(node_text(source, n))),
        _ => None,
    }
}

pub fn infer_java_type(source: &str, value: TsNode) -> Option<String> {
    if value.kind() == "object_creation_expression" {
        return value
            .child_by_field_name("type")
            .and_then(|t| clean_type_name(node_text(source, t)));
    }
    None
}

pub fn collect_java_var_types(source: &str, fn_node: TsNode, out: &mut VarTypes) {
    out.clear();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() != "formal_parameter" {
                continue;
            }
            let name = child.child_by_field_name("name");
            let ty = child.child_by_field_name("type");
            if let (Some(name), Some(ty)) = (name, ty) {
                if let Some(t) = clean_type_name(node_text(source, ty)) {
                    out.insert(node_text(source, name).to_string(), t);
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_java_locals(source, body, out);
    }
}

fn collect_java_locals(source: &str, node: TsNode, out: &mut VarTypes) {
    if node.kind() == "local_variable_declaration" {
        let decl_ty = node
            .child_by_field_name("type")
            .and_then(|t| clean_type_name(node_text(source, t)));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() != "variable_declarator" {
                continue;
            }
            let name = child.child_by_field_name("name");
            let value = child.child_by_field_name("value");
            let ty = decl_ty.clone().or_else(|| {
                value.and_then(|v| infer_java_type(source, v))
            });
            if let (Some(name), Some(ty)) = (name, ty) {
                out.insert(node_text(source, name).to_string(), ty);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_java_locals(source, child, out);
    }
}

pub fn resolve_python_callee(
    source: &str,
    func: TsNode,
    var_types: &VarTypes,
) -> (String, Option<String>) {
    match func.kind() {
        "attribute" => {
            let method = func
                .child_by_field_name("attribute")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_default();
            let hint = func
                .child_by_field_name("object")
                .and_then(|o| resolve_python_receiver(source, o, var_types));
            (method, hint)
        }
        "identifier" => (node_text(source, func).to_string(), None),
        _ => {
            let full = node_text(source, func);
            let short = short_call_name(full).unwrap_or_else(|| full.to_string());
            (short, None)
        }
    }
}

fn resolve_python_receiver(source: &str, node: TsNode, var_types: &VarTypes) -> Option<String> {
    match node.kind() {
        "identifier" => {
            let name = node_text(source, node);
            receiver_from_identifier(name, var_types).or_else(|| qualifier_hint(name))
        }
        "attribute" => node
            .child_by_field_name("attribute")
            .and_then(|a| qualifier_hint(node_text(source, a))),
        _ => None,
    }
}

pub fn infer_python_type(source: &str, value: TsNode) -> Option<String> {
    if value.kind() == "call" {
        if let Some(func) = value.child_by_field_name("function") {
            if func.kind() == "identifier" {
                return qualifier_hint(node_text(source, func)).map(strip_authority);
            }
        }
    }
    None
}

pub fn collect_python_var_types(source: &str, fn_node: TsNode, out: &mut VarTypes) {
    out.clear();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "typed_parameter" {
                let name = child.child_by_field_name("name");
                let ty = child.child_by_field_name("type");
                if let (Some(name), Some(ty)) = (name, ty) {
                    if let Some(t) = clean_type_name(node_text(source, ty)) {
                        out.insert(node_text(source, name).to_string(), t);
                    }
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_python_assignments(source, body, out);
    }
}

fn collect_python_assignments(source: &str, node: TsNode, out: &mut VarTypes) {
    match node.kind() {
        "assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "identifier" {
                    let name = node_text(source, left).to_string();
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| clean_type_name(node_text(source, t)))
                        .or_else(|| {
                            node.child_by_field_name("right")
                                .and_then(|r| infer_python_type(source, r))
                        });
                    if let Some(ty) = ty {
                        out.insert(name, ty);
                    }
                }
            }
        }
        "annotated_assignment" => {
            if let Some(name) = node.child_by_field_name("name") {
                if name.kind() == "identifier" {
                    if let Some(ty) = node
                        .child_by_field_name("type")
                        .and_then(|t| clean_type_name(node_text(source, t)))
                    {
                        out.insert(node_text(source, name).to_string(), ty);
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_python_assignments(source, child, out);
    }
}

pub fn collect_js_var_types(source: &str, fn_node: TsNode, out: &mut VarTypes) {
    out.clear();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() != "required_parameter" && child.kind() != "optional_parameter" {
                continue;
            }
            let name = child
                .child_by_field_name("pattern")
                .filter(|p| p.kind() == "identifier")
                .or_else(|| {
                    if child.kind() == "identifier" {
                        Some(child)
                    } else {
                        None
                    }
                });
            let ty = child.child_by_field_name("type");
            if let (Some(name), Some(ty)) = (name, ty) {
                if let Some(t) = clean_type_name(node_text(source, ty)) {
                    out.insert(node_text(source, name).to_string(), t);
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_js_bindings(source, body, out);
    }
}

fn collect_js_bindings(source: &str, node: TsNode, out: &mut VarTypes) {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "variable_declarator" {
                    continue;
                }
                let name = child
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "identifier");
                let ty = child
                    .child_by_field_name("type")
                    .and_then(|t| clean_type_name(node_text(source, t)))
                    .or_else(|| {
                        child
                            .child_by_field_name("value")
                            .and_then(|v| infer_js_type(source, v))
                    });
                if let (Some(name), Some(ty)) = (name, ty) {
                    out.insert(node_text(source, name).to_string(), ty);
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_js_bindings(source, child, out);
    }
}

fn infer_js_type(source: &str, value: TsNode) -> Option<String> {
    if value.kind() == "new_expression" {
        if let Some(ctor) = value.child_by_field_name("constructor") {
            let name = node_text(source, ctor);
            let seg = name.rsplit('.').next().unwrap_or(name);
            return clean_type_name(seg);
        }
    }
    None
}

pub fn resolve_kotlin_call(
    source: &str,
    node: TsNode,
    var_types: &VarTypes,
) -> Option<(String, Option<String>)> {
    let callee = node.named_child(0)?;
    match callee.kind() {
        "identifier" | "simple_identifier" => {
            Some((node_text(source, callee).to_string(), None))
        }
        "navigation_expression" => {
            let method = callee
                .named_children(&mut callee.walk())
                .filter(|c| matches!(c.kind(), "identifier" | "simple_identifier"))
                .last()
                .map(|n| node_text(source, n).to_string())?;
            let hint = kotlin_navigation_receiver(source, callee, var_types);
            Some((method, hint))
        }
        _ => {
            let full = node_text(source, callee);
            short_call_name(full).map(|m| (m, None))
        }
    }
}

fn kotlin_navigation_receiver(source: &str, nav: TsNode, var_types: &VarTypes) -> Option<String> {
    nav.named_child(0)
        .and_then(|n| kotlin_receiver_expr(source, n, var_types))
}

fn kotlin_receiver_expr(source: &str, node: TsNode, var_types: &VarTypes) -> Option<String> {
    match node.kind() {
        "this" | "this_expression" => Some("Self".to_string()),
        "identifier" | "simple_identifier" => {
            let name = node_text(source, node);
            receiver_from_identifier(name, var_types).or_else(|| qualifier_hint(name))
        }
        _ => None,
    }
}

pub fn infer_kotlin_type(source: &str, value: TsNode) -> Option<String> {
    if value.kind() == "call_expression" {
        return value
            .named_child(0)
            .and_then(|c| kotlin_receiver_expr(source, c, &VarTypes::new()))
            .map(strip_authority);
    }
    None
}

pub fn collect_kotlin_var_types(source: &str, fn_node: TsNode, out: &mut VarTypes) {
    out.clear();
    if let Some(params) = fn_node
        .child_by_field_name("parameters")
        .or_else(|| fn_node.child_by_field_name("function_value_parameters"))
    {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if !matches!(
                child.kind(),
                "parameter" | "parameter_with_optional_type" | "parameter_with_type"
            ) {
                continue;
            }
            let name = child
                .child_by_field_name("name")
                .or_else(|| {
                    child
                        .named_children(&mut child.walk())
                        .find(|c| matches!(c.kind(), "identifier" | "simple_identifier"))
                });
            let ty = child
                .child_by_field_name("type")
                .or_else(|| child.child_by_field_name("return_type"));
            if let (Some(name), Some(ty)) = (name, ty) {
                if let Some(t) = clean_type_name(node_text(source, ty)) {
                    out.insert(node_text(source, name).to_string(), t);
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_kotlin_locals(source, body, out);
    }
}

fn collect_kotlin_locals(source: &str, node: TsNode, out: &mut VarTypes) {
    if matches!(
        node.kind(),
        "property_declaration" | "variable_declaration" | "local_variable_declaration"
    ) {
        let decl_ty = node
            .child_by_field_name("type")
            .and_then(|t| clean_type_name(node_text(source, t)));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !matches!(
                child.kind(),
                "variable_declaration" | "property_declaration" | "identifier"
                    | "simple_identifier"
            ) {
                continue;
            }
            let name = if matches!(child.kind(), "identifier" | "simple_identifier") {
                Some(child)
            } else {
                child
                    .child_by_field_name("name")
                    .or_else(|| {
                        child.named_children(&mut child.walk()).find(|c| {
                            matches!(c.kind(), "identifier" | "simple_identifier")
                        })
                    })
            };
            let value = node.child_by_field_name("value").or_else(|| {
                child.child_by_field_name("value")
            });
            let ty = decl_ty
                .clone()
                .or_else(|| value.and_then(|v| infer_kotlin_type(source, v)));
            if let (Some(name), Some(ty)) = (name, ty) {
                out.insert(node_text(source, name).to_string(), ty);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_kotlin_locals(source, child, out);
    }
}

pub fn resolve_csharp_invocation(
    source: &str,
    node: TsNode,
    var_types: &VarTypes,
) -> Option<(String, Option<String>)> {
    let f = node.child_by_field_name("function")?;
    match f.kind() {
        "member_access_expression" => {
            let name = f.child_by_field_name("name").map(|n| node_text(source, n))?;
            let hint = f
                .child_by_field_name("expression")
                .and_then(|e| resolve_csharp_receiver(source, e, var_types));
            Some((name.to_string(), hint))
        }
        "identifier" => Some((node_text(source, f).to_string(), None)),
        _ => {
            let full = node_text(source, f);
            short_call_name(full).map(|m| (m, None))
        }
    }
}

fn resolve_csharp_receiver(source: &str, node: TsNode, var_types: &VarTypes) -> Option<String> {
    match node.kind() {
        "this" | "base" => Some("Self".to_string()),
        "identifier" => {
            let name = node_text(source, node);
            receiver_from_identifier(name, var_types).or_else(|| qualifier_hint(name))
        }
        "member_access_expression" => node
            .child_by_field_name("name")
            .and_then(|n| qualifier_hint(node_text(source, n))),
        _ => None,
    }
}

pub fn infer_csharp_type(source: &str, value: TsNode) -> Option<String> {
    if value.kind() == "object_creation_expression" {
        return value
            .child_by_field_name("type")
            .and_then(|t| clean_type_name(node_text(source, t)));
    }
    None
}

pub fn collect_csharp_var_types(source: &str, fn_node: TsNode, out: &mut VarTypes) {
    out.clear();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() != "parameter" {
                continue;
            }
            let name = child.child_by_field_name("name");
            let ty = child.child_by_field_name("type");
            if let (Some(name), Some(ty)) = (name, ty) {
                if let Some(t) = clean_type_name(node_text(source, ty)) {
                    out.insert(node_text(source, name).to_string(), t);
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_csharp_locals(source, body, out);
    }
}

fn collect_csharp_locals(source: &str, node: TsNode, out: &mut VarTypes) {
    if node.kind() == "local_declaration_statement" {
        if let Some(decl) = node.child_by_field_name("declaration") {
            let decl_ty = decl
                .child_by_field_name("type")
                .and_then(|t| clean_type_name(node_text(source, t)));
            let mut cursor = decl.walk();
            for child in decl.children(&mut cursor) {
                if child.kind() != "variable_declarator" {
                    continue;
                }
                let name = child.child_by_field_name("name");
                let value = child.child_by_field_name("value");
                let ty = decl_ty
                    .clone()
                    .or_else(|| value.and_then(|v| infer_csharp_type(source, v)));
                if let (Some(name), Some(ty)) = (name, ty) {
                    out.insert(node_text(source, name).to_string(), ty);
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_csharp_locals(source, child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifier_hint_marks_concrete_types_authoritative() {
        assert_eq!(qualifier_hint("Connection"), Some("!Connection".to_string()));
        assert_eq!(qualifier_hint("Self"), Some("Self".to_string()));
        assert_eq!(qualifier_hint("T"), None);
    }

    #[test]
    fn clean_type_strips_references_and_generics() {
        assert_eq!(
            clean_type_name("&mut Connection"),
            Some("Connection".to_string())
        );
        assert_eq!(
            clean_type_name("java.util.ArrayList"),
            Some("ArrayList".to_string())
        );
        assert_eq!(clean_type_name("i32"), None);
    }

    #[test]
    fn kotlin_compact_class_parses_at_grammar_level() {
        let src = "class Db { fun open() {} fun run() { this.open() } }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        assert!(!root.has_error(), "compact class body should parse cleanly");
        assert!(
            root.children(&mut root.walk())
                .any(|c| c.kind() == "class_declaration")
        );
    }

    #[test]
    fn kotlin_resolves_qualified_navigation_call() {
        let src = "fun run() { Db.open() }\nobject Db { fun open() {} }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let call = find_node(tree.root_node(), "call_expression").expect("call_expression");
        let (method, hint) =
            resolve_kotlin_call(src, call, &VarTypes::new()).expect("resolved call");
        assert_eq!(method, "open");
        assert_eq!(hint, Some("!Db".to_string()));
    }

    fn find_node<'a>(node: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node(child, kind) {
                return Some(found);
            }
        }
        None
    }
}
