use anyhow::Result;
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::db::Db;
use crate::extract::{extractor_for, ExtractionResult};
use crate::types::{
    detect_language, is_ignored_path, refine_language, Edge, EdgeKind, FileRecord, Node, NodeKind,
    Provenance,
};

/// Extract the file stem from an absolute or relative path string.
/// E.g. `/src/utils.py` → `"utils"`, `Button.tsx` → `"Button"`.
fn file_stem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => base,
    }
}

/// Return whether `stem` appears as a path segment inside `import_text`.
/// Works for most styles: `"from utils import foo"`, `"use crate::utils::foo"`,
/// `"./utils"`, `"import com.example.Utils;"`.
fn stem_in_import(stem: &str, import_text: &str) -> bool {
    if stem.is_empty() {
        return false;
    }
    import_text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|seg| seg.eq_ignore_ascii_case(stem))
}

/// Given a set of cross-file candidates and the source file's import nodes,
/// return only those candidates whose file stem appears in at least one import.
/// Falls back to all candidates if no import evidence narrows the set.
fn import_filtered(candidates: &[Node], db: &Db, source_file: &str) -> Vec<Node> {
    let import_names = match db.import_names_for_file(source_file) {
        Ok(names) => names,
        Err(_) => return candidates.to_vec(),
    };
    if import_names.is_empty() {
        return candidates.to_vec();
    }
    let filtered: Vec<Node> = candidates
        .iter()
        .filter(|c| {
            let stem = file_stem(&c.file_path);
            import_names.iter().any(|imp| stem_in_import(stem, imp))
        })
        .cloned()
        .collect();
    if filtered.is_empty() {
        candidates.to_vec()
    } else {
        filtered
    }
}

/// Reduce a container's name to a comparable type token. Handles both plain
/// type nodes (`Config`) and Rust impl-block labels (`impl Db`,
/// `impl Trait for Foo`, `impl Builder<'a>`) by taking the last whitespace
/// token, then dropping any generics and path prefix.
fn type_token(name: &str) -> &str {
    let last = name.split_whitespace().next_back().unwrap_or(name);
    let last = last.split('<').next().unwrap_or(last);
    last.rsplit("::").next().unwrap_or(last)
}

/// Node kinds a reference of the given edge kind may legitimately point at.
/// `None` means any kind is acceptable (e.g. generic `references`).
fn compatible_target_kinds(kind: &EdgeKind) -> Option<Vec<NodeKind>> {
    match kind {
        EdgeKind::Calls => Some(vec![NodeKind::Function, NodeKind::Method]),
        EdgeKind::Extends => Some(vec![
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Enum,
            NodeKind::TypeAlias,
        ]),
        EdgeKind::Implements => Some(vec![
            NodeKind::Trait,
            NodeKind::Interface,
            NodeKind::Protocol,
        ]),
        EdgeKind::Instantiates => Some(vec![
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Enum,
        ]),
        _ => None,
    }
}

pub struct Indexer {
    db: Arc<Mutex<Db>>,
    project_root: PathBuf,
    config: Config,
}

impl Indexer {
    pub fn new(db: Arc<Mutex<Db>>, project_root: PathBuf) -> Self {
        let config = Config::load(&project_root);
        Self {
            db,
            project_root,
            config,
        }
    }

    /// Lock the shared database, recovering the guard even if another thread
    /// panicked while holding it. Poisoning should not take down the whole
    /// process (especially the long-lived MCP/watch servers).
    fn lock_db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Index the project. With `force`, every file is re-parsed; otherwise only
    /// files whose content hash changed (or that are new) are re-parsed. In both
    /// cases files that have disappeared from disk are pruned, and the call graph
    /// is recomputed from the full reference set so cross-file edges stay
    /// consistent.
    pub fn index_all(&self, force: bool, quiet: bool) -> Result<IndexStats> {
        let files = self.collect_files()?;
        let current: HashSet<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let known: HashMap<String, String> = {
            let db = self.lock_db();
            db.all_file_hashes()?.into_iter().collect()
        };

        let targets: Vec<PathBuf> = if force {
            files.clone()
        } else {
            files
                .iter()
                .filter(|path| self.is_changed(path, &known))
                .cloned()
                .collect()
        };

        if !quiet {
            info!(
                "Indexing {} of {} files in {}{}",
                targets.len(),
                files.len(),
                self.project_root.display(),
                if force { " (forced)" } else { "" }
            );
        }

        // Read and parse each target exactly once in parallel; carry the hash
        // and byte size forward so the serial DB pass needs no further file I/O.
        let results: Vec<ParsedFile> = targets
            .par_iter()
            .filter_map(|path| self.parse_file(path))
            .collect();

        let reindexed = results.len();
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

        {
            let db = self.lock_db();
            db.transaction(|| {
                for parsed in &results {
                    self.write_result(&db, parsed, now)?;
                }
                // Prune files that no longer exist on disk.
                for known_path in known.keys() {
                    if !current.contains(known_path) {
                        debug!("Pruning deleted file: {}", known_path);
                        db.delete_file(known_path)?;
                    }
                }
                self.resolve_refs(&db)?;
                Ok(())
            })?;
        }

        let totals = self.lock_db().stats()?;
        Ok(IndexStats {
            files: reindexed,
            nodes: totals.node_count,
            edges: totals.edge_count,
        })
    }

    /// Incremental sync: re-index only changed files and prune deleted ones.
    pub fn sync(&self) -> Result<IndexStats> {
        self.index_all(false, true)
    }

    /// Read, language-detect, and extract a single file. Returns `None` for
    /// unreadable/unsupported/oversized files (logged as warnings).
    fn parse_file(&self, path: &Path) -> Option<ParsedFile> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Cannot read {}: {}", path.display(), e);
                return None;
            }
        };
        if content.len() as u64 > self.config.max_file_size {
            debug!("Skipping large file: {}", path.display());
            return None;
        }
        let lang = refine_language(detect_language(path)?, path, &content);
        if !self.config.language_enabled(lang) {
            return None;
        }
        let extractor = extractor_for(lang)?;
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let size = content.len() as u64;
        match extractor.extract(path, &content) {
            Ok(mut result) => {
                crate::extract::routes::append(&mut result, path, lang, &content);
                let (parse_errors, parse_missing) =
                    crate::extract::parse_diagnostics(lang, &content);
                Some(ParsedFile {
                    path: path.to_path_buf(),
                    lang: lang.to_string(),
                    hash,
                    size,
                    result,
                    parse_errors,
                    parse_missing,
                })
            }
            Err(e) => {
                warn!("Extraction failed for {}: {}", path.display(), e);
                None
            }
        }
    }

    fn is_changed(&self, path: &Path, known: &HashMap<String, String>) -> bool {
        let content = match std::fs::read(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let hash = blake3::hash(&content).to_hex().to_string();
        let key = path.to_string_lossy().to_string();
        known.get(&key).map(|h| h != &hash).unwrap_or(true)
    }

    /// Persist one file's extraction result: file record, fresh nodes,
    /// `contains` edges, and unresolved references (stale rows for the file are
    /// cleared first). Reference resolution is done separately, once.
    fn write_result(&self, db: &Db, parsed: &ParsedFile, now: i64) -> Result<()> {
        let file_path = parsed.path.to_string_lossy().to_string();
        db.upsert_file(&FileRecord {
            id: Node::new_id(&file_path, &file_path),
            path: file_path.clone(),
            language: parsed.lang.clone(),
            content_hash: parsed.hash.clone(),
            size: parsed.size,
            last_indexed: now,
        })?;
        db.delete_nodes_for_file(&file_path)?;
        db.delete_edges_for_file(&file_path)?;

        for node in &parsed.result.nodes {
            db.upsert_node(node)?;
        }
        for edge in &parsed.result.edges {
            db.upsert_edge(edge)?;
        }
        for uref in &parsed.result.unresolved {
            db.insert_unresolved_ref(uref)?;
        }
        if parsed.parse_errors > 0 || parsed.parse_missing > 0 {
            db.update_file_diagnostics(
                &file_path,
                parsed.parse_errors as i64,
                parsed.parse_missing as i64,
            )?;
        }
        Ok(())
    }

    /// Apply a batch of file changes atomically: (re)index `updated`, drop
    /// `removed`, then incrementally re-resolve only affected references.
    /// Used by the watcher so a burst of saves costs a single resolve pass
    /// over just the changed-file subset rather than the whole graph.
    pub fn apply_changes(&self, updated: &[PathBuf], removed: &[PathBuf]) -> Result<()> {
        let parsed: Vec<ParsedFile> = updated.iter().filter_map(|p| self.parse_file(p)).collect();
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

        let changed_files: Vec<String> = parsed
            .iter()
            .map(|pf| pf.path.to_string_lossy().into_owned())
            .chain(removed.iter().map(|p| p.to_string_lossy().into_owned()))
            .collect();

        let db = self.lock_db();
        db.transaction(|| {
            for pf in &parsed {
                self.write_result(&db, pf, now)?;
            }
            for path in removed {
                db.delete_file(&path.to_string_lossy())?;
            }
            self.resolve_refs_for_files(&db, &changed_files)?;
            Ok(())
        })
    }

    /// Recompute every derived edge from the full `unresolved_refs` table.
    /// Called by `index_all` and after a full re-index.
    fn resolve_refs(&self, db: &Db) -> Result<()> {
        db.delete_derived_edges()?;
        let refs = db.all_unresolved_refs()?;
        self.emit_edges(db, &refs)?;
        self.recompute_ranks(db)?;
        self.recompute_embeddings(db)
    }

    /// Recompute local semantic embeddings for every node. Done on full resolves
    /// only (the vector index can lag a save or two without harm), keeping
    /// natural-language search fresh.
    fn recompute_embeddings(&self, db: &Db) -> Result<()> {
        let nodes = db.all_nodes()?;
        let rows: Vec<(String, Vec<f32>)> = nodes
            .iter()
            .map(|n| (n.id.clone(), crate::embed::embed_node(n)))
            .collect();
        db.replace_embeddings(&rows)
    }

    /// Recompute PageRank centrality over the call graph and persist it. Done on
    /// full resolves (index/init/sync); incremental watch updates leave ranks
    /// slightly stale until the next full pass, which is an acceptable trade for
    /// fast saves.
    fn recompute_ranks(&self, db: &Db) -> Result<()> {
        let node_ids = db.all_node_ids()?;
        let edges = db.call_edges()?;
        let ranks = crate::graph::pagerank(&node_ids, &edges, 0.85, 20);
        db.set_node_ranks(&ranks)?;
        Ok(())
    }

    /// Incrementally re-resolve only the refs affected by changes to
    /// `changed_files`. Re-resolves refs that *originate* in those files and
    /// refs from anywhere that *target* names defined in those files.
    fn resolve_refs_for_files(&self, db: &Db, changed_files: &[String]) -> Result<()> {
        if changed_files.is_empty() {
            return Ok(());
        }
        // Remove stale derived edges: outgoing from changed files and incoming
        // to nodes in changed files (their definitions may have changed).
        db.delete_derived_edges_for_files(changed_files)?;
        db.delete_derived_edges_to_files(changed_files)?;

        // Collect the two ref sets: sourced in changed files, and targeting
        // names that were (re)defined in changed files.
        let mut refs = db.unresolved_refs_for_files(changed_files)?;
        let changed_names = db.node_names_in_files(changed_files)?;
        let targeting = db.unresolved_refs_targeting_names(&changed_names)?;

        // Deduplicate by (source_node_id, target_name, kind) before emitting.
        let seen: HashSet<String> = refs
            .iter()
            .map(|r| format!("{}|{}|{}", r.source_node_id, r.target_name, r.kind.as_str()))
            .collect();
        for r in targeting {
            let key = format!("{}|{}|{}", r.source_node_id, r.target_name, r.kind.as_str());
            if !seen.contains(&key) {
                refs.push(r);
            }
        }

        self.emit_edges(db, &refs)
    }

    /// Resolve a list of unresolved refs into edges and write them to the DB.
    /// Resolution is tiered and each emitted edge carries a confidence score so
    /// consumers can rank or filter. Tiers, best first:
    ///   1. Unique candidate (after kind filter)          → 1.00
    ///   2. Receiver/qualifier type match                  → 0.98
    ///   3. Same enclosing type (method on own class)     → 0.97
    ///   4. Same source file                               → 0.90
    ///   5. Method inherited from a known base type        → 0.85
    ///   6. Import-narrowed to a single file               → 0.80
    ///   7. Import-narrowed but still ambiguous            → 0.60
    ///   8. Ambiguous global match (emit all, low trust)   → 0.40
    ///
    /// Authoritative type hints (`!Type`) with no matching candidate are skipped.
    fn emit_edges(&self, db: &Db, refs: &[crate::types::UnresolvedRef]) -> Result<()> {
        let mut resolved = 0usize;

        for uref in refs {
            let mut candidates = db.find_node_by_name(&uref.target_name)?;
            if let Some(kinds) = compatible_target_kinds(&uref.kind) {
                candidates.retain(|n| kinds.contains(&n.kind));
            }
            if candidates.is_empty() {
                continue;
            }

            let (targets, confidence) = self.choose_targets(db, uref, candidates)?;
            for target in targets {
                let edge = Edge {
                    id: Edge::new_id(&uref.source_node_id, &target.id, &uref.kind),
                    source: uref.source_node_id.clone(),
                    target: target.id,
                    kind: uref.kind.clone(),
                    provenance: Provenance::TreeSitter,
                    metadata: None,
                };
                db.upsert_edge_scored(&edge, confidence)?;
                resolved += 1;
            }
        }

        debug!("Resolved {} references", resolved);
        Ok(())
    }

    /// Pick the most likely target(s) for one reference and the confidence to
    /// attach. See `emit_edges` for the tier definitions.
    fn choose_targets(
        &self,
        db: &Db,
        uref: &crate::types::UnresolvedRef,
        candidates: Vec<Node>,
    ) -> Result<(Vec<Node>, f32)> {
        // Type-directed resolution: if the reference carries a receiver/qualifier
        // type, prefer candidates whose enclosing type matches it. An
        // authoritative hint (`!Type`, from an explicit `Type::method()`
        // qualifier) means we *know* the type, so when nothing matches we drop
        // the edge rather than guess — this is what stops `Connection::open()`
        // mis-linking to a same-named `Db::open()`, or `Vec::new()` to any
        // `new`. Advisory hints (inferred receiver types, `self`) only narrow.
        if let Some(raw) = &uref.receiver_hint {
            let authoritative = raw.starts_with('!');
            let hint = raw.trim_start_matches('!');
            let want: Option<String> = if hint == "Self" {
                db.container_of(&uref.source_node_id)?
                    .map(|t| type_token(&t.name).to_string())
            } else {
                Some(hint.to_string())
            };
            if let Some(want) = want {
                let typed: Vec<Node> = candidates
                    .iter()
                    .filter(|c| {
                        db.container_of(&c.id)
                            .ok()
                            .flatten()
                            .is_some_and(|t| type_token(&t.name) == want)
                    })
                    .cloned()
                    .collect();
                if !typed.is_empty() {
                    return Ok((typed, 0.98));
                }
                if authoritative {
                    return Ok((vec![], 0.0));
                }
            }
        }

        if candidates.len() == 1 {
            return Ok((candidates, 1.0));
        }

        // Inheritance-aware resolution for calls: prefer a method on the
        // caller's own type, then one inherited from a declared base type.
        if matches!(uref.kind, EdgeKind::Calls) {
            if let Some(caller_type) = db.container_of(&uref.source_node_id)? {
                let same_type: Vec<Node> = candidates
                    .iter()
                    .filter(|c| {
                        db.container_of(&c.id)
                            .ok()
                            .flatten()
                            .is_some_and(|t| t.id == caller_type.id)
                    })
                    .cloned()
                    .collect();
                if !same_type.is_empty() {
                    return Ok((same_type, 0.97));
                }

                let bases = db.base_type_names(&caller_type.id)?;
                if !bases.is_empty() {
                    let inherited: Vec<Node> = candidates
                        .iter()
                        .filter(|c| {
                            db.container_of(&c.id)
                                .ok()
                                .flatten()
                                .is_some_and(|t| bases.contains(&t.name))
                        })
                        .cloned()
                        .collect();
                    if !inherited.is_empty() {
                        return Ok((inherited, 0.85));
                    }
                }
            }
        }

        let same_file: Vec<Node> = candidates
            .iter()
            .filter(|n| n.file_path == uref.file_path)
            .cloned()
            .collect();
        if !same_file.is_empty() {
            return Ok((same_file, 0.90));
        }

        let imp = import_filtered(&candidates, db, &uref.file_path);
        match imp.len() {
            1 => Ok((imp, 0.80)),
            n if n < candidates.len() => Ok((imp, 0.60)),
            _ => Ok((candidates, 0.40)),
        }
    }

    fn collect_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = vec![];
        let mut seen: HashSet<PathBuf> = HashSet::new();
        self.walk_root(&self.project_root, &mut files, &mut seen)?;

        // Also index any configured external roots (deps/stdlib) so calls into
        // them resolve. Relative roots are taken against the project root.
        for extra in &self.config.extra_roots {
            let root = {
                let p = Path::new(extra);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.project_root.join(p)
                }
            };
            if root.exists() {
                self.walk_root(&root, &mut files, &mut seen)?;
            } else {
                warn!("extra_root does not exist, skipping: {}", root.display());
            }
        }
        Ok(files)
    }

    fn walk_root(
        &self,
        root: &Path,
        files: &mut Vec<PathBuf>,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .add_custom_ignore_filename(".rusty-graphignore")
            .build();

        for entry in walker {
            let entry = entry?;
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.into_path();
            if is_ignored_path(&path) {
                continue;
            }
            match detect_language(&path) {
                Some(lang) if self.config.language_enabled(lang) => {}
                _ => continue,
            }
            let meta = std::fs::metadata(&path)?;
            if meta.len() > self.config.max_file_size {
                continue;
            }
            if seen.insert(path.clone()) {
                files.push(path);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct IndexStats {
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
}

/// A single file's parse output, carried from the parallel parse stage to the
/// serial DB-write stage so files are read and parsed exactly once.
struct ParsedFile {
    path: PathBuf,
    lang: String,
    hash: String,
    size: u64,
    result: ExtractionResult,
    parse_errors: u32,
    parse_missing: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeKind;

    fn new_indexer(root: &Path) -> (Arc<Mutex<Db>>, Indexer) {
        let db = Arc::new(Mutex::new(Db::open_memory().unwrap()));
        let indexer = Indexer::new(db.clone(), root.to_path_buf());
        (db, indexer)
    }

    #[test]
    fn type_token_extracts_impl_target() {
        assert_eq!(type_token("impl Connection"), "Connection");
        assert_eq!(type_token("impl Trait for Foo"), "Foo");
        assert_eq!(type_token("std::vec::Vec"), "Vec");
    }

    #[test]
    fn qualified_call_resolves_to_matching_type_not_same_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub struct Db;\n\
             pub struct Connection;\n\
             impl Db { pub fn open() -> Self { Db } }\n\
             impl Connection { pub fn open() -> Self { Connection } }\n\
             pub fn run() {\n\
                 let _ = Db::open();\n\
                 let _ = Connection::open();\n\
             }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        let open_callees: Vec<_> = callees.iter().filter(|n| n.name == "open").collect();
        assert_eq!(
            open_callees.len(),
            2,
            "each qualified call should resolve to exactly one open(): {callees:?}"
        );
        let containers: Vec<String> = open_callees
            .iter()
            .filter_map(|n| db.container_of(&n.id).ok().flatten().map(|c| c.name))
            .collect();
        assert!(
            containers.iter().any(|n| n.contains("Db")),
            "Db::open should resolve to Db's open: {containers:?}"
        );
        assert!(
            containers.iter().any(|n| n.contains("Connection")),
            "Connection::open should resolve to Connection's open: {containers:?}"
        );
    }

    #[test]
    fn authoritative_hint_skips_unknown_std_type() {
        // `Vec::new()` must not fan out to a same-named project function.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub struct Widget;\n\
             impl Widget { pub fn new() -> Self { Widget } }\n\
             pub fn run() { let _ = Vec::new(); }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert!(
            !callees.iter().any(|n| n.name == "new"),
            "Vec::new should not resolve to Widget::new: {callees:?}"
        );
    }

    #[test]
    fn java_qualified_call_resolves_to_matching_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("App.java"),
            "class Db { static void open() {} }\n\
             class Connection { static void open() {} }\n\
             class App { void run() { Db.open(); Connection.open(); } }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        let open_callees: Vec<_> = callees.iter().filter(|n| n.name == "open").collect();
        assert_eq!(open_callees.len(), 2, "expected two distinct open() targets");
        let containers: Vec<String> = open_callees
            .iter()
            .filter_map(|n| db.container_of(&n.id).ok().flatten().map(|c| c.name))
            .collect();
        assert!(containers.iter().any(|n| n == "Db"), "{containers:?}");
        assert!(containers.iter().any(|n| n == "Connection"), "{containers:?}");
    }

    #[test]
    fn csharp_qualified_call_resolves_to_matching_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("App.cs"),
            "class Db { public static void Open() {} }\n\
             class Connection { public static void Open() {} }\n\
             class App { void Run() { Db.Open(); Connection.Open(); } }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("Run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        let open_callees: Vec<_> = callees.iter().filter(|n| n.name == "Open").collect();
        assert_eq!(open_callees.len(), 2, "expected two Open() targets");
        let containers: Vec<String> = open_callees
            .iter()
            .filter_map(|n| db.container_of(&n.id).ok().flatten().map(|c| c.name))
            .collect();
        assert!(containers.iter().any(|n| n == "Db"), "{containers:?}");
        assert!(containers.iter().any(|n| n == "Connection"), "{containers:?}");
    }

    #[test]
    fn kotlin_qualified_call_resolves_to_matching_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Db.kt"),
            "class Db {\n  fun open() {}\n  fun run() { this.open() }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Connection.kt"),
            "class Connection {\n  fun open() {}\n  fun run() { this.open() }\n}\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        for class in ["Db", "Connection"] {
            let run = db
                .find_node_by_name("run")
                .unwrap()
                .into_iter()
                .find(|n| {
                    db.container_of(&n.id)
                        .ok()
                        .flatten()
                        .is_some_and(|c| c.name == class)
                })
                .unwrap_or_else(|| panic!("run() in {class}"));
            let callees = db.callees(&run.id, 10).unwrap();
            let open = callees.iter().find(|n| n.name == "open").expect("open callee");
            let container = db.container_of(&open.id).ok().flatten().expect("container");
            assert_eq!(
                container.name, class,
                "{class}::run should call {class}::open, not {:?}",
                container.name
            );
        }
    }

    #[test]
    fn calls_resolve_to_functions_not_same_named_fields() {
        // `value` is both a struct field and a function; a call to value() must
        // link to the function only, never the field.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub struct Holder { value: i32 }\n\
             pub fn value() -> i32 { 0 }\n\
             pub fn run() -> i32 { value() }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert!(
            callees.iter().all(|n| n.kind != NodeKind::Field),
            "a call must never resolve to a field"
        );
        assert!(
            callees
                .iter()
                .any(|n| n.name == "value" && n.kind == NodeKind::Function),
            "the call should resolve to the function named value"
        );
    }

    #[test]
    fn calls_prefer_same_file_definition() {
        // A local `helper` shadows a same-named helper in another file.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("other.rs"),
            "pub fn helper() -> i32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "fn helper() -> i32 { 2 }\npub fn run() -> i32 { helper() }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let main_path = dir.path().join("main.rs").to_string_lossy().to_string();
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert_eq!(callees.len(), 1, "should resolve to a single helper");
        assert_eq!(
            callees[0].file_path, main_path,
            "prefer the same-file helper"
        );
    }

    #[test]
    fn index_all_resolves_cross_file_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn helper() -> i32 { 42 }\n").unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            "pub fn run() -> i32 { helper() }\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        let stats = indexer.index_all(true, true).unwrap();
        assert_eq!(stats.files, 2);

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        assert_eq!(run.len(), 1, "exactly one run() symbol");
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert!(
            callees.iter().any(|n| n.name == "helper"),
            "cross-file call run() -> helper() should be resolved"
        );
    }

    #[test]
    fn reindexing_unchanged_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "pub fn a() {}\n").unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();
        let before = db.lock().unwrap().stats().unwrap().node_count;

        // Same content => a non-forced index must skip the file entirely.
        let stats = indexer.index_all(false, true).unwrap();
        assert_eq!(stats.files, 0, "unchanged file should not be re-indexed");
        let after = db.lock().unwrap().stats().unwrap().node_count;
        assert_eq!(before, after);
    }

    #[test]
    fn apply_changes_builds_edges_and_refreshes_search() {
        // The watcher routes single-file edits through apply_changes, which must
        // resolve references into edges and keep FTS (trigger-maintained) current.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn helper() {}\npub fn run() { helper() }\n").unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer
            .apply_changes(std::slice::from_ref(&f), &[])
            .unwrap();

        let db = db.lock().unwrap();
        // FTS search reflects the new file.
        let found = db.search_nodes("run", None, 10).unwrap();
        assert!(
            found.iter().any(|n| n.name == "run"),
            "FTS should find run()"
        );
        // Call edge was resolved.
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert!(
            callees.iter().any(|n| n.name == "helper"),
            "apply_changes should resolve run() -> helper()"
        );
    }

    #[test]
    fn sync_only_reindexes_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        // Add a new file, then sync.
        std::fs::write(dir.path().join("b.rs"), "pub fn b() {}\n").unwrap();
        let stats = indexer.sync().unwrap();
        assert_eq!(stats.files, 1, "only the new file b.rs should be synced");
        assert!(!db
            .lock()
            .unwrap()
            .find_node_by_name("b")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn sync_prunes_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "pub fn a() {}\n").unwrap();
        std::fs::write(&b, "pub fn b() {}\n").unwrap();
        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();
        assert!(!db
            .lock()
            .unwrap()
            .find_node_by_name("b")
            .unwrap()
            .is_empty());

        // Delete b.rs on disk, then sync: its node must disappear from the index.
        std::fs::remove_file(&b).unwrap();
        indexer.sync().unwrap();
        assert!(
            db.lock()
                .unwrap()
                .find_node_by_name("b")
                .unwrap()
                .is_empty(),
            "deleted file's symbols must be pruned"
        );
        assert!(!db
            .lock()
            .unwrap()
            .find_node_by_name("a")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn incremental_update_refreshes_cross_file_edges() {
        // B calls helper() defined in A. After renaming helper in A and syncing,
        // B's stale edge must be recomputed (gone), not left dangling.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "pub fn helper() -> i32 { 1 }\n").unwrap();
        std::fs::write(&b, "pub fn run() -> i32 { helper() }\n").unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();
        {
            let db = db.lock().unwrap();
            let run = db.find_node_by_name("run").unwrap();
            assert!(db
                .callees(&run[0].id, 10)
                .unwrap()
                .iter()
                .any(|n| n.name == "helper"));
        }

        // Rename helper -> helper2 in A. run() now resolves to nothing.
        std::fs::write(&a, "pub fn helper2() -> i32 { 1 }\n").unwrap();
        indexer.sync().unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        assert!(
            !callees.iter().any(|n| n.name == "helper"),
            "stale cross-file edge to renamed helper must be cleared"
        );
    }

    #[test]
    fn apply_changes_batches_updates_and_removals() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "pub fn helper() -> i32 { 1 }\n").unwrap();
        std::fs::write(&b, "pub fn run() -> i32 { helper() }\n").unwrap();

        let (db, indexer) = new_indexer(dir.path());
        // First batch: index both files in one call.
        indexer.apply_changes(&[a.clone(), b.clone()], &[]).unwrap();
        {
            let db = db.lock().unwrap();
            let run = db.find_node_by_name("run").unwrap();
            assert!(db
                .callees(&run[0].id, 10)
                .unwrap()
                .iter()
                .any(|n| n.name == "helper"));
        }

        // Second batch: delete a.rs (helper) in the same call that touches b.
        std::fs::remove_file(&a).unwrap();
        indexer
            .apply_changes(std::slice::from_ref(&b), std::slice::from_ref(&a))
            .unwrap();
        let db = db.lock().unwrap();
        assert!(
            db.find_node_by_name("helper").unwrap().is_empty(),
            "helper pruned"
        );
        let run = db.find_node_by_name("run").unwrap();
        assert!(
            !db.callees(&run[0].id, 10)
                .unwrap()
                .iter()
                .any(|n| n.name == "helper"),
            "edge to removed helper must be gone"
        );
    }

    #[test]
    fn import_filtering_narrows_ambiguous_cross_file_calls() {
        // `init` is defined in both b.rs and c.rs.
        // a.rs imports b (not c) and calls init(), so the edge must go to b::init.
        let dir = tempfile::tempdir().unwrap();
        // Python so we get proper import tracking.
        std::fs::write(dir.path().join("b.py"), "def init(): pass\n").unwrap();
        std::fs::write(dir.path().join("c.py"), "def init(): pass\n").unwrap();
        std::fs::write(
            dir.path().join("a.py"),
            "from b import init\ndef run():\n    init()\n",
        )
        .unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        let db = db.lock().unwrap();
        let run = db.find_node_by_name("run").unwrap();
        let callees = db.callees(&run[0].id, 10).unwrap();
        let b_path = dir.path().join("b.py").to_string_lossy().to_string();
        let c_path = dir.path().join("c.py").to_string_lossy().to_string();
        assert!(
            callees.iter().any(|n| n.file_path == b_path),
            "should resolve to b::init (imported)"
        );
        assert!(
            callees.iter().all(|n| n.file_path != c_path),
            "must NOT resolve to c::init (not imported)"
        );
    }

    #[test]
    fn incremental_resolve_updates_only_affected_edges() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let c = dir.path().join("c.rs");
        std::fs::write(&a, "pub fn alpha() {}\n").unwrap();
        std::fs::write(&b, "pub fn beta() { alpha() }\n").unwrap();
        std::fs::write(&c, "pub fn gamma() {}\n").unwrap();

        let (db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        // Verify initial edges.
        {
            let db = db.lock().unwrap();
            let beta = db.find_node_by_name("beta").unwrap();
            assert!(db
                .callees(&beta[0].id, 10)
                .unwrap()
                .iter()
                .any(|n| n.name == "alpha"));
        }

        // Rename alpha to alpha2 in a.rs. The edge beta→alpha should disappear.
        std::fs::write(&a, "pub fn alpha2() {}\n").unwrap();
        indexer
            .apply_changes(std::slice::from_ref(&a), &[])
            .unwrap();

        let db = db.lock().unwrap();
        let beta = db.find_node_by_name("beta").unwrap();
        let callees = db.callees(&beta[0].id, 10).unwrap();
        assert!(
            callees.iter().all(|n| n.name != "alpha"),
            "edge to removed alpha must be gone after incremental resolve"
        );
        // gamma (unrelated) must be unchanged.
        assert!(!db.find_node_by_name("gamma").unwrap().is_empty());
    }

    #[test]
    fn index_all_force_reindexes_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        let (_db, indexer) = new_indexer(dir.path());
        indexer.index_all(true, true).unwrap();

        // No changes: incremental reindexes 0, force reindexes 1.
        assert_eq!(indexer.index_all(false, true).unwrap().files, 0);
        assert_eq!(indexer.index_all(true, true).unwrap().files, 1);
    }
}
