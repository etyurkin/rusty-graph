mod cfamily;
mod common;
mod hints;
mod csharp;
mod dart;
mod go;
mod java;
mod javascript;
mod kotlin;
mod lisp;
mod php;
mod python;
pub mod routes;
mod ruby;
mod rust;
mod swift;
mod webcomponent;

use anyhow::Result;
use std::path::Path;

use crate::types::{Edge, Node, UnresolvedRef};

pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedRef>,
}

impl ExtractionResult {
    pub fn empty() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            unresolved: vec![],
        }
    }
}

pub trait Extractor: Send + Sync {
    fn language(&self) -> &'static str;
    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult>;
}

pub fn extractor_for(language: &str) -> Option<Box<dyn Extractor>> {
    match language {
        "typescript" | "javascript" => Some(Box::new(javascript::JsExtractor)),
        "rust" => Some(Box::new(rust::RustExtractor)),
        "python" => Some(Box::new(python::PythonExtractor)),
        "go" => Some(Box::new(go::GoExtractor)),
        "java" => Some(Box::new(java::JavaExtractor)),
        "c" => Some(Box::new(cfamily::CFamilyExtractor { cpp: false })),
        "cpp" => Some(Box::new(cfamily::CFamilyExtractor { cpp: true })),
        "elisp" => Some(Box::new(lisp::LispExtractor {
            dialect: lisp::Dialect::Elisp,
        })),
        "commonlisp" => Some(Box::new(lisp::LispExtractor {
            dialect: lisp::Dialect::CommonLisp,
        })),
        "scheme" => Some(Box::new(lisp::LispExtractor {
            dialect: lisp::Dialect::Scheme,
        })),
        "ruby" => Some(Box::new(ruby::RubyExtractor)),
        "csharp" => Some(Box::new(csharp::CSharpExtractor)),
        "php" => Some(Box::new(php::PhpExtractor)),
        "swift" => Some(Box::new(swift::SwiftExtractor)),
        "kotlin" => Some(Box::new(kotlin::KotlinExtractor)),
        "dart" => Some(Box::new(dart::DartExtractor)),
        "svelte" => Some(Box::new(webcomponent::WebComponentExtractor {
            language: "svelte",
        })),
        "vue" => Some(Box::new(webcomponent::WebComponentExtractor {
            language: "vue",
        })),
        _ => None,
    }
}

/// Map a language id to its tree-sitter grammar. Mirrors `extractor_for`'s
/// language coverage; returns `None` for languages without a single grammar
/// (e.g. Svelte/Vue, which delegate to the JS grammar per `<script>` block).
pub fn language_for(language: &str) -> Option<tree_sitter::Language> {
    let lang = match language {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "elisp" => tree_sitter_elisp::LANGUAGE.into(),
        "commonlisp" => tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
        "scheme" => tree_sitter_scheme::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        _ => return None,
    };
    Some(lang)
}

/// Parse `source` and count tree-sitter `ERROR` and `MISSING` nodes. A non-zero
/// count means the grammar couldn't fully parse the file, so extracted symbols
/// for it are likely incomplete — surfaced via `status --health`.
pub fn parse_diagnostics(language: &str, source: &str) -> (u32, u32) {
    let Some(lang) = language_for(language) else {
        return (0, 0);
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang).is_err() {
        return (0, 0);
    }
    let Some(tree) = parser.parse(source, None) else {
        return (0, 0);
    };
    let mut errors = 0u32;
    let mut missing = 0u32;
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() {
            errors += 1;
        }
        if node.is_missing() {
            missing += 1;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    (errors, missing)
}

/// Shared utilities for building node IDs and edges.
pub(crate) mod util {
    use crate::types::{Edge, EdgeKind, Node, Provenance};

    pub fn contains_edge(parent: &Node, child: &Node) -> Edge {
        let id = Edge::new_id(&parent.id, &child.id, &EdgeKind::Contains);
        Edge {
            id,
            source: parent.id.clone(),
            target: child.id.clone(),
            kind: EdgeKind::Contains,
            provenance: Provenance::TreeSitter,
            metadata: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeKind, NodeKind};
    use std::path::Path;

    fn extract(lang: &str, file: &str, src: &str) -> ExtractionResult {
        extractor_for(lang)
            .unwrap_or_else(|| panic!("no extractor for {lang}"))
            .extract(Path::new(file), src)
            .expect("extraction failed")
    }

    fn names(res: &ExtractionResult) -> Vec<&str> {
        res.nodes.iter().map(|n| n.name.as_str()).collect()
    }

    fn has(res: &ExtractionResult, name: &str, kind: NodeKind) -> bool {
        res.nodes.iter().any(|n| n.name == name && n.kind == kind)
    }

    fn calls(res: &ExtractionResult, target: &str) -> bool {
        res.unresolved
            .iter()
            .any(|u| u.target_name == target && matches!(u.kind, EdgeKind::Calls))
    }

    #[test]
    fn unknown_language_has_no_extractor() {
        assert!(extractor_for("cobol").is_none());
    }

    #[test]
    fn every_result_starts_with_a_file_node() {
        let res = extract("rust", "t.rs", "pub fn a() {}\n");
        assert!(res.nodes.iter().any(|n| n.kind == NodeKind::File));
    }

    #[test]
    fn rust_extracts_struct_method_function_and_calls() {
        let src = r#"
pub struct Foo { x: i32 }
impl Foo {
    pub fn bar(&self) -> i32 { self.x }
}
fn helper() -> i32 { 1 }
pub fn run() -> i32 { helper() }
"#;
        let res = extract("rust", "t.rs", src);
        let n = names(&res);
        assert!(n.contains(&"Foo"));
        assert!(n.contains(&"bar"));
        assert!(n.contains(&"helper"));
        assert!(n.contains(&"run"));
        assert!(has(&res, "Foo", NodeKind::Struct));
        assert!(has(&res, "bar", NodeKind::Method));
        assert!(has(&res, "helper", NodeKind::Function));
        assert!(
            calls(&res, "helper"),
            "run() should record a call to helper()"
        );
    }

    #[test]
    fn rust_marks_pub_items_exported() {
        let res = extract("rust", "t.rs", "pub fn shown() {}\nfn hidden() {}\n");
        let shown = res.nodes.iter().find(|n| n.name == "shown").unwrap();
        let hidden = res.nodes.iter().find(|n| n.name == "hidden").unwrap();
        assert!(shown.is_exported);
        assert!(!hidden.is_exported);
    }

    #[test]
    fn python_extracts_class_method_function_and_calls() {
        let src = "class Animal:\n    def speak(self):\n        return noise()\n\ndef noise():\n    return 'woof'\n";
        let res = extract("python", "t.py", src);
        let n = names(&res);
        assert!(n.contains(&"Animal"));
        assert!(n.contains(&"speak"));
        assert!(n.contains(&"noise"));
        assert!(has(&res, "Animal", NodeKind::Class));
        assert!(has(&res, "speak", NodeKind::Method));
        assert!(
            calls(&res, "noise"),
            "speak() should record a call to noise()"
        );
    }

    #[test]
    fn javascript_extracts_function_class_method_and_calls() {
        let src = "function greet() { return 1; }\nclass Widget { render() { return greet(); } }\n";
        let res = extract("javascript", "t.js", src);
        let n = names(&res);
        assert!(n.contains(&"greet"));
        assert!(n.contains(&"Widget"));
        assert!(n.contains(&"render"));
        assert!(has(&res, "Widget", NodeKind::Class));
        assert!(
            calls(&res, "greet"),
            "render() should record a call to greet()"
        );
    }

    #[test]
    fn java_extracts_class_method_and_calls() {
        let src = "package app;\nimport java.util.List;\npublic class Service {\n  private int helper() { return 1; }\n  public int run() { return helper(); }\n}\n";
        let res = extract("java", "Service.java", src);
        let n = names(&res);
        assert!(n.contains(&"Service"));
        assert!(n.contains(&"helper"));
        assert!(n.contains(&"run"));
        assert!(has(&res, "Service", NodeKind::Class));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(
            calls(&res, "helper"),
            "run() should record a call to helper()"
        );
        let svc = res.nodes.iter().find(|x| x.name == "Service").unwrap();
        assert!(svc.is_exported);
        let helper = res.nodes.iter().find(|x| x.name == "helper").unwrap();
        assert!(!helper.is_exported);
    }

    #[test]
    fn java_qualified_call_carries_type_hint() {
        let src = "class Db { static void open() {} }\n\
                   class Connection { static void open() {} }\n\
                   class App { void run() { Db.open(); Connection.open(); } }\n";
        let res = extract("java", "App.java", src);
        let open_hints: Vec<_> = res
            .unresolved
            .iter()
            .filter(|u| u.target_name == "open" && matches!(u.kind, EdgeKind::Calls))
            .map(|u| u.receiver_hint.clone())
            .collect();
        assert!(
            open_hints.contains(&Some("!Db".to_string())),
            "Db.open() should carry !Db hint: {open_hints:?}"
        );
        assert!(
            open_hints.contains(&Some("!Connection".to_string())),
            "Connection.open() should carry !Connection hint: {open_hints:?}"
        );
    }

    #[test]
    fn javascript_qualified_call_carries_type_hint() {
        let src = "class Db { static open() {} }\n\
                   class Connection { static open() {} }\n\
                   function run() { Db.open(); Connection.open(); }\n";
        let res = extract("javascript", "app.js", src);
        let hints: Vec<_> = res
            .unresolved
            .iter()
            .filter(|u| u.target_name == "open")
            .map(|u| u.receiver_hint.clone())
            .collect();
        assert!(hints.contains(&Some("!Db".to_string())), "{hints:?}");
        assert!(hints.contains(&Some("!Connection".to_string())), "{hints:?}");
    }

    #[test]
    fn python_qualified_call_carries_type_hint() {
        let src = "class Db:\n    @staticmethod\n    def open(): pass\n\
                   class Connection:\n    @staticmethod\n    def open(): pass\n\
                   def run():\n        Db.open()\n        Connection.open()\n";
        let res = extract("python", "app.py", src);
        let hints: Vec<_> = res
            .unresolved
            .iter()
            .filter(|u| u.target_name == "open")
            .map(|u| u.receiver_hint.clone())
            .collect();
        assert!(hints.contains(&Some("!Db".to_string())), "{hints:?}");
        assert!(hints.contains(&Some("!Connection".to_string())), "{hints:?}");
    }

    #[test]
    fn c_extracts_struct_functions_and_calls() {
        let src = "#include <stdio.h>\nstruct Point { int x; int y; };\nstatic int helper(void) { return 1; }\nint run(void) { return helper(); }\n";
        let res = extract("c", "t.c", src);
        let n = names(&res);
        assert!(n.contains(&"Point"));
        assert!(n.contains(&"helper"));
        assert!(n.contains(&"run"));
        assert!(has(&res, "Point", NodeKind::Struct));
        assert!(has(&res, "helper", NodeKind::Function));
        assert!(
            calls(&res, "helper"),
            "run() should record a call to helper()"
        );
    }

    #[test]
    fn cpp_extracts_class_namespace_method_and_calls() {
        let src = "namespace app {\nclass Widget {\npublic:\n  int render();\n};\nint helper() { return 1; }\nint Widget::render() { return helper(); }\n}\n";
        let res = extract("cpp", "t.cpp", src);
        let n = names(&res);
        assert!(n.contains(&"app"), "{n:?}");
        assert!(n.contains(&"Widget"), "{n:?}");
        assert!(n.contains(&"render"), "{n:?}");
        assert!(has(&res, "app", NodeKind::Namespace));
        assert!(has(&res, "Widget", NodeKind::Class));
        assert!(
            calls(&res, "helper"),
            "render() should record a call to helper()"
        );
    }

    #[test]
    fn elisp_extracts_defun_defvar_and_calls() {
        let src = "(defvar my-var 1)\n(defun my-helper () 1)\n(defun my-run () (my-helper))\n";
        let res = extract("elisp", "t.el", src);
        let n = names(&res);
        assert!(n.contains(&"my-var"));
        assert!(n.contains(&"my-helper"));
        assert!(n.contains(&"my-run"));
        assert!(has(&res, "my-var", NodeKind::Variable));
        assert!(has(&res, "my-helper", NodeKind::Function));
        assert!(calls(&res, "my-helper"), "my-run should call my-helper");
    }

    #[test]
    fn commonlisp_extracts_defun_defclass_and_calls() {
        let src = "(defpackage :app)\n(defclass point () ())\n(defun helper () 1)\n(defun run () (helper))\n";
        let res = extract("commonlisp", "t.lisp", src);
        let n = names(&res);
        assert!(n.contains(&"app"), "{n:?}");
        assert!(n.contains(&"point"), "{n:?}");
        assert!(n.contains(&"helper"));
        assert!(has(&res, "point", NodeKind::Struct));
        assert!(has(&res, "app", NodeKind::Module));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn scheme_distinguishes_function_and_value_defines() {
        let src = "(define x 42)\n(define (helper) 1)\n(define (run) (helper))\n";
        let res = extract("scheme", "t.scm", src);
        assert!(has(&res, "x", NodeKind::Variable), "{:?}", names(&res));
        assert!(has(&res, "helper", NodeKind::Function));
        assert!(has(&res, "run", NodeKind::Function));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn go_extracts_struct_functions_and_calls() {
        let src = "package main\n\ntype Server struct {}\n\nfunc helper() int { return 1 }\n\nfunc Run() int { return helper() }\n";
        let res = extract("go", "t.go", src);
        let n = names(&res);
        assert!(n.contains(&"Server"));
        assert!(n.contains(&"helper"));
        assert!(n.contains(&"Run"));
        assert!(has(&res, "Server", NodeKind::Struct));
        assert!(
            calls(&res, "helper"),
            "Run() should record a call to helper()"
        );
    }

    #[test]
    fn ruby_extracts_module_class_method_and_calls() {
        let src = "module App\n  class User < Base\n    def run\n      helper()\n    end\n    def helper; 1; end\n  end\nend\n";
        let res = extract("ruby", "user.rb", src);
        assert!(has(&res, "App", NodeKind::Module), "{:?}", names(&res));
        assert!(has(&res, "User", NodeKind::Class));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(calls(&res, "helper"), "run should call helper");
        assert!(res
            .unresolved
            .iter()
            .any(|u| u.target_name == "Base" && matches!(u.kind, EdgeKind::Extends)));
    }

    #[test]
    fn csharp_extracts_namespace_class_method_and_calls() {
        let src = "namespace App {\n  public class User {\n    private int Helper() { return 1; }\n    public int Run() { return Helper(); }\n  }\n}\n";
        let res = extract("csharp", "User.cs", src);
        assert!(has(&res, "App", NodeKind::Namespace), "{:?}", names(&res));
        assert!(has(&res, "User", NodeKind::Class));
        assert!(has(&res, "Helper", NodeKind::Method));
        assert!(calls(&res, "Helper"), "Run should call Helper");
        let user = res.nodes.iter().find(|n| n.name == "User").unwrap();
        assert!(user.is_exported);
        let helper = res.nodes.iter().find(|n| n.name == "Helper").unwrap();
        assert!(!helper.is_exported);
    }

    #[test]
    fn csharp_qualified_call_carries_type_hint() {
        let src = "class Db { public static void Open() {} }\n\
                   class Connection { public static void Open() {} }\n\
                   class App { void Run() { Db.Open(); Connection.Open(); } }\n";
        let res = extract("csharp", "App.cs", src);
        let hints: Vec<_> = res
            .unresolved
            .iter()
            .filter(|u| u.target_name == "Open")
            .map(|u| u.receiver_hint.clone())
            .collect();
        assert!(hints.contains(&Some("!Db".to_string())), "{hints:?}");
        assert!(hints.contains(&Some("!Connection".to_string())), "{hints:?}");
    }

    #[test]
    fn php_extracts_class_method_and_calls() {
        let src = "<?php\nclass User {\n  private function helper() { return 1; }\n  public function run() { return $this->helper(); }\n}\n";
        let res = extract("php", "User.php", src);
        assert!(has(&res, "User", NodeKind::Class), "{:?}", names(&res));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn swift_extracts_class_struct_method_and_calls() {
        let src = "class User {\n  func helper() -> Int { return 1 }\n  func run() -> Int { return helper() }\n}\nstruct Point { var x: Int }\n";
        let res = extract("swift", "User.swift", src);
        assert!(has(&res, "User", NodeKind::Class), "{:?}", names(&res));
        assert!(has(&res, "Point", NodeKind::Struct));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn kotlin_extracts_class_method_and_calls() {
        let src = "class User {\n  fun helper(): Int { return 1 }\n  fun run(): Int { return helper() }\n}\n";
        let res = extract("kotlin", "User.kt", src);
        assert!(has(&res, "User", NodeKind::Class), "{:?}", names(&res));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn kotlin_compact_class_body_parses_without_layout_hacks() {
        let src = "class Db { fun open() {} fun run() { this.open() } }";
        let res = extract("kotlin", "Db.kt", src);
        assert!(has(&res, "Db", NodeKind::Class), "{:?}", names(&res));
        assert!(has(&res, "open", NodeKind::Method));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(calls(&res, "open"));
    }

    #[test]
    fn kotlin_single_file_class_extracts_run_and_open_calls() {
        let src = "class Db {\n  fun open() {}\n  fun run() { this.open() }\n}\n";
        let res = extract("kotlin", "Db.kt", src);
        assert!(has(&res, "Db", NodeKind::Class));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(calls(&res, "open"));
    }

    #[test]
    fn kotlin_qualified_call_carries_type_hint() {
        let src = "class User {\n  fun open() {}\n  fun run() { this.open() }\n}\n";
        let res = extract("kotlin", "User.kt", src);
        let hint = res
            .unresolved
            .iter()
            .find(|u| u.target_name == "open" && matches!(u.kind, EdgeKind::Calls))
            .and_then(|u| u.receiver_hint.clone());
        assert!(calls(&res, "open"), "expected open() call");
        assert_eq!(hint, Some("Self".to_string()));
    }

    #[test]
    fn dart_extracts_class_method_and_calls() {
        let src =
            "class User {\n  int helper() { return 1; }\n  int run() { return helper(); }\n}\n";
        let res = extract("dart", "user.dart", src);
        assert!(has(&res, "User", NodeKind::Class), "{:?}", names(&res));
        assert!(has(&res, "helper", NodeKind::Method));
        assert!(has(&res, "run", NodeKind::Method));
        assert!(calls(&res, "helper"), "run should call helper");
    }

    #[test]
    fn svelte_extracts_script_symbols_with_line_offset() {
        let src = "<h1>Title</h1>\n<script>\nfunction greet() { return 1; }\nfunction run() { return greet(); }\n</script>\n";
        let res = extract("svelte", "Card.svelte", src);
        assert!(has(&res, "greet", NodeKind::Function), "{:?}", names(&res));
        assert!(has(&res, "run", NodeKind::Function));
        assert!(calls(&res, "greet"), "run should call greet");
        // The file node is relabelled to the component language.
        let file = res.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
        assert_eq!(file.language, "svelte");
        // greet sits on line 3 of the original file, not line 1 of the script.
        let greet = res.nodes.iter().find(|n| n.name == "greet").unwrap();
        assert_eq!(greet.start_line, 3);
    }
}
