use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

use crate::types::{Edge, EdgeKind, FileRecord, Node, NodeKind, UnresolvedRef};

/// Ordered schema migrations. Index `i` is schema version `i+1`; each runs once,
/// gated by `PRAGMA user_version`. Never edit an existing entry — append a new
/// one. Statements must be idempotent-safe for version 1 (it may run against a
/// pre-existing v0 database created before versioning was introduced).
const MIGRATIONS: &[&str] = &[
    // v1 — base schema.
    "
    CREATE TABLE IF NOT EXISTS project_metadata (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS files (
        id           TEXT PRIMARY KEY,
        path         TEXT NOT NULL UNIQUE,
        language     TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        size         INTEGER NOT NULL,
        last_indexed INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS nodes (
        id             TEXT PRIMARY KEY,
        kind           TEXT NOT NULL,
        name           TEXT NOT NULL,
        qualified_name TEXT NOT NULL,
        file_path      TEXT NOT NULL,
        language       TEXT NOT NULL,
        start_line     INTEGER NOT NULL,
        end_line       INTEGER NOT NULL,
        signature      TEXT,
        docstring      TEXT,
        visibility     TEXT,
        is_exported    INTEGER NOT NULL DEFAULT 0,
        is_async       INTEGER NOT NULL DEFAULT 0,
        is_static      INTEGER NOT NULL DEFAULT 0,
        is_abstract    INTEGER NOT NULL DEFAULT 0,
        FOREIGN KEY(file_path) REFERENCES files(path) ON DELETE CASCADE
    );

    CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_path);
    CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
    CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);

    CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
        id UNINDEXED,
        name,
        qualified_name,
        signature,
        docstring,
        content='nodes',
        content_rowid='rowid'
    );

    CREATE TRIGGER IF NOT EXISTS nodes_fts_ai AFTER INSERT ON nodes BEGIN
        INSERT INTO nodes_fts(rowid, id, name, qualified_name, signature, docstring)
        VALUES (new.rowid, new.id, new.name, new.qualified_name, new.signature, new.docstring);
    END;
    CREATE TRIGGER IF NOT EXISTS nodes_fts_ad AFTER DELETE ON nodes BEGIN
        INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, signature, docstring)
        VALUES ('delete', old.rowid, old.id, old.name, old.qualified_name, old.signature, old.docstring);
    END;
    CREATE TRIGGER IF NOT EXISTS nodes_fts_au AFTER UPDATE ON nodes BEGIN
        INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, signature, docstring)
        VALUES ('delete', old.rowid, old.id, old.name, old.qualified_name, old.signature, old.docstring);
        INSERT INTO nodes_fts(rowid, id, name, qualified_name, signature, docstring)
        VALUES (new.rowid, new.id, new.name, new.qualified_name, new.signature, new.docstring);
    END;

    CREATE TABLE IF NOT EXISTS edges (
        id         TEXT PRIMARY KEY,
        source     TEXT NOT NULL,
        target     TEXT NOT NULL,
        kind       TEXT NOT NULL,
        provenance TEXT NOT NULL DEFAULT 'tree-sitter',
        metadata   TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source);
    CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target);
    CREATE INDEX IF NOT EXISTS idx_edges_kind   ON edges(kind);

    CREATE TABLE IF NOT EXISTS unresolved_refs (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        source_node_id TEXT NOT NULL,
        target_name    TEXT NOT NULL,
        kind           TEXT NOT NULL,
        file_path      TEXT NOT NULL
    );
    ",
    // v2 — PageRank centrality persisted per node.
    "
    ALTER TABLE nodes ADD COLUMN rank REAL NOT NULL DEFAULT 0;
    CREATE INDEX IF NOT EXISTS idx_nodes_rank ON nodes(rank);
    ",
    // v3 — resolution confidence on derived edges.
    "ALTER TABLE edges ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;",
    // v4 — parse diagnostics per file.
    "
    ALTER TABLE files ADD COLUMN parse_errors INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE files ADD COLUMN parse_missing INTEGER NOT NULL DEFAULT 0;
    ",
    // v5 — local semantic-search embeddings per node.
    "
    CREATE TABLE IF NOT EXISTS embeddings (
        node_id TEXT PRIMARY KEY,
        vec     BLOB NOT NULL
    );
    ",
    // v6 — type hint for type-directed call/reference resolution.
    "ALTER TABLE unresolved_refs ADD COLUMN receiver_hint TEXT;",
];

/// Raw row shape for an unresolved ref: (source, target, kind, file, receiver_hint).
type UnresolvedRow = (String, String, String, String, Option<String>);

/// Build a `?1, ?2, …, ?N` placeholder string for a dynamic IN clause.
fn placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Merge several ranked node lists by reciprocal-rank fusion, deduplicating by
/// id. A node ranked highly in several lists rises to the top.
fn fuse_ranked(lists: &[Vec<Node>], limit: usize) -> Vec<Node> {
    use std::collections::HashMap;
    const K: f32 = 60.0;
    let mut score: HashMap<String, f32> = HashMap::new();
    let mut node_by_id: HashMap<String, Node> = HashMap::new();
    for list in lists {
        for (rank, n) in list.iter().enumerate() {
            *score.entry(n.id.clone()).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
            node_by_id.entry(n.id.clone()).or_insert_with(|| n.clone());
        }
    }
    let mut ranked: Vec<(String, f32)> = score.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    ranked.truncate(limit);
    ranked
        .into_iter()
        .filter_map(|(id, _)| node_by_id.remove(&id))
        .collect()
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Block briefly on a held write lock instead of failing with SQLITE_BUSY
        // when several MCP requests or the watcher contend for the connection.
        conn.pragma_update(None, "busy_timeout", 5000)?;

        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an ephemeral in-memory database. Test-only helper.
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Run `f` inside a single deferred transaction, committing on success and
    /// rolling back on error. Makes a multi-file index update atomic and far
    /// faster than autocommitting each statement.
    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN")?;
        match f() {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Apply any migrations the database hasn't seen yet, tracked via SQLite's
    /// `PRAGMA user_version`. Each entry in `MIGRATIONS` is applied exactly once,
    /// in order; bumping the schema is a matter of appending a new statement.
    /// This replaces the old "CREATE IF NOT EXISTS everything" approach so future
    /// schema changes can't silently diverge between fresh and existing indexes.
    fn migrate(&self) -> Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, stmt) in MIGRATIONS.iter().enumerate() {
            let version = (i + 1) as i64;
            if current < version {
                self.conn
                    .execute_batch(stmt)
                    .with_context(|| format!("migration {version} failed"))?;
                // user_version doesn't accept bound params; the value is our own
                // integer constant, so formatting it is safe.
                self.conn
                    .execute_batch(&format!("PRAGMA user_version = {version};"))?;
            }
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    // --- File operations ---

    pub fn upsert_file(&self, file: &FileRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files (id, path, language, content_hash, size, last_indexed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
               content_hash = excluded.content_hash,
               size         = excluded.size,
               last_indexed = excluded.last_indexed",
            params![
                file.id,
                file.path,
                file.language,
                file.content_hash,
                file.size as i64,
                file.last_indexed
            ],
        )?;
        Ok(())
    }

    pub fn get_file_hash(&self, path: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT content_hash FROM files WHERE path = ?1")?;
        let result = stmt.query_row(params![path], |row| row.get(0));
        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_file(&self, path: &str) -> Result<()> {
        // Cascade handles nodes; clean up edges separately since they reference node IDs
        let node_ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM nodes WHERE file_path = ?1")?;
            let rows = stmt
                .query_map(params![path], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };

        for id in &node_ids {
            self.conn.execute(
                "DELETE FROM edges WHERE source = ?1 OR target = ?1",
                params![id],
            )?;
        }

        self.conn.execute(
            "DELETE FROM unresolved_refs WHERE file_path = ?1",
            params![path],
        )?;
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn all_file_hashes(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, content_hash FROM files")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // --- Node operations ---

    pub fn upsert_node(&self, node: &Node) -> Result<()> {
        self.conn.execute(
            "INSERT INTO nodes
               (id, kind, name, qualified_name, file_path, language,
                start_line, end_line, signature, docstring, visibility,
                is_exported, is_async, is_static, is_abstract)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(id) DO UPDATE SET
               kind           = excluded.kind,
               name           = excluded.name,
               qualified_name = excluded.qualified_name,
               start_line     = excluded.start_line,
               end_line       = excluded.end_line,
               signature      = excluded.signature,
               docstring      = excluded.docstring,
               visibility     = excluded.visibility,
               is_exported    = excluded.is_exported,
               is_async       = excluded.is_async,
               is_static      = excluded.is_static,
               is_abstract    = excluded.is_abstract",
            params![
                node.id,
                node.kind.as_str(),
                node.name,
                node.qualified_name,
                node.file_path,
                node.language,
                node.start_line,
                node.end_line,
                node.signature,
                node.docstring,
                node.visibility,
                node.is_exported as i32,
                node.is_async as i32,
                node.is_static as i32,
                node.is_abstract as i32,
            ],
        )?;
        Ok(())
    }

    pub fn delete_nodes_for_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM nodes WHERE file_path = ?1", params![path])?;
        self.conn.execute(
            "DELETE FROM unresolved_refs WHERE file_path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub fn search_nodes(&self, query: &str, kind: Option<&str>, limit: usize) -> Result<Vec<Node>> {
        if let Some(k) = kind {
            let mut stmt = self.conn.prepare(
                "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
                        n.start_line, n.end_line, n.signature, n.docstring, n.visibility,
                        n.is_exported, n.is_async, n.is_static, n.is_abstract
                 FROM nodes_fts f
                 JOIN nodes n ON n.id = f.id
                 WHERE nodes_fts MATCH ?1 AND n.kind = ?2
                 ORDER BY bm25(nodes_fts) - 5.0 * n.rank
                 LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![query, k, limit as i64], row_to_node)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
                        n.start_line, n.end_line, n.signature, n.docstring, n.visibility,
                        n.is_exported, n.is_async, n.is_static, n.is_abstract
                 FROM nodes_fts f
                 JOIN nodes n ON n.id = f.id
                 WHERE nodes_fts MATCH ?1
                 ORDER BY bm25(nodes_fts) - 5.0 * n.rank
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![query, limit as i64], row_to_node)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }
    }

    pub fn find_node_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, language,
                    start_line, end_line, signature, docstring, visibility,
                    is_exported, is_async, is_static, is_abstract
             FROM nodes WHERE name = ?1 OR qualified_name = ?1",
        )?;
        let rows = stmt
            .query_map(params![name], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, kind, name, qualified_name, file_path, language,
                    start_line, end_line, signature, docstring, visibility,
                    is_exported, is_async, is_static, is_abstract
             FROM nodes WHERE id = ?1",
        )?;
        match stmt.query_row(params![id], row_to_node) {
            Ok(n) => Ok(Some(n)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn nodes_in_file(&self, file_path: &str) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, language,
                    start_line, end_line, signature, docstring, visibility,
                    is_exported, is_async, is_static, is_abstract
             FROM nodes WHERE file_path = ?1
             ORDER BY start_line",
        )?;
        let rows = stmt
            .query_map(params![file_path], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- Edge operations ---

    pub fn upsert_edge(&self, edge: &Edge) -> Result<()> {
        let metadata = edge.metadata.as_ref().map(|m| m.to_string());
        self.conn.execute(
            "INSERT INTO edges (id, source, target, kind, provenance, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO NOTHING",
            params![
                edge.id,
                edge.source,
                edge.target,
                edge.kind.as_str(),
                edge.provenance.as_str(),
                metadata
            ],
        )?;
        Ok(())
    }

    /// Like `upsert_edge`, but records a resolution `confidence` (0.0–1.0) and
    /// refreshes it on conflict. Used for derived (resolved) edges so consumers
    /// can rank or filter by how sure we are the link is real.
    pub fn upsert_edge_scored(&self, edge: &Edge, confidence: f32) -> Result<()> {
        let metadata = edge.metadata.as_ref().map(|m| m.to_string());
        self.conn.execute(
            "INSERT INTO edges (id, source, target, kind, provenance, metadata, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
               provenance = excluded.provenance,
               confidence = excluded.confidence",
            params![
                edge.id,
                edge.source,
                edge.target,
                edge.kind.as_str(),
                edge.provenance.as_str(),
                metadata,
                confidence as f64,
            ],
        )?;
        Ok(())
    }

    /// The type-like node (class/struct/interface/…) that directly contains the
    /// given node, if any. Used for inheritance-aware call resolution: a bare
    /// `helper()` inside a method should resolve to the method's own class first.
    pub fn container_of(&self, node_id: &str) -> Result<Option<Node>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
                    n.start_line, n.end_line, n.signature, n.docstring, n.visibility,
                    n.is_exported, n.is_async, n.is_static, n.is_abstract
             FROM edges e
             JOIN nodes n ON n.id = e.source
             WHERE e.target = ?1 AND e.kind = 'contains'
               AND n.kind IN ('class','struct','interface','trait','enum','protocol','module','namespace')
             LIMIT 1",
        )?;
        match stmt.query_row(params![node_id], row_to_node) {
            Ok(n) => Ok(Some(n)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Names of the base types a type node extends or implements, taken from the
    /// recorded (still-unresolved) references. Lets call resolution prefer
    /// methods inherited from a known superclass/interface.
    pub fn base_type_names(&self, type_node_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT target_name FROM unresolved_refs
             WHERE source_node_id = ?1 AND kind IN ('extends','implements')",
        )?;
        let rows = stmt.query_map(params![type_node_id], |r| r.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn callers(&self, node_id: &str, limit: usize) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
                    n.start_line, n.end_line, n.signature, n.docstring, n.visibility,
                    n.is_exported, n.is_async, n.is_static, n.is_abstract
             FROM edges e
             JOIN nodes n ON n.id = e.source
             WHERE e.target = ?1 AND e.kind = 'calls'
             ORDER BY e.confidence DESC, n.rank DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![node_id, limit as i64], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn callees(&self, node_id: &str, limit: usize) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language,
                    n.start_line, n.end_line, n.signature, n.docstring, n.visibility,
                    n.is_exported, n.is_async, n.is_static, n.is_abstract
             FROM edges e
             JOIN nodes n ON n.id = e.target
             WHERE e.source = ?1 AND e.kind = 'calls'
             ORDER BY e.confidence DESC, n.rank DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![node_id, limit as i64], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_edges_for_file(&self, file_path: &str) -> Result<()> {
        // Delete edges where source node is in this file
        self.conn.execute(
            "DELETE FROM edges WHERE source IN (SELECT id FROM nodes WHERE file_path = ?1)",
            params![file_path],
        )?;
        Ok(())
    }

    /// Delete every edge that is *derived* from cross-file reference resolution
    /// (calls, extends, implements, …). `contains` edges come straight from the
    /// extractors and are rebuilt per file, so they are preserved. Used to
    /// recompute the call graph from the full `unresolved_refs` table whenever
    /// any file changes, which keeps incremental updates consistent.
    pub fn delete_derived_edges(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE kind != 'contains'", [])?;
        Ok(())
    }

    /// Delete derived (non-contains) edges whose *source* node lives in one of
    /// the given files. Used for incremental re-resolution after a file edit.
    pub fn delete_derived_edges_for_files(&self, file_paths: &[String]) -> Result<()> {
        if file_paths.is_empty() {
            return Ok(());
        }
        let ph = placeholders(file_paths.len());
        self.conn.execute(
            &format!(
                "DELETE FROM edges WHERE kind != 'contains'
                 AND source IN (SELECT id FROM nodes WHERE file_path IN ({}))",
                ph
            ),
            rusqlite::params_from_iter(file_paths),
        )?;
        Ok(())
    }

    /// Delete derived edges whose *target* node lives in one of the given
    /// files. Needed when a definition changes and other files' call edges to
    /// it must be re-resolved.
    pub fn delete_derived_edges_to_files(&self, file_paths: &[String]) -> Result<()> {
        if file_paths.is_empty() {
            return Ok(());
        }
        let ph = placeholders(file_paths.len());
        self.conn.execute(
            &format!(
                "DELETE FROM edges WHERE kind != 'contains'
                 AND target IN (SELECT id FROM nodes WHERE file_path IN ({}))",
                ph
            ),
            rusqlite::params_from_iter(file_paths),
        )?;
        Ok(())
    }

    /// Import node names (the raw import text) recorded in the given file.
    /// Used to disambiguate cross-file call resolution by checking which
    /// modules the calling file actually imports.
    pub fn import_names_for_file(&self, file_path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM nodes WHERE file_path = ?1 AND kind = 'import'")?;
        let rows = stmt.query_map(params![file_path], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Node names defined in the given files. Used to find unresolved refs
    /// from other files that target those names (for incremental re-resolve).
    pub fn node_names_in_files(&self, file_paths: &[String]) -> Result<Vec<String>> {
        if file_paths.is_empty() {
            return Ok(vec![]);
        }
        let ph = placeholders(file_paths.len());
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT name FROM nodes
             WHERE file_path IN ({}) AND kind NOT IN ('file', 'import')",
            ph
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(file_paths), |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // --- Unresolved refs ---

    pub fn insert_unresolved_ref(&self, r: &UnresolvedRef) -> Result<()> {
        self.conn.execute(
            "INSERT INTO unresolved_refs (source_node_id, target_name, kind, file_path, receiver_hint)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                r.source_node_id,
                r.target_name,
                r.kind.as_str(),
                r.file_path,
                r.receiver_hint,
            ],
        )?;
        Ok(())
    }

    pub fn all_unresolved_refs(&self) -> Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_node_id, target_name, kind, file_path, receiver_hint FROM unresolved_refs",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?
            .collect::<Result<Vec<UnresolvedRow>, _>>()?;
        Ok(Self::parse_unresolved_rows(rows))
    }

    /// Unresolved refs whose source is in one of the given files.
    pub fn unresolved_refs_for_files(&self, file_paths: &[String]) -> Result<Vec<UnresolvedRef>> {
        if file_paths.is_empty() {
            return Ok(vec![]);
        }
        let ph = placeholders(file_paths.len());
        let sql = format!(
            "SELECT source_node_id, target_name, kind, file_path, receiver_hint
             FROM unresolved_refs WHERE file_path IN ({})",
            ph
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(file_paths), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?
            .collect::<Result<Vec<UnresolvedRow>, _>>()?;
        Ok(Self::parse_unresolved_rows(rows))
    }

    /// Unresolved refs whose `target_name` matches one of the given names.
    /// Used to pull in refs from other files that point at changed definitions.
    pub fn unresolved_refs_targeting_names(&self, names: &[String]) -> Result<Vec<UnresolvedRef>> {
        if names.is_empty() {
            return Ok(vec![]);
        }
        let ph = placeholders(names.len());
        let sql = format!(
            "SELECT source_node_id, target_name, kind, file_path, receiver_hint
             FROM unresolved_refs WHERE target_name IN ({})",
            ph
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(names), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?
            .collect::<Result<Vec<UnresolvedRow>, _>>()?;
        Ok(Self::parse_unresolved_rows(rows))
    }

    fn parse_unresolved_rows(rows: Vec<UnresolvedRow>) -> Vec<UnresolvedRef> {
        rows.into_iter()
            .filter_map(|(source, target, kind_str, file_path, receiver_hint)| {
                EdgeKind::from_str(&kind_str).map(|kind| UnresolvedRef {
                    source_node_id: source,
                    target_name: target,
                    kind,
                    file_path,
                    receiver_hint,
                })
            })
            .collect()
    }

    pub fn stats(&self) -> Result<DbStats> {
        let file_count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let node_count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        let edge_count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok(DbStats {
            file_count,
            node_count,
            edge_count,
        })
    }

    // --- PageRank / centrality ---

    /// All `calls` edges as (source_id, target_id) pairs, for centrality.
    pub fn call_edges(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, target FROM edges WHERE kind = 'calls'")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn all_node_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM nodes WHERE kind != 'file'")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Persist computed centrality scores. Call inside a transaction.
    pub fn set_node_ranks(&self, ranks: &[(String, f64)]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare_cached("UPDATE nodes SET rank = ?2 WHERE id = ?1")?;
        for (id, rank) in ranks {
            stmt.execute(params![id, rank])?;
        }
        Ok(())
    }

    // --- Parse diagnostics ---

    pub fn update_file_diagnostics(&self, path: &str, errors: i64, missing: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET parse_errors = ?2, parse_missing = ?3 WHERE path = ?1",
            params![path, errors, missing],
        )?;
        Ok(())
    }

    /// Files whose last parse had tree-sitter ERROR or MISSING nodes, worst first.
    pub fn files_with_parse_issues(&self) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, parse_errors, parse_missing FROM files
             WHERE parse_errors > 0 OR parse_missing > 0
             ORDER BY parse_errors + parse_missing DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // --- Fuzzy search support ---

    /// (id, name, qualified_name) for every non-file node. Used by trigram fuzzy
    /// matching when exact/FTS search misses. Capped by `limit` to bound memory
    /// on very large graphs.
    pub fn all_search_fields(&self, limit: usize) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, qualified_name FROM nodes
             WHERE kind != 'file' ORDER BY rank DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Fuzzy (trigram) search over symbol names, used as a fallback when FTS
    /// returns nothing. Scans up to `pool` of the highest-ranked symbols, scores
    /// each, and returns the best `limit` as full nodes.
    pub fn fuzzy_search(&self, query: &str, limit: usize, pool: usize) -> Result<Vec<Node>> {
        let fields = self.all_search_fields(pool)?;
        let mut scored: Vec<(f32, String)> = fields
            .iter()
            .filter_map(|(id, name, qualified)| {
                let s = crate::fuzzy::score(query, name, qualified);
                (s > 0.15).then(|| (s, id.clone()))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        let mut out = vec![];
        for (_, id) in scored {
            if let Some(n) = self.get_node(&id)? {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// FTS search with a trigram-fuzzy and vector (semantic) fallback when
    /// full-text matching finds nothing — tolerant of typos, partial names, and
    /// natural-language intent ("validate token" → `validateToken`).
    pub fn smart_search(&self, query: &str, kind: Option<&str>, limit: usize) -> Result<Vec<Node>> {
        let exact = self.search_nodes(query, kind, limit)?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        // Fuse the two recall-oriented signals by reciprocal-rank fusion.
        let fuzzy = self.fuzzy_search(query, limit * 2, 5000)?;
        let vector = self.vector_search(query, limit * 2)?;
        let mut fused = fuse_ranked(&[fuzzy, vector], limit * 2);
        if let Some(k) = kind {
            fused.retain(|n| n.kind.as_str() == k);
        }
        fused.truncate(limit);
        Ok(fused)
    }

    // --- Embeddings / vector search ---

    /// Replace all stored embeddings with `rows` in one transaction-friendly
    /// sweep. Called on full resolves so the vector index tracks the graph.
    pub fn replace_embeddings(&self, rows: &[(String, Vec<f32>)]) -> Result<()> {
        self.conn.execute("DELETE FROM embeddings", [])?;
        let mut stmt = self
            .conn
            .prepare_cached("INSERT OR REPLACE INTO embeddings (node_id, vec) VALUES (?1, ?2)")?;
        for (id, vec) in rows {
            stmt.execute(params![id, crate::embed::to_bytes(vec)])?;
        }
        Ok(())
    }

    pub fn embedding_count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?)
    }

    /// Cosine-rank stored embeddings against `query`'s embedding. Returns the
    /// best `limit` nodes. Empty when no embeddings have been computed.
    pub fn vector_search(&self, query: &str, limit: usize) -> Result<Vec<Node>> {
        let q = crate::embed::embed_text(query);
        let mut stmt = self.conn.prepare("SELECT node_id, vec FROM embeddings")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut scored: Vec<(f32, String)> = rows
            .iter()
            .filter_map(|(id, bytes)| {
                let v = crate::embed::from_bytes(bytes)?;
                let s = crate::embed::cosine(&q, &v);
                (s > 0.05).then(|| (s, id.clone()))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        let mut out = Vec::new();
        for (_, id) in scored {
            if let Some(n) = self.get_node(&id)? {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// Every non-file node. Used by whole-graph analyses (architecture report,
    /// embeddings, export).
    pub fn all_nodes(&self) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, language,
                    start_line, end_line, signature, docstring, visibility,
                    is_exported, is_async, is_static, is_abstract
             FROM nodes WHERE kind != 'file'",
        )?;
        let rows = stmt
            .query_map([], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All edges as (source, target, kind) triples. Used by export/arch.
    pub fn all_edges_typed(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, target, kind FROM edges")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Highest-centrality non-file nodes with their rank, for the graph
    /// explorer's default view (rank sizes the rendered nodes).
    pub fn top_nodes_with_rank(&self, limit: usize) -> Result<Vec<(Node, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, language,
                    start_line, end_line, signature, docstring, visibility,
                    is_exported, is_async, is_static, is_abstract, rank
             FROM nodes WHERE kind != 'file'
             ORDER BY rank DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((row_to_node(r)?, r.get::<_, f64>(15)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- Health / breakdowns ---

    pub fn language_breakdown(&self) -> Result<Vec<(String, usize)>> {
        let mut stmt = self.conn.prepare(
            "SELECT language, COUNT(*) FROM nodes WHERE kind != 'file'
             GROUP BY language ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn kind_breakdown(&self) -> Result<Vec<(String, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM nodes GROUP BY kind ORDER BY COUNT(*) DESC")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn unresolved_count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM unresolved_refs", [], |r| r.get(0))?)
    }

    pub fn edge_count_by_kind(&self, kind: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE kind = ?1",
            params![kind],
            |r| r.get(0),
        )?)
    }
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let kind_str: String = row.get(1)?;
    let kind = NodeKind::from_str(&kind_str).unwrap_or(NodeKind::Function);
    Ok(Node {
        id: row.get(0)?,
        kind,
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        language: row.get(5)?,
        start_line: row.get::<_, u32>(6)?,
        end_line: row.get::<_, u32>(7)?,
        signature: row.get(8)?,
        docstring: row.get(9)?,
        visibility: row.get(10)?,
        is_exported: row.get::<_, i32>(11)? != 0,
        is_async: row.get::<_, i32>(12)? != 0,
        is_static: row.get::<_, i32>(13)? != 0,
        is_abstract: row.get::<_, i32>(14)? != 0,
    })
}

pub struct DbStats {
    pub file_count: usize,
    pub node_count: usize,
    pub edge_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Provenance;

    fn file_record(path: &str) -> FileRecord {
        FileRecord {
            id: Node::new_id(path, path),
            path: path.to_string(),
            language: "rust".to_string(),
            content_hash: "hash".to_string(),
            size: 0,
            last_indexed: 0,
        }
    }

    fn node(name: &str, file: &str, kind: NodeKind) -> Node {
        let qualified = format!("{file}::{name}");
        Node {
            id: Node::new_id(file, &qualified),
            kind,
            name: name.to_string(),
            qualified_name: qualified,
            file_path: file.to_string(),
            language: "rust".to_string(),
            start_line: 1,
            end_line: 2,
            signature: Some(format!("fn {name}()")),
            docstring: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
        }
    }

    fn calls_edge(source: &Node, target: &Node) -> Edge {
        Edge {
            id: Edge::new_id(&source.id, &target.id, &EdgeKind::Calls),
            source: source.id.clone(),
            target: target.id.clone(),
            kind: EdgeKind::Calls,
            provenance: Provenance::TreeSitter,
            metadata: None,
        }
    }

    /// Seed one file with `nodes` already inserted; returns the open db.
    fn seeded(path: &str, nodes: &[Node]) -> Db {
        let db = Db::open_memory().unwrap();
        db.upsert_file(&file_record(path)).unwrap();
        for n in nodes {
            db.upsert_node(n).unwrap();
        }
        db
    }

    #[test]
    fn fts_is_maintained_incrementally_by_triggers() {
        // Triggers keep the FTS index in sync on insert/delete with no rebuild.
        let helper = node("widgetfn", "a.rs", NodeKind::Function);
        let db = seeded("a.rs", std::slice::from_ref(&helper));
        let found = db.search_nodes("widgetfn", None, 10).unwrap();
        assert!(
            found.iter().any(|n| n.id == helper.id),
            "insert indexed by trigger"
        );

        db.delete_file("a.rs").unwrap();
        let after = db.search_nodes("widgetfn", None, 10).unwrap();
        assert!(after.is_empty(), "delete removed from FTS by trigger");
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let db = seeded("a.rs", &[]);
        let n = node("keep", "a.rs", NodeKind::Function);
        let result: anyhow::Result<()> = db.transaction(|| {
            db.upsert_node(&n)?;
            anyhow::bail!("boom");
        });
        assert!(result.is_err());
        // The node inserted inside the aborted transaction must not survive.
        assert!(db.get_node(&n.id).unwrap().is_none());
    }

    #[test]
    fn unresolved_refs_for_files_filters_correctly() {
        let a = node("fa", "a.rs", NodeKind::Function);
        let b = node("fb", "b.rs", NodeKind::Function);
        let db = seeded("a.rs", std::slice::from_ref(&a));
        db.upsert_file(&file_record("b.rs")).unwrap();
        db.upsert_node(&b).unwrap();
        let ref_a = UnresolvedRef::new(
            a.id.clone(),
            "x".to_string(),
            EdgeKind::Calls,
            "a.rs".to_string(),
        );
        let ref_b = UnresolvedRef::new(
            b.id.clone(),
            "y".to_string(),
            EdgeKind::Calls,
            "b.rs".to_string(),
        );
        db.insert_unresolved_ref(&ref_a).unwrap();
        db.insert_unresolved_ref(&ref_b).unwrap();

        let only_a = db.unresolved_refs_for_files(&["a.rs".to_string()]).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].target_name, "x");

        let targeting_x = db
            .unresolved_refs_targeting_names(&["x".to_string()])
            .unwrap();
        assert_eq!(targeting_x.len(), 1);
        assert_eq!(targeting_x[0].file_path, "a.rs");
    }

    #[test]
    fn search_can_filter_by_kind() {
        let f = node("thing", "a.rs", NodeKind::Function);
        let s = node("thing", "a.rs", NodeKind::Struct);
        let db = seeded("a.rs", &[f.clone(), s.clone()]);
        let only_structs = db.search_nodes("thing", Some("struct"), 10).unwrap();
        assert!(only_structs.iter().all(|n| n.kind == NodeKind::Struct));
        assert!(only_structs.iter().any(|n| n.id == s.id));
    }

    #[test]
    fn callers_and_callees_follow_call_edges() {
        let caller = node("run", "a.rs", NodeKind::Function);
        let callee = node("helper", "a.rs", NodeKind::Function);
        let db = seeded("a.rs", &[caller.clone(), callee.clone()]);
        db.upsert_edge(&calls_edge(&caller, &callee)).unwrap();

        let callers = db.callers(&callee.id, 10).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].id, caller.id);

        let callees = db.callees(&caller.id, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].id, callee.id);
    }

    #[test]
    fn upsert_edge_is_idempotent() {
        let a = node("a", "a.rs", NodeKind::Function);
        let b = node("b", "a.rs", NodeKind::Function);
        let db = seeded("a.rs", &[a.clone(), b.clone()]);
        let edge = calls_edge(&a, &b);
        db.upsert_edge(&edge).unwrap();
        db.upsert_edge(&edge).unwrap();
        assert_eq!(db.stats().unwrap().edge_count, 1);
    }

    #[test]
    fn delete_file_removes_nodes_and_their_edges() {
        let a = node("a", "a.rs", NodeKind::Function);
        let b = node("b", "a.rs", NodeKind::Function);
        let db = seeded("a.rs", &[a.clone(), b.clone()]);
        db.upsert_edge(&calls_edge(&a, &b)).unwrap();

        db.delete_file("a.rs").unwrap();

        let stats = db.stats().unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.node_count, 0, "nodes cascade-deleted with the file");
        assert_eq!(
            stats.edge_count, 0,
            "edges referencing deleted nodes removed"
        );
        assert!(db.get_node(&a.id).unwrap().is_none());
    }

    #[test]
    fn unresolved_refs_persist() {
        let a = node("a", "a.rs", NodeKind::Function);
        let db = seeded("a.rs", std::slice::from_ref(&a));
        db.insert_unresolved_ref(&UnresolvedRef::new(
            a.id.clone(),
            "helper".to_string(),
            EdgeKind::Calls,
            "a.rs".to_string(),
        ))
        .unwrap();
        let pending = db.all_unresolved_refs().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].target_name, "helper");
    }
}
