use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Trait,
    Protocol,
    Function,
    Method,
    Property,
    Field,
    Variable,
    Constant,
    Enum,
    EnumMember,
    TypeAlias,
    Namespace,
    Parameter,
    Import,
    Export,
    Route,
    Component,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Class => "class",
            NodeKind::Struct => "struct",
            NodeKind::Interface => "interface",
            NodeKind::Trait => "trait",
            NodeKind::Protocol => "protocol",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Property => "property",
            NodeKind::Field => "field",
            NodeKind::Variable => "variable",
            NodeKind::Constant => "constant",
            NodeKind::Enum => "enum",
            NodeKind::EnumMember => "enum_member",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Namespace => "namespace",
            NodeKind::Parameter => "parameter",
            NodeKind::Import => "import",
            NodeKind::Export => "export",
            NodeKind::Route => "route",
            NodeKind::Component => "component",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file" => Some(NodeKind::File),
            "module" => Some(NodeKind::Module),
            "class" => Some(NodeKind::Class),
            "struct" => Some(NodeKind::Struct),
            "interface" => Some(NodeKind::Interface),
            "trait" => Some(NodeKind::Trait),
            "protocol" => Some(NodeKind::Protocol),
            "function" => Some(NodeKind::Function),
            "method" => Some(NodeKind::Method),
            "property" => Some(NodeKind::Property),
            "field" => Some(NodeKind::Field),
            "variable" => Some(NodeKind::Variable),
            "constant" => Some(NodeKind::Constant),
            "enum" => Some(NodeKind::Enum),
            "enum_member" => Some(NodeKind::EnumMember),
            "type_alias" => Some(NodeKind::TypeAlias),
            "namespace" => Some(NodeKind::Namespace),
            "parameter" => Some(NodeKind::Parameter),
            "import" => Some(NodeKind::Import),
            "export" => Some(NodeKind::Export),
            "route" => Some(NodeKind::Route),
            "component" => Some(NodeKind::Component),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Calls,
    Imports,
    Exports,
    Extends,
    Implements,
    References,
    TypeOf,
    Returns,
    Instantiates,
    Overrides,
    Decorates,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Calls => "calls",
            EdgeKind::Imports => "imports",
            EdgeKind::Exports => "exports",
            EdgeKind::Extends => "extends",
            EdgeKind::Implements => "implements",
            EdgeKind::References => "references",
            EdgeKind::TypeOf => "type_of",
            EdgeKind::Returns => "returns",
            EdgeKind::Instantiates => "instantiates",
            EdgeKind::Overrides => "overrides",
            EdgeKind::Decorates => "decorates",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "contains" => Some(EdgeKind::Contains),
            "calls" => Some(EdgeKind::Calls),
            "imports" => Some(EdgeKind::Imports),
            "exports" => Some(EdgeKind::Exports),
            "extends" => Some(EdgeKind::Extends),
            "implements" => Some(EdgeKind::Implements),
            "references" => Some(EdgeKind::References),
            "type_of" => Some(EdgeKind::TypeOf),
            "returns" => Some(EdgeKind::Returns),
            "instantiates" => Some(EdgeKind::Instantiates),
            "overrides" => Some(EdgeKind::Overrides),
            "decorates" => Some(EdgeKind::Decorates),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    TreeSitter,
    Heuristic,
}

impl Provenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provenance::TreeSitter => "tree-sitter",
            Provenance::Heuristic => "heuristic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub visibility: Option<String>,
    pub is_exported: bool,
    pub is_async: bool,
    pub is_static: bool,
    pub is_abstract: bool,
}

impl Node {
    pub fn new_id(file_path: &str, qualified_name: &str) -> String {
        let input = format!("{}::{}", file_path, qualified_name);
        blake3::hash(input.as_bytes()).to_hex().to_string()[..16].to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
    pub provenance: Provenance,
    pub metadata: Option<serde_json::Value>,
}

impl Edge {
    pub fn new_id(source: &str, target: &str, kind: &EdgeKind) -> String {
        let input = format!("{}->{}:{}", source, target, kind.as_str());
        blake3::hash(input.as_bytes()).to_hex().to_string()[..16].to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub size: u64,
    pub last_indexed: i64,
}

/// Unresolved cross-file reference, stored for later resolution.
#[derive(Debug, Clone)]
pub struct UnresolvedRef {
    pub source_node_id: String,
    pub target_name: String,
    pub kind: EdgeKind,
    pub file_path: String,
    /// Optional type the reference is qualified by, for type-directed
    /// resolution. For a call `recv.method()` or `Type::method()` this is the
    /// (inferred) type of the receiver/qualifier; the special value `"Self"`
    /// means "the caller's own enclosing type". `None` falls back to name-only
    /// resolution.
    pub receiver_hint: Option<String>,
}

impl UnresolvedRef {
    /// Construct a name-only reference (no type hint).
    pub fn new(source_node_id: String, target_name: String, kind: EdgeKind, file_path: String) -> Self {
        Self {
            source_node_id,
            target_name,
            kind,
            file_path,
            receiver_hint: None,
        }
    }
}

/// Map a file extension to a language with a registered extractor.
/// Only languages that `crate::extract::extractor_for` can handle are listed.
pub fn detect_language(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some("rust"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some("cpp"),
        "el" => Some("elisp"),
        "lisp" | "cl" | "lsp" | "asd" => Some("commonlisp"),
        "scm" | "ss" | "sld" | "sls" => Some("scheme"),
        "rb" => Some("ruby"),
        "cs" => Some("csharp"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        "dart" => Some("dart"),
        "svelte" => Some("svelte"),
        "vue" => Some("vue"),
        _ => None,
    }
}

/// Directory fragments that are never worth indexing. Shared by the indexer's
/// walk and the file watcher so the two cannot drift apart.
pub const IGNORED_DIR_FRAGMENTS: &[&str] = &[
    "/.git/",
    "/node_modules/",
    "/target/",
    "/dist/",
    "/build/",
    "/.rusty-graph/",
];

/// True if `path` lives under a directory we always skip.
pub fn is_ignored_path(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    IGNORED_DIR_FRAGMENTS.iter().any(|frag| s.contains(frag))
}

/// Refine an extension-based language guess using file contents. Currently this
/// disambiguates `.h` headers, which are C by extension but routinely contain
/// C++ (classes, namespaces, templates); misclassifying them yields garbage
/// parses.
pub fn refine_language(lang: &'static str, path: &std::path::Path, content: &str) -> &'static str {
    let is_header = path.extension().and_then(|e| e.to_str()) == Some("h");
    if lang == "c" && is_header && looks_like_cpp(content) {
        "cpp"
    } else {
        lang
    }
}

fn looks_like_cpp(content: &str) -> bool {
    content.contains("class ")
        || content.contains("namespace ")
        || content.contains("template<")
        || content.contains("template <")
        || content.contains("::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detect_language_maps_known_extensions() {
        assert_eq!(detect_language(Path::new("a.rs")), Some("rust"));
        assert_eq!(detect_language(Path::new("a.py")), Some("python"));
        assert_eq!(detect_language(Path::new("a.go")), Some("go"));
        assert_eq!(detect_language(Path::new("a.ts")), Some("typescript"));
        assert_eq!(detect_language(Path::new("a.tsx")), Some("typescript"));
        assert_eq!(detect_language(Path::new("a.js")), Some("javascript"));
        assert_eq!(detect_language(Path::new("a.java")), Some("java"));
        assert_eq!(detect_language(Path::new("a.c")), Some("c"));
        assert_eq!(detect_language(Path::new("a.h")), Some("c"));
        assert_eq!(detect_language(Path::new("a.cpp")), Some("cpp"));
        assert_eq!(detect_language(Path::new("a.hpp")), Some("cpp"));
        assert_eq!(detect_language(Path::new("a.el")), Some("elisp"));
        assert_eq!(detect_language(Path::new("a.lisp")), Some("commonlisp"));
        assert_eq!(detect_language(Path::new("a.cl")), Some("commonlisp"));
        assert_eq!(detect_language(Path::new("a.scm")), Some("scheme"));
        assert_eq!(detect_language(Path::new("a.sld")), Some("scheme"));
        assert_eq!(detect_language(Path::new("a.rb")), Some("ruby"));
        assert_eq!(detect_language(Path::new("a.cs")), Some("csharp"));
        assert_eq!(detect_language(Path::new("a.php")), Some("php"));
        assert_eq!(detect_language(Path::new("a.swift")), Some("swift"));
        assert_eq!(detect_language(Path::new("a.kt")), Some("kotlin"));
        assert_eq!(detect_language(Path::new("a.dart")), Some("dart"));
        assert_eq!(detect_language(Path::new("a.svelte")), Some("svelte"));
        assert_eq!(detect_language(Path::new("a.vue")), Some("vue"));
    }

    #[test]
    fn detect_language_returns_none_for_unknown_or_missing_extension() {
        assert_eq!(detect_language(Path::new("a.txt")), None);
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }

    #[test]
    fn refine_language_promotes_cpp_headers() {
        let cpp_header = "#pragma once\nclass Widget { int x; };\n";
        assert_eq!(refine_language("c", Path::new("w.h"), cpp_header), "cpp");
        let c_header = "#pragma once\nint add(int a, int b);\n";
        assert_eq!(refine_language("c", Path::new("w.h"), c_header), "c");
        // A real .c file is never reclassified, regardless of contents.
        assert_eq!(refine_language("c", Path::new("w.c"), "class X {};"), "c");
    }

    #[test]
    fn is_ignored_path_matches_known_dirs() {
        assert!(is_ignored_path(Path::new("/p/target/debug/x.rs")));
        assert!(is_ignored_path(Path::new("/p/node_modules/lib/a.js")));
        assert!(!is_ignored_path(Path::new("/p/src/a.rs")));
    }

    #[test]
    fn node_kind_round_trips_through_str() {
        for kind in [
            NodeKind::File,
            NodeKind::Module,
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Field,
            NodeKind::Enum,
            NodeKind::EnumMember,
            NodeKind::TypeAlias,
            NodeKind::Namespace,
            NodeKind::Import,
            NodeKind::Constant,
        ] {
            assert_eq!(
                NodeKind::from_str(kind.as_str()),
                Some(kind.clone()),
                "{:?}",
                kind
            );
        }
        assert_eq!(NodeKind::from_str("not_a_kind"), None);
    }

    #[test]
    fn edge_kind_round_trips_through_str() {
        for kind in [
            EdgeKind::Contains,
            EdgeKind::Calls,
            EdgeKind::Imports,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::References,
            EdgeKind::Overrides,
        ] {
            assert_eq!(
                EdgeKind::from_str(kind.as_str()),
                Some(kind.clone()),
                "{:?}",
                kind
            );
        }
        assert_eq!(EdgeKind::from_str("not_an_edge"), None);
    }

    #[test]
    fn node_id_is_deterministic_and_distinct() {
        let a1 = Node::new_id("src/foo.rs", "foo::bar");
        let a2 = Node::new_id("src/foo.rs", "foo::bar");
        let b = Node::new_id("src/foo.rs", "foo::baz");
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.len(), 16);
    }

    #[test]
    fn edge_id_depends_on_kind() {
        let calls = Edge::new_id("s", "t", &EdgeKind::Calls);
        let contains = Edge::new_id("s", "t", &EdgeKind::Contains);
        assert_ne!(calls, contains);
        assert_eq!(calls, Edge::new_id("s", "t", &EdgeKind::Calls));
    }
}
