use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
    },
    serve_server,
    service::RequestContext,
    transport::stdio,
    RoleServer, ServerHandler,
};
use serde_json::{json, Value};
use tracing::info;

use crate::db::Db;
use crate::graph::{Graph, SourceMap};

pub struct RustyGraphServer {
    db: Arc<Mutex<Db>>,
    project_root: PathBuf,
    source_map: Arc<SourceMap>,
    extra_tools: Vec<String>,
}

impl RustyGraphServer {
    pub fn new(db: Arc<Mutex<Db>>, project_root: PathBuf, extra_tools: Vec<String>) -> Self {
        Self {
            db,
            project_root,
            source_map: Arc::new(SourceMap::new()),
            extra_tools,
        }
    }

    fn ok(text: String) -> CallToolResult {
        CallToolResult::success(vec![Content::text(text)])
    }

    fn err(msg: String) -> CallToolResult {
        CallToolResult::error(vec![Content::text(msg)])
    }

    fn schema(props: serde_json::Value) -> Arc<serde_json::Map<String, Value>> {
        let obj = json!({"type": "object", "properties": props});
        Arc::new(obj.as_object().unwrap().clone())
    }

    fn tool(name: &'static str, desc: &'static str, props: serde_json::Value) -> Tool {
        Tool::new(name, desc, Self::schema(props))
    }

    /// Only `rusty_graph_explore` is always on; the rest must be opted in via
    /// `--tools`/`RUSTY_GRAPH_MCP_TOOLS`. Mirrors what `list_tools` advertises so a
    /// client cannot invoke a tool that was never offered.
    fn is_tool_enabled(&self, name: &str) -> bool {
        name == "rusty_graph_explore" || self.extra_tools.iter().any(|t| t == name)
    }

    fn handle_explore(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let query = args
            .as_ref()
            .and_then(|a| a.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if query.is_empty() {
            return Self::err("query parameter is required".into());
        }
        let graph = Graph::new(self.db.clone());
        let root = self.project_root.to_string_lossy().to_string();
        match graph.explore(query, &root, &self.source_map) {
            Ok(r) => Self::ok(r.format()),
            Err(e) => Self::err(format!("explore error: {}", e)),
        }
    }

    fn handle_search(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let query = args
            .as_ref()
            .and_then(|a| a.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let kind = args
            .as_ref()
            .and_then(|a| a.get("kind"))
            .and_then(|v| v.as_str());
        let limit = args
            .as_ref()
            .and_then(|a| a.get("limit"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;

        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.search_nodes(query, kind, limit) {
            Ok(nodes) => {
                let mut out = String::new();
                for node in &nodes {
                    out.push_str(&format!(
                        "[{}] {} — {}:{}\n",
                        node.kind.as_str(),
                        node.qualified_name,
                        node.file_path,
                        node.start_line,
                    ));
                    if let Some(sig) = &node.signature {
                        out.push_str(&format!("  {}\n", sig));
                    }
                }
                if out.is_empty() {
                    out = format!("No results for \"{}\"", query);
                }
                Self::ok(out)
            }
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_node(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let symbol = args
            .as_ref()
            .and_then(|a| a.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let nodes = match db.find_node_by_name(symbol) {
            Ok(n) => n,
            Err(e) => return Self::err(format!("{}", e)),
        };
        if nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", symbol));
        }
        let mut out = String::new();
        for node in &nodes {
            out.push_str(&format!(
                "[{}] {} ({}:{}-{})\n",
                node.kind.as_str(),
                node.qualified_name,
                node.file_path,
                node.start_line,
                node.end_line
            ));
            if let Some(sig) = &node.signature {
                out.push_str(&format!("  signature: {}\n", sig));
            }
            for (ln, text) in self.source_map.get_lines(
                &node.file_path,
                node.start_line as usize,
                node.end_line as usize,
            ) {
                out.push_str(&format!("{}\t{}\n", ln, text));
            }
            if let Ok(callers) = db.callers(&node.id, 10) {
                if !callers.is_empty() {
                    out.push_str("  callers:\n");
                    for c in &callers {
                        out.push_str(&format!("    - {}\n", c.qualified_name));
                    }
                }
            }
            out.push('\n');
        }
        Self::ok(out)
    }

    fn handle_callers(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let symbol = args
            .as_ref()
            .and_then(|a| a.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = args
            .as_ref()
            .and_then(|a| a.get("limit"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let nodes = match db.find_node_by_name(symbol) {
            Ok(n) => n,
            Err(e) => return Self::err(format!("{}", e)),
        };
        if nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", symbol));
        }
        let mut out = String::new();
        for node in &nodes {
            if let Ok(callers) = db.callers(&node.id, limit) {
                out.push_str(&format!("Callers of {}:\n", node.qualified_name));
                for c in &callers {
                    out.push_str(&format!(
                        "  - {} ({}:{})\n",
                        c.qualified_name, c.file_path, c.start_line
                    ));
                }
            }
        }
        Self::ok(out)
    }

    fn handle_callees(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let symbol = args
            .as_ref()
            .and_then(|a| a.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = args
            .as_ref()
            .and_then(|a| a.get("limit"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let nodes = match db.find_node_by_name(symbol) {
            Ok(n) => n,
            Err(e) => return Self::err(format!("{}", e)),
        };
        if nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", symbol));
        }
        let mut out = String::new();
        for node in &nodes {
            if let Ok(callees) = db.callees(&node.id, limit) {
                out.push_str(&format!("Callees of {}:\n", node.qualified_name));
                for c in &callees {
                    out.push_str(&format!(
                        "  - {} ({}:{})\n",
                        c.qualified_name, c.file_path, c.start_line
                    ));
                }
            }
        }
        Self::ok(out)
    }

    fn handle_path(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let get = |k: &str| {
            args.as_ref()
                .and_then(|a| a.get(k))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (from, to) = (get("from"), get("to"));
        if from.is_empty() || to.is_empty() {
            return Self::err("both 'from' and 'to' parameters are required".into());
        }

        let (from_nodes, to_nodes) = {
            let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
            match (db.find_node_by_name(&from), db.find_node_by_name(&to)) {
                (Ok(f), Ok(t)) => (f, t),
                (Err(e), _) | (_, Err(e)) => return Self::err(format!("{}", e)),
            }
        };
        if from_nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", from));
        }
        if to_nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", to));
        }

        let graph = Graph::new(self.db.clone());
        match graph.call_path(&from_nodes[0].id, &to_nodes[0].id) {
            Ok(Some(nodes)) => {
                let mut out = format!("Call path {} → {}:\n", from, to);
                for (i, n) in nodes.iter().enumerate() {
                    out.push_str(&format!("{}{}\n", "  ".repeat(i + 1), n.qualified_name));
                }
                Self::ok(out)
            }
            Ok(None) => Self::ok(format!("No call path from {} to {}", from, to)),
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_impact(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let symbol = args
            .as_ref()
            .and_then(|a| a.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let depth = args
            .as_ref()
            .and_then(|a| a.get("depth"))
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let nodes = {
            let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
            match db.find_node_by_name(symbol) {
                Ok(n) => n,
                Err(e) => return Self::err(format!("{}", e)),
            }
        };
        if nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", symbol));
        }
        let graph = Graph::new(self.db.clone());
        let mut out = String::new();
        for node in &nodes {
            match graph.impact(&node.id, depth) {
                Ok(impacts) => {
                    out.push_str(&format!(
                        "Impact of {} (depth {}):\n",
                        node.qualified_name, depth
                    ));
                    for imp in &impacts {
                        out.push_str(&format!(
                            "  [{}] {} ({}:{})\n",
                            imp.depth,
                            imp.node.qualified_name,
                            imp.node.file_path,
                            imp.node.start_line
                        ));
                    }
                    if impacts.is_empty() {
                        out.push_str("  No transitive callers found.\n");
                    }
                }
                Err(e) => out.push_str(&format!("Error: {}\n", e)),
            }
        }
        Self::ok(out)
    }

    fn handle_status(&self, _: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.stats() {
            Ok(s) => Self::ok(format!(
                "CodeGraph: {} files, {} nodes, {} edges\nroot: {}",
                s.file_count,
                s.node_count,
                s.edge_count,
                self.project_root.display()
            )),
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_files(&self, _: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.all_file_hashes() {
            Ok(files) => {
                let root = self.project_root.to_string_lossy().to_string();
                let out: String = files
                    .iter()
                    .map(|(p, _)| format!("{}\n", p.strip_prefix(&root).unwrap_or(p)))
                    .collect();
                Self::ok(out)
            }
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_context(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let query = args
            .as_ref()
            .and_then(|a| a.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if query.is_empty() {
            return Self::err("query parameter is required".into());
        }
        let budget = args
            .as_ref()
            .and_then(|a| a.get("budget"))
            .and_then(|v| v.as_u64())
            .unwrap_or(crate::context::DEFAULT_BUDGET_TOKENS as u64) as usize;
        match crate::context::build(&self.db, &self.source_map, query, budget) {
            Ok(pack) => Self::ok(pack.format()),
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_tests(&self, args: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let symbol = args
            .as_ref()
            .and_then(|a| a.get("symbol"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let depth = args
            .as_ref()
            .and_then(|a| a.get("depth"))
            .and_then(|v| v.as_u64())
            .unwrap_or(6) as usize;
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let nodes = match db.find_node_by_name(symbol) {
            Ok(n) => n,
            Err(e) => return Self::err(format!("{}", e)),
        };
        if nodes.is_empty() {
            return Self::ok(format!("Symbol not found: {}", symbol));
        }
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        match crate::testmap::tests_for_nodes(&db, &ids, depth) {
            Ok(tests) if tests.is_empty() => Self::ok(format!("No tests cover {}", symbol)),
            Ok(tests) => {
                let mut out = format!("{} test(s) cover {}:\n", tests.len(), symbol);
                for t in &tests {
                    out.push_str(&format!("  - {} ({})\n", t.qualified_name, t.file_path));
                }
                Self::ok(out)
            }
            Err(e) => Self::err(format!("{}", e)),
        }
    }

    fn handle_arch(&self, _: Option<serde_json::Map<String, Value>>) -> CallToolResult {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match crate::arch::report(&db, &self.project_root.to_string_lossy()) {
            Ok(report) => Self::ok(report.format()),
            Err(e) => Self::err(format!("{}", e)),
        }
    }
}

impl ServerHandler for RustyGraphServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let server_info =
            Implementation::new("codegraph", env!("CARGO_PKG_VERSION")).with_title("CodeGraph");
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let mut tools = vec![Self::tool(
            "rusty_graph_explore",
            "Search for symbols (full-text keyword match) and return source with \
                 call paths and blast radius. Use as the primary tool to understand \
                 code structure.",
            json!({
                "query": {
                    "type": "string",
                    "description": "Symbol name, file path, or keywords (full-text matched)"
                }
            }),
        )];

        for name in &self.extra_tools {
            let t = match name.as_str() {
                "rusty_graph_search" => Self::tool(
                    "rusty_graph_search",
                    "Full-text search for symbols by name",
                    json!({
                        "query": {"type":"string"},
                        "kind": {"type":"string","description":"function|class|struct|method|..."},
                        "limit": {"type":"integer","default":20}
                    }),
                ),
                "rusty_graph_node" => Self::tool(
                    "rusty_graph_node",
                    "Get source and callers of a specific symbol",
                    json!({"symbol": {"type":"string"}}),
                ),
                "rusty_graph_callers" => Self::tool(
                    "rusty_graph_callers",
                    "List what calls a symbol",
                    json!({"symbol":{"type":"string"},"limit":{"type":"integer","default":20}}),
                ),
                "rusty_graph_callees" => Self::tool(
                    "rusty_graph_callees",
                    "List what a symbol calls",
                    json!({"symbol":{"type":"string"},"limit":{"type":"integer","default":20}}),
                ),
                "rusty_graph_impact" => Self::tool(
                    "rusty_graph_impact",
                    "Transitive blast radius of a symbol change",
                    json!({"symbol":{"type":"string"},"depth":{"type":"integer","default":5}}),
                ),
                "rusty_graph_path" => Self::tool(
                    "rusty_graph_path",
                    "Find a call path from one symbol to another",
                    json!({"from":{"type":"string"},"to":{"type":"string"}}),
                ),
                "rusty_graph_status" => Self::tool("rusty_graph_status", "Index statistics", json!({})),
                "rusty_graph_files" => {
                    Self::tool("rusty_graph_files", "List all indexed files", json!({}))
                }
                "rusty_graph_context" => Self::tool(
                    "rusty_graph_context",
                    "Assemble a token-budgeted context pack: the smallest ranked set of \
                     symbols + source that answers a task, with call-graph dependencies \
                     pulled in. The most token-efficient way to load code into context.",
                    json!({
                        "query": {"type":"string","description":"Task or question"},
                        "budget": {"type":"integer","default":8000,"description":"Token budget"}
                    }),
                ),
                "rusty_graph_tests" => Self::tool(
                    "rusty_graph_tests",
                    "List the tests that transitively cover a symbol — the tests worth \
                     running after changing it",
                    json!({"symbol":{"type":"string"},"depth":{"type":"integer","default":6}}),
                ),
                "rusty_graph_arch" => Self::tool(
                    "rusty_graph_arch",
                    "Architecture report: circular dependencies, hotspots, likely dead \
                     code, and cross-layer coupling",
                    json!({}),
                ),
                _ => continue,
            };
            tools.push(t);
        }

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if !self.is_tool_enabled(request.name.as_ref()) {
            return Ok(Self::err(format!("Tool not enabled: {}", request.name)));
        }
        let args = request.arguments;
        Ok(match request.name.as_ref() {
            "rusty_graph_explore" => self.handle_explore(args),
            "rusty_graph_search" => self.handle_search(args),
            "rusty_graph_node" => self.handle_node(args),
            "rusty_graph_callers" => self.handle_callers(args),
            "rusty_graph_callees" => self.handle_callees(args),
            "rusty_graph_impact" => self.handle_impact(args),
            "rusty_graph_path" => self.handle_path(args),
            "rusty_graph_status" => self.handle_status(args),
            "rusty_graph_files" => self.handle_files(args),
            "rusty_graph_context" => self.handle_context(args),
            "rusty_graph_tests" => self.handle_tests(args),
            "rusty_graph_arch" => self.handle_arch(args),
            other => Self::err(format!("Unknown tool: {}", other)),
        })
    }
}

pub async fn run_mcp_server(
    db: Arc<Mutex<Db>>,
    project_root: PathBuf,
    extra_tools: Vec<String>,
) -> Result<()> {
    info!("Starting MCP server for {}", project_root.display());
    let server = RustyGraphServer::new(db, project_root, extra_tools);
    let running = serve_server(server, stdio()).await?;
    running.waiting().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(extra: &[&str]) -> RustyGraphServer {
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        RustyGraphServer::new(
            db,
            PathBuf::from("/tmp/proj"),
            extra.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn explore_is_always_enabled() {
        let s = server(&[]);
        assert!(s.is_tool_enabled("rusty_graph_explore"));
    }

    #[test]
    fn extra_tools_must_be_opted_in() {
        let s = server(&[]);
        assert!(!s.is_tool_enabled("rusty_graph_search"));

        let s = server(&["rusty_graph_search"]);
        assert!(s.is_tool_enabled("rusty_graph_search"));
        assert!(!s.is_tool_enabled("rusty_graph_impact"));
    }

    #[test]
    fn unknown_tools_are_rejected() {
        let s = server(&["rusty_graph_search"]);
        assert!(!s.is_tool_enabled("codegraph_delete_everything"));
    }
}
