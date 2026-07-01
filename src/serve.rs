//! `rusty-graph serve` — a small HTTP server exposing the knowledge graph as a
//! JSON API plus an embedded, dependency-light graph explorer (vis-network from
//! a CDN). Handy for browsing a codebase visually and for ad-hoc integrations.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::db::Db;
use crate::graph::Graph;

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Db>>,
    root: String,
}

pub async fn run(db: Arc<Mutex<Db>>, project_root: PathBuf, port: u16) -> Result<()> {
    let state = AppState {
        db,
        root: project_root.to_string_lossy().to_string(),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/stats", get(stats))
        .route("/api/search", get(search))
        .route("/api/graph", get(graph))
        .route("/api/node/{id}", get(node))
        .route("/api/cycles", get(cycles))
        .route("/api/impact", get(impact))
        .route("/api/path", get(path))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("codegraph explorer on http://{addr}  (Ctrl-C to stop)");
    axum::serve(listener, app).await?;
    Ok(())
}

fn lock(db: &Arc<Mutex<Db>>) -> std::sync::MutexGuard<'_, Db> {
    db.lock().unwrap_or_else(|e| e.into_inner())
}

async fn stats(State(s): State<AppState>) -> impl IntoResponse {
    let db = lock(&s.db);
    match db.stats() {
        Ok(st) => Json(json!({
            "root": s.root,
            "files": st.file_count,
            "nodes": st.node_count,
            "edges": st.edge_count,
        }))
        .into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn search(State(s): State<AppState>, Query(p): Query<SearchParams>) -> impl IntoResponse {
    let db = lock(&s.db);
    match db.smart_search(&p.q, None, p.limit.unwrap_or(30)) {
        Ok(nodes) => Json(nodes).into_response(),
        Err(e) => err(e),
    }
}

async fn node(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let db = lock(&s.db);
    match db.get_node(&id) {
        Ok(Some(n)) => {
            let callers = db.callers(&id, 50).unwrap_or_default();
            let callees = db.callees(&id, 50).unwrap_or_default();
            Json(json!({ "node": n, "callers": callers, "callees": callees })).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct GraphParams {
    #[serde(default)]
    limit: Option<usize>,
}

/// Return the top-ranked subgraph: the highest-centrality nodes and the call
/// edges among them, ready to render.
async fn graph(State(s): State<AppState>, Query(p): Query<GraphParams>) -> impl IntoResponse {
    let db = lock(&s.db);
    let limit = p.limit.unwrap_or(150);
    let nodes = match db.top_nodes_with_rank(limit) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let ids: HashSet<String> = nodes.iter().map(|(n, _)| n.id.clone()).collect();
    let edges: Vec<_> = db
        .call_edges()
        .unwrap_or_default()
        .into_iter()
        .filter(|(s, t)| ids.contains(s) && ids.contains(t))
        .map(|(s, t)| json!({ "from": s, "to": t }))
        .collect();
    let vnodes: Vec<_> = nodes
        .iter()
        .map(|(n, rank)| {
            json!({
                "id": n.id,
                "label": n.name,
                "group": n.kind.as_str(),
                // vis-network scales node size by `value`; rank ∈ [0,1].
                "value": (rank * 100.0).round() as i64 + 1,
                "title": format!("{} ({}:{})", n.qualified_name, n.file_path, n.start_line),
            })
        })
        .collect();
    Json(json!({ "nodes": vnodes, "edges": edges })).into_response()
}

/// Strongly-connected components (call cycles) as lists of node ids, so the UI
/// can highlight circular dependencies.
async fn cycles(State(s): State<AppState>) -> impl IntoResponse {
    let db = lock(&s.db);
    match crate::arch::report(&db, &s.root) {
        Ok(report) => {
            let groups: Vec<Vec<String>> = report
                .cycles
                .iter()
                .map(|c| c.iter().map(|n| n.id.clone()).collect())
                .collect();
            Json(json!({ "cycles": groups })).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct ImpactParams {
    id: String,
    #[serde(default)]
    depth: Option<usize>,
}

/// Transitive callers (blast radius) of a node id, for overlay highlighting.
async fn impact(State(s): State<AppState>, Query(p): Query<ImpactParams>) -> impl IntoResponse {
    let graph = Graph::new(s.db.clone());
    match graph.impact(&p.id, p.depth.unwrap_or(3)) {
        Ok(nodes) => {
            let ids: Vec<String> = nodes.iter().map(|i| i.node.id.clone()).collect();
            Json(json!({ "ids": ids })).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct PathParams {
    from: String,
    to: String,
}

/// A call path between two symbols (by name), as an ordered list of node ids.
async fn path(State(s): State<AppState>, Query(p): Query<PathParams>) -> impl IntoResponse {
    let (from_nodes, to_nodes) = {
        let db = lock(&s.db);
        match (db.find_node_by_name(&p.from), db.find_node_by_name(&p.to)) {
            (Ok(f), Ok(t)) => (f, t),
            (Err(e), _) | (_, Err(e)) => return err(e),
        }
    };
    if from_nodes.is_empty() || to_nodes.is_empty() {
        return Json(json!({ "ids": [] })).into_response();
    }
    let graph = Graph::new(s.db.clone());
    match graph.call_path(&from_nodes[0].id, &to_nodes[0].id) {
        Ok(Some(nodes)) => {
            let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
            Json(json!({ "ids": ids })).into_response()
        }
        Ok(None) => Json(json!({ "ids": [] })).into_response(),
        Err(e) => err(e),
    }
}

fn err(e: anyhow::Error) -> axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8"/>
<title>codegraph explorer</title>
<script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
<style>
  body { margin:0; font-family: ui-monospace, monospace; background:#0d1117; color:#c9d1d9; }
  #bar { padding:8px; background:#161b22; display:flex; gap:8px; align-items:center; }
  #bar input { flex:1; padding:6px; background:#0d1117; color:#c9d1d9; border:1px solid #30363d; border-radius:6px; }
  #net { height: calc(100vh - 46px); }
  #info { position:absolute; right:0; top:46px; width:340px; max-height:70vh; overflow:auto;
          background:#161b22ee; border-left:1px solid #30363d; padding:10px; font-size:12px; }
  button { padding:6px 10px; background:#238636; color:#fff; border:0; border-radius:6px; cursor:pointer; }
  h3 { margin:6px 0; }
  a { color:#58a6ff; cursor:pointer; }
</style>
</head>
<body>
  <div id="bar">
    <strong>codegraph</strong>
    <input id="q" placeholder="search symbols… (enter)"/>
    <button onclick="loadGraph()">Top graph</button>
    <button onclick="showCycles()" title="Highlight circular dependencies">Cycles</button>
    <button onclick="blastSelected()" title="Highlight transitive callers of the selected node">Blast radius</button>
    <input id="pf" placeholder="path from" style="flex:0 0 120px"/>
    <input id="pt" placeholder="path to" style="flex:0 0 120px"/>
    <button onclick="tracePath()">Trace</button>
    <button onclick="resetColors()">Reset</button>
  </div>
  <div id="net"></div>
  <div id="info">Click a node for details.</div>
<script>
let network, nodes, edges, selected=null;
const HL='#f0883e', BASE=undefined;
function draw(data) {
  nodes = new vis.DataSet(data.nodes);
  edges = new vis.DataSet(data.edges);
  const container = document.getElementById('net');
  network = new vis.Network(container, {nodes, edges}, {
    nodes:{shape:'dot', scaling:{min:6,max:40}, font:{color:'#c9d1d9'}},
    edges:{arrows:'to', color:{color:'#30363d'}, smooth:false},
    physics:{stabilization:true, barnesHut:{gravitationalConstant:-8000}}
  });
  network.on('click', async p => {
    if (!p.nodes.length) return;
    selected = p.nodes[0];
    const r = await fetch('/api/node/' + selected); const d = await r.json();
    const cs = (d.callers||[]).map(c=>'<li>'+c.qualified_name+'</li>').join('');
    const ce = (d.callees||[]).map(c=>'<li>'+c.qualified_name+'</li>').join('');
    document.getElementById('info').innerHTML =
      '<h3>'+d.node.qualified_name+'</h3><div>'+d.node.kind+' — '+d.node.file_path+':'+d.node.start_line+'</div>'+
      (d.node.signature?('<pre>'+d.node.signature+'</pre>'):'')+
      '<h3>callers</h3><ul>'+(cs||'<i>none</i>')+'</ul>'+
      '<h3>calls</h3><ul>'+(ce||'<i>none</i>')+'</ul>';
  });
}
function highlight(ids, color) {
  const set = new Set(ids);
  nodes.update(nodes.get().map(n=>({id:n.id, color: set.has(n.id)?color:BASE})));
}
function resetColors(){ if(nodes) nodes.update(nodes.get().map(n=>({id:n.id,color:BASE}))); }
async function loadGraph() {
  const r = await fetch('/api/graph'); draw(await r.json());
}
async function showCycles() {
  const r = await fetch('/api/cycles'); const d = await r.json();
  const ids = [].concat.apply([], d.cycles||[]);
  highlight(ids, HL);
  document.getElementById('info').innerHTML = '<h3>'+(d.cycles||[]).length+' cycle(s)</h3>'+
    (d.cycles||[]).map(c=>'<div>'+c.length+' nodes</div>').join('');
}
async function blastSelected() {
  if(!selected){ alert('Click a node first'); return; }
  const r = await fetch('/api/impact?id='+encodeURIComponent(selected)+'&depth=4'); const d = await r.json();
  highlight((d.ids||[]).concat([selected]), HL);
  document.getElementById('info').innerHTML = '<h3>blast radius: '+(d.ids||[]).length+' callers</h3>';
}
async function tracePath() {
  const from=document.getElementById('pf').value.trim(), to=document.getElementById('pt').value.trim();
  if(!from||!to) return;
  const r = await fetch('/api/path?from='+encodeURIComponent(from)+'&to='+encodeURIComponent(to));
  const d = await r.json();
  if(!(d.ids||[]).length){ document.getElementById('info').innerHTML='<i>no path</i>'; return; }
  highlight(d.ids, HL);
  document.getElementById('info').innerHTML = '<h3>path ('+d.ids.length+')</h3>';
}
async function runSearch() {
  const q = document.getElementById('q').value.trim(); if(!q) return;
  const r = await fetch('/api/search?q='+encodeURIComponent(q)); const list = await r.json();
  draw({nodes:list.map(n=>({id:n.id,label:n.name,group:n.kind,title:n.qualified_name})), edges:[]});
  document.getElementById('info').innerHTML = '<h3>'+list.length+' results</h3>'+
    list.map(n=>'<div>'+n.kind+' '+n.qualified_name+'</div>').join('');
}
document.getElementById('q').addEventListener('keydown', e=>{ if(e.key==='Enter') runSearch(); });
loadGraph();
</script>
</body>
</html>"#;
