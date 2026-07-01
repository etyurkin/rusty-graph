//! Framework route recognition.
//!
//! HTTP routes are declared in a handful of recognizable shapes across the web
//! frameworks we care about:
//!   * method-call DSLs  — `app.get('/x', h)` (Express/Koa/Fastify), `r.GET("/x", h)` (Gin/Echo), `Route::get('/x', h)` (Laravel)
//!   * decorators        — `@app.get('/x')` (FastAPI/Flask), `@Get('/x')` (NestJS)
//!   * annotations       — `@GetMapping("/x")` (Spring)
//!   * attributes        — `[HttpGet("/x")]` (ASP.NET), `#[Route('/x')]` (Symfony)
//!   * Rails routes.rb   — `get '/x', to: 'c#a'`
//!   * file conventions  — Next.js `pages/api/**`, `app/**/route.ts`
//!
//! All of these reduce to "a recognizable token, then `(`/whitespace, then a
//! string path". We scan for those tokens textually (the AST shape varies too
//! much per framework to be worth six bespoke tree walks) and emit a `Route`
//! node per match, linked to the file and—when discoverable—its handler symbol.

use std::path::Path;

use super::ExtractionResult;
use crate::types::{Edge, EdgeKind, Node, NodeKind, Provenance, UnresolvedRef};

const HTTP_VERBS_LOWER: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "all",
];
const HTTP_VERBS_UPPER: &[&str] = &[
    "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "ANY",
];

struct Hit {
    verb: String,
    path: String,
    handler: Option<String>,
    byte: usize,
}

/// Append any framework routes found in `source` to `result`.
pub fn append(result: &mut ExtractionResult, path: &Path, language: &str, source: &str) {
    let needles = needles_for(language, path);
    let refs: Vec<(&str, &str)> = needles
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut hits = scan(source, &refs);
    if let Some((verb, route)) = nextjs_file_route(path, language) {
        hits.push(Hit {
            verb,
            path: route,
            handler: None,
            byte: 0,
        });
    }
    if hits.is_empty() {
        return;
    }
    let file_path = path.to_string_lossy().to_string();
    let file_id = Node::new_id(&file_path, &file_path);
    for hit in hits {
        let label = format!("{} {}", hit.verb, hit.path);
        let qualified = format!("{}::route::{}", file_path, label);
        let id = Node::new_id(&file_path, &qualified);
        let line = line_of(source, hit.byte);
        let route = Node {
            id: id.clone(),
            kind: NodeKind::Route,
            name: label,
            qualified_name: qualified,
            file_path: file_path.clone(),
            language: language.to_string(),
            start_line: line,
            end_line: line,
            signature: Some(match &hit.handler {
                Some(h) => format!("{} {} -> {}", hit.verb, hit.path, h),
                None => format!("{} {}", hit.verb, hit.path),
            }),
            docstring: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
        };
        result.edges.push(Edge {
            id: Edge::new_id(&file_id, &id, &EdgeKind::Contains),
            source: file_id.clone(),
            target: id.clone(),
            kind: EdgeKind::Contains,
            provenance: Provenance::TreeSitter,
            metadata: None,
        });
        if let Some(handler) = hit.handler {
            result.unresolved.push(UnresolvedRef::new(
                id,
                handler,
                EdgeKind::References,
                file_path.clone(),
            ));
        }
        result.nodes.push(route);
    }
}

/// Build the (token, verb) needle list for a language/file. Each token is the
/// literal text that precedes the route path, ending just before the path.
fn needles_for(language: &str, path: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    match language {
        "javascript" | "typescript" | "svelte" | "vue" => {
            // Express / Koa / Fastify method DSL on common router receivers.
            let receivers = ["app", "router", "route", "server", "api", "fastify", "r"];
            for recv in receivers {
                for v in HTTP_VERBS_LOWER {
                    out.push((format!("{recv}.{v}("), v.to_uppercase()));
                }
            }
            // NestJS decorators.
            for v in [
                "Get", "Post", "Put", "Delete", "Patch", "Options", "Head", "All",
            ] {
                out.push((format!("@{v}("), v.to_uppercase()));
            }
        }
        "python" => {
            // FastAPI / Flask decorators: @app.get(...), @router.post(...), @app.route(...)
            let receivers = ["app", "router", "api", "blueprint", "bp"];
            for recv in receivers {
                out.push((format!("@{recv}.route("), "ANY".to_string()));
                for v in HTTP_VERBS_LOWER {
                    out.push((format!("@{recv}.{v}("), v.to_uppercase()));
                }
            }
            // Django URLConf.
            if stem == "urls" {
                out.push(("path(".to_string(), "ANY".to_string()));
                out.push(("re_path(".to_string(), "ANY".to_string()));
            }
        }
        "ruby" => {
            // Rails routes.rb DSL: `get '/x', to: 'c#a'` (no parentheses).
            if stem == "routes" {
                for v in HTTP_VERBS_LOWER {
                    out.push((format!("{v} "), v.to_uppercase()));
                }
            }
        }
        "php" => {
            // Laravel facade DSL and Symfony attributes.
            for v in HTTP_VERBS_LOWER {
                out.push((format!("Route::{v}("), v.to_uppercase()));
            }
            out.push(("#[Route(".to_string(), "ANY".to_string()));
        }
        "go" => {
            // Gin / Echo / chi method DSL on common receivers.
            let receivers = ["r", "router", "e", "g", "group", "mux", "api", "v1", "rg"];
            for recv in receivers {
                for v in HTTP_VERBS_UPPER {
                    out.push((format!("{recv}.{v}("), (*v).to_string()));
                }
            }
        }
        "java" | "kotlin" => {
            // Spring MVC annotations.
            for (token, verb) in [
                ("@GetMapping(", "GET"),
                ("@PostMapping(", "POST"),
                ("@PutMapping(", "PUT"),
                ("@DeleteMapping(", "DELETE"),
                ("@PatchMapping(", "PATCH"),
                ("@RequestMapping(", "ANY"),
            ] {
                out.push((token.to_string(), verb.to_string()));
            }
        }
        "csharp" => {
            for (token, verb) in [
                ("[HttpGet(", "GET"),
                ("[HttpPost(", "POST"),
                ("[HttpPut(", "PUT"),
                ("[HttpDelete(", "DELETE"),
                ("[HttpPatch(", "PATCH"),
                ("[Route(", "ANY"),
            ] {
                out.push((token.to_string(), verb.to_string()));
            }
        }
        _ => {}
    }
    out
}

/// Next.js file conventions: `pages/api/**` and App-Router `app/**/route.ts`
/// expose HTTP endpoints by location rather than by an in-file call.
fn nextjs_file_route(path: &Path, language: &str) -> Option<(String, String)> {
    if !matches!(language, "javascript" | "typescript") {
        return None;
    }
    let p = path.to_string_lossy().replace('\\', "/");
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(idx) = p.find("/pages/api/") {
        let route = p[idx + "/pages/api".len()..]
            .trim_end_matches(".ts")
            .trim_end_matches(".js")
            .trim_end_matches(".tsx")
            .trim_end_matches(".jsx");
        let route = route.trim_end_matches("/index");
        return Some(("ANY".to_string(), route.to_string()));
    }
    if stem == "route" {
        if let Some(idx) = p.find("/app/") {
            let dir = &p[idx + "/app".len()..];
            let route = dir
                .trim_end_matches("/route.ts")
                .trim_end_matches("/route.js");
            return Some(("ANY".to_string(), route.to_string()));
        }
    }
    None
}

/// Scan `source` for each `(needle, verb)`; for every match parse the route path
/// (first string literal after the needle) and an optional handler identifier.
fn scan(source: &str, needles: &[(&str, &str)]) -> Vec<Hit> {
    let mut hits = vec![];
    for (needle, verb) in needles {
        let mut from = 0;
        while let Some(rel) = source[from..].find(needle) {
            let pos = from + rel;
            from = pos + needle.len();
            // The character before a route token must not be part of a larger
            // identifier (avoids matching `xapp.get(` or `budget `).
            if let Some(prev) = source[..pos].chars().last() {
                if prev.is_alphanumeric() || prev == '_' {
                    continue;
                }
            }
            let last_idx = pos + needle.len() - 1;
            // Parenthesised call DSL/decorator/attribute vs. bare Rails-style
            // `verb '/path'` whose args run to end of line.
            let args: &str = if source.as_bytes().get(last_idx) == Some(&b'(') {
                match balanced_args(source, last_idx) {
                    Some(a) => a,
                    None => continue,
                }
            } else {
                let start = pos + needle.len();
                let end = source[start..]
                    .find('\n')
                    .map(|n| start + n)
                    .unwrap_or(source.len());
                &source[start..end]
            };
            let Some(path) = first_string(args) else {
                continue;
            };
            let handler = handler_ident(args, &path);
            hits.push(Hit {
                verb: verb.to_string(),
                path,
                handler,
                byte: pos,
            });
        }
    }
    hits
}

/// Return the slice between the `(` at `open` and its matching `)`, string-aware.
fn balanced_args(source: &str, open: usize) -> Option<&str> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' | b'`' => quote = Some(c),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&source[open + 1..i]);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn first_string(args: &str) -> Option<String> {
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' || c == b'"' || c == b'`' {
            let end_rel = args[i + 1..].find(c as char)?;
            return Some(args[i + 1..i + 1 + end_rel].to_string());
        }
        i += 1;
    }
    None
}

/// Best-effort handler: the last identifier appearing outside string literals,
/// skipping common non-handler keywords.
fn handler_ident(args: &str, path: &str) -> Option<String> {
    let without_strings = strip_strings(args);
    let mut last: Option<String> = None;
    let mut cur = String::new();
    for ch in without_strings.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            last = Some(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        last = Some(cur);
    }
    match last {
        Some(ref h) if h == path || matches!(h.as_str(), "methods" | "name" | "to" | "as") => None,
        other => other,
    }
}

fn strip_strings(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' | b'`' => quote = Some(c),
                _ => out.push(c as char),
            },
        }
        i += 1;
    }
    out
}

fn line_of(source: &str, byte: usize) -> u32 {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lang: &str, file: &str, src: &str) -> ExtractionResult {
        let mut r = ExtractionResult::empty();
        append(&mut r, Path::new(file), lang, src);
        r
    }

    fn route(r: &ExtractionResult, verb: &str, path: &str) -> bool {
        let label = format!("{} {}", verb, path);
        r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Route && n.name == label)
    }

    fn refs(r: &ExtractionResult, handler: &str) -> bool {
        r.unresolved
            .iter()
            .any(|u| u.target_name == handler && matches!(u.kind, EdgeKind::References))
    }

    #[test]
    fn express_method_dsl() {
        let r = run("javascript", "app.js", "app.get('/users', getUsers);\n");
        assert!(route(&r, "GET", "/users"), "{:?}", names(&r));
        assert!(refs(&r, "getUsers"));
    }

    #[test]
    fn nestjs_decorator() {
        let r = run(
            "typescript",
            "cats.controller.ts",
            "@Get('/cats')\nfindAll() {}\n",
        );
        assert!(route(&r, "GET", "/cats"), "{:?}", names(&r));
    }

    #[test]
    fn gin_method_dsl() {
        let r = run("go", "main.go", "r.GET(\"/ping\", ping)\n");
        assert!(route(&r, "GET", "/ping"), "{:?}", names(&r));
        assert!(refs(&r, "ping"));
    }

    #[test]
    fn laravel_facade_dsl() {
        let r = run(
            "php",
            "web.php",
            "Route::post('/login', 'AuthController@login');\n",
        );
        assert!(route(&r, "POST", "/login"), "{:?}", names(&r));
    }

    #[test]
    fn symfony_attribute() {
        let r = run("php", "Ctrl.php", "#[Route('/sym', methods: ['GET'])]\n");
        assert!(route(&r, "ANY", "/sym"), "{:?}", names(&r));
    }

    #[test]
    fn rails_routes_file_dsl() {
        let r = run(
            "ruby",
            "config/routes.rb",
            "  get '/health', to: 'health#show'\n",
        );
        assert!(route(&r, "GET", "/health"), "{:?}", names(&r));
    }

    #[test]
    fn rails_dsl_ignored_outside_routes_file() {
        // A method named `get` elsewhere must not be treated as a route.
        let r = run("ruby", "user.rb", "  get '/health'\n");
        assert!(!r.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn fastapi_and_flask_decorators() {
        let r = run(
            "python",
            "main.py",
            "@app.get('/items')\n@app.route('/home')\n",
        );
        assert!(route(&r, "GET", "/items"), "{:?}", names(&r));
        assert!(route(&r, "ANY", "/home"));
    }

    #[test]
    fn django_urlconf() {
        let r = run(
            "python",
            "urls.py",
            "    path('admin/', admin.site.urls),\n",
        );
        assert!(route(&r, "ANY", "admin/"), "{:?}", names(&r));
    }

    #[test]
    fn spring_annotation() {
        let r = run(
            "java",
            "Ctrl.java",
            "@GetMapping(\"/api/v1\")\npublic String list() {}\n",
        );
        assert!(route(&r, "GET", "/api/v1"), "{:?}", names(&r));
    }

    #[test]
    fn aspnet_attribute() {
        let r = run(
            "csharp",
            "Ctrl.cs",
            "[HttpGet(\"/v\")]\npublic IActionResult Get() {}\n",
        );
        assert!(route(&r, "GET", "/v"), "{:?}", names(&r));
    }

    #[test]
    fn nextjs_file_convention() {
        let r = run(
            "typescript",
            "/proj/pages/api/users.ts",
            "export default function handler() {}\n",
        );
        assert!(route(&r, "ANY", "/users"), "{:?}", names(&r));
    }

    #[test]
    fn map_get_is_not_mistaken_for_a_route() {
        // `cache.get('key')` is not a router receiver and the path isn't route-like
        // for an unknown receiver, so no route should be produced.
        let r = run("javascript", "x.js", "cache.get('key');\n");
        assert!(
            !r.nodes.iter().any(|n| n.kind == NodeKind::Route),
            "{:?}",
            names(&r)
        );
    }

    fn names(r: &ExtractionResult) -> Vec<&str> {
        r.nodes.iter().map(|n| n.name.as_str()).collect()
    }
}
