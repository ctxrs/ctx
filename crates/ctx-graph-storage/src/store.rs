use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Statement, Transaction, params};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::Path,
    time::Duration,
};
use unicode_normalization::UnicodeNormalization;

const APPLICATION_ID: i64 = 0x47524146;
const SEARCH_VERSION: i64 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageLayout {
    Legacy,
    Compact,
}

// Physical layout is independent of the public graph/snapshot schema. Call
// inside the operation's transaction so an existing reader keeps its layout.
pub(crate) fn storage_layout(conn: &Connection) -> Result<StorageLayout> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        1 => Ok(StorageLayout::Legacy),
        // Formats 3 through 5 expose the same public payload columns. Formats
        // 4 and 5 reconstruct them from normalized fields instead of storing
        // each graph record twice; format 5 adds derived counters and leaner
        // indexes without changing the graph model.
        2..=5 => Ok(StorageLayout::Compact),
        _ => anyhow::bail!("unsupported Graf storage version {version}; expected 1, 2, 3, 4 or 5"),
    }
}

pub(crate) fn normalized_storage(conn: &Connection) -> Result<bool> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? >= 4)
}

pub struct Store {
    pub(crate) conn: Connection,
    baseline_generation: u64,
}

/// Logical SQLite page counts, not filesystem sizes. A busy checkpoint can
/// leave the old main-file length and WAL allocated after VACUUM commits.
#[derive(Debug, serde::Serialize)]
pub struct CompactionReport {
    pub schema_version: u32,
    pub page_size: u64,
    pub pages_before: u64,
    pub pages_after: u64,
    pub free_pages_before: u64,
    pub free_pages_after: u64,
    pub checkpoint_busy: bool,
}

const SCHEMA: &str = r#"
CREATE TABLE metadata (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    generation INTEGER NOT NULL, kind TEXT NOT NULL,
    root TEXT, coverage TEXT NOT NULL, graph_metadata TEXT NOT NULL
);
INSERT INTO metadata VALUES (1, 0, 'empty', NULL,
 '{"supported_files":0,"unsupported_files":0,"unchanged_files":0}', 'null');
"#;

const STORAGE_COUNTS: &str = r#"
CREATE TABLE storage_counts (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    files INTEGER NOT NULL CHECK(files >= 0),
    nodes INTEGER NOT NULL CHECK(nodes >= 0),
    edges INTEGER NOT NULL CHECK(edges >= 0),
    unresolved_references INTEGER NOT NULL CHECK(unresolved_references >= 0)
);
INSERT INTO storage_counts VALUES (1, 0, 0, 0, 0);
"#;

const STORAGE_COUNT_TRIGGERS: &str = r#"
CREATE TRIGGER storage_count_files_insert AFTER INSERT ON files BEGIN
    UPDATE storage_counts SET files=files+1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_files_delete AFTER DELETE ON files BEGIN
    UPDATE storage_counts SET files=files-1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_nodes_insert AFTER INSERT ON nodes BEGIN
    UPDATE storage_counts SET nodes=nodes+1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_nodes_delete AFTER DELETE ON nodes BEGIN
    UPDATE storage_counts SET nodes=nodes-1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_edges_insert AFTER INSERT ON edges BEGIN
    UPDATE storage_counts SET edges=edges+1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_edges_delete AFTER DELETE ON edges BEGIN
    UPDATE storage_counts SET edges=edges-1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_refs_insert AFTER INSERT ON refs
WHEN new.resolved_target_key IS NULL BEGIN
    UPDATE storage_counts SET unresolved_references=unresolved_references+1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_refs_delete AFTER DELETE ON refs
WHEN old.resolved_target_key IS NULL BEGIN
    UPDATE storage_counts SET unresolved_references=unresolved_references-1 WHERE singleton=1;
END;
CREATE TRIGGER storage_count_refs_update AFTER UPDATE OF resolved_target_key ON refs
WHEN (old.resolved_target_key IS NULL) != (new.resolved_target_key IS NULL) BEGIN
    UPDATE storage_counts SET unresolved_references=unresolved_references
        + CASE WHEN new.resolved_target_key IS NULL THEN 1 ELSE -1 END
        WHERE singleton=1;
END;
"#;

const DROP_STORAGE_COUNT_TRIGGERS: &str = r#"
DROP TRIGGER IF EXISTS storage_count_files_insert;
DROP TRIGGER IF EXISTS storage_count_files_delete;
DROP TRIGGER IF EXISTS storage_count_nodes_insert;
DROP TRIGGER IF EXISTS storage_count_nodes_delete;
DROP TRIGGER IF EXISTS storage_count_edges_insert;
DROP TRIGGER IF EXISTS storage_count_edges_delete;
DROP TRIGGER IF EXISTS storage_count_refs_insert;
DROP TRIGGER IF EXISTS storage_count_refs_delete;
DROP TRIGGER IF EXISTS storage_count_refs_update;
"#;

// The same tables serve fresh stores and transactional upgrades. Temporary
// names let the old parents and their children coexist while foreign keys stay on.
const COMPACT_TABLES: &str = r#"
CREATE TABLE compact_files (
    fkey INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
    hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL
);
CREATE TABLE compact_nodes (
    nkey INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL, qualified_name TEXT, binding_key TEXT,
    file TEXT NOT NULL, owner_key INTEGER REFERENCES compact_files(fkey) ON DELETE CASCADE,
    payload TEXT NOT NULL, search TEXT NOT NULL
);
CREATE TABLE compact_node_aliases (
    node_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    binding_key TEXT NOT NULL, PRIMARY KEY(node_key, binding_key)
) WITHOUT ROWID;
"#;

const COMPACT_REFERENCE_TABLES: &str = r#"
CREATE TABLE compact_refs (
    rkey INTEGER PRIMARY KEY,
    id TEXT GENERATED ALWAYS AS (json_extract(payload,'$.id')) VIRTUAL NOT NULL UNIQUE,
    source_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    owner_key INTEGER NOT NULL REFERENCES compact_files(fkey) ON DELETE CASCADE,
    relation TEXT NOT NULL, payload TEXT NOT NULL,
    resolved_target_key INTEGER, resolution_reason TEXT NOT NULL
);
CREATE TABLE compact_ref_keys (
    ref_key INTEGER NOT NULL REFERENCES compact_refs(rkey) ON DELETE CASCADE,
    priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
    PRIMARY KEY(ref_key, priority)
) WITHOUT ROWID;
CREATE TABLE compact_edges (
    id TEXT PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    target_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
    owner_key INTEGER REFERENCES compact_files(fkey) ON DELETE CASCADE,
    ref_key INTEGER UNIQUE REFERENCES compact_refs(rkey) ON DELETE CASCADE,
    payload TEXT NOT NULL
);
"#;

const COMPACT_PUBLISH: &str = r#"
ALTER TABLE compact_files RENAME TO files;
ALTER TABLE compact_nodes RENAME TO nodes;
ALTER TABLE compact_node_aliases RENAME TO node_aliases;
CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
INSERT INTO node_search(rowid,text) SELECT nkey,search FROM nodes;
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_search(rowid,text) VALUES(new.nkey,new.search);
END;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM node_search WHERE rowid=old.nkey;
END;
"#;

const COMPACT_REFERENCE_PUBLISH: &str = r#"
ALTER TABLE compact_refs RENAME TO refs;
ALTER TABLE compact_ref_keys RENAME TO ref_keys;
ALTER TABLE compact_edges RENAME TO edges;
"#;

// Format 4 keeps the query-facing payload contract but makes it virtual. This
// removes the duplicate full-record JSON while preserving schema-1 snapshots,
// existing SQL read paths, integer foreign keys, and FTS row identities.
const NORMALIZED_TABLES: &str = r#"
CREATE TABLE normalized_files (
    fkey INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
    hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL,
    facts_hash TEXT
);
CREATE TABLE normalized_nodes (
    nkey INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL, kind TEXT NOT NULL, file TEXT NOT NULL,
    line INTEGER, end_line INTEGER, qualified_name TEXT, binding_key TEXT,
    metadata TEXT NOT NULL CHECK(json_valid(metadata)),
    owner_key INTEGER REFERENCES normalized_files(fkey) ON DELETE CASCADE,
    search TEXT NOT NULL,
    payload TEXT GENERATED ALWAYS AS (
        json_object('id',id,'label',label,'kind',kind,'file',file,
                    'line',line,'end_line',end_line,
                    'qualified_name',qualified_name,'binding_key',binding_key,
                    'metadata',json(metadata))
    ) VIRTUAL
);
CREATE TABLE normalized_node_aliases (
    node_key INTEGER NOT NULL REFERENCES normalized_nodes(nkey) ON DELETE CASCADE,
    binding_key TEXT NOT NULL, PRIMARY KEY(node_key, binding_key)
) WITHOUT ROWID;
"#;

const NORMALIZED_REFERENCE_TABLES: &str = r#"
CREATE TABLE normalized_refs (
    rkey INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    source_key INTEGER NOT NULL REFERENCES normalized_nodes(nkey) ON DELETE CASCADE,
    owner_key INTEGER NOT NULL REFERENCES normalized_files(fkey) ON DELETE CASCADE,
    label TEXT NOT NULL, relation TEXT NOT NULL, file TEXT NOT NULL,
    line INTEGER NOT NULL, candidate_keys TEXT NOT NULL CHECK(json_valid(candidate_keys)),
    reason TEXT NOT NULL,
    resolved_target_key INTEGER, resolution_reason TEXT NOT NULL,
    payload TEXT GENERATED ALWAYS AS (
        json_object('id',id,'source',source,'label',label,'relation',relation,
                    'file',file,'line',line,'candidate_keys',json(candidate_keys),
                    'reason',reason)
    ) VIRTUAL
);
CREATE TABLE normalized_ref_keys (
    ref_key INTEGER NOT NULL REFERENCES normalized_refs(rkey) ON DELETE CASCADE,
    priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
    PRIMARY KEY(ref_key, priority)
) WITHOUT ROWID;
CREATE TABLE normalized_edges (
    id TEXT PRIMARY KEY, source TEXT NOT NULL, target TEXT NOT NULL,
    source_key INTEGER NOT NULL REFERENCES normalized_nodes(nkey) ON DELETE CASCADE,
    target_key INTEGER NOT NULL REFERENCES normalized_nodes(nkey) ON DELETE CASCADE,
    relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
    file TEXT, line INTEGER, confidence TEXT NOT NULL,
    metadata TEXT NOT NULL CHECK(json_valid(metadata)),
    owner_key INTEGER REFERENCES normalized_files(fkey) ON DELETE CASCADE,
    ref_key INTEGER UNIQUE REFERENCES normalized_refs(rkey) ON DELETE CASCADE,
    payload TEXT GENERATED ALWAYS AS (
        json_object('id',id,'source',source,'target',target,'relation',relation,
                    'directed',json(CASE directed WHEN 1 THEN 'true' ELSE 'false' END),
                    'file',file,'line',line,'confidence',confidence,
                    'metadata',json(metadata))
    ) VIRTUAL
);
"#;

const NORMALIZED_PUBLISH: &str = r#"
ALTER TABLE normalized_files RENAME TO files;
ALTER TABLE normalized_nodes RENAME TO nodes;
ALTER TABLE normalized_node_aliases RENAME TO node_aliases;
CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
INSERT INTO node_search(rowid,text) SELECT nkey,search FROM nodes;
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_search(rowid,text) VALUES(new.nkey,new.search);
END;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM node_search WHERE rowid=old.nkey;
END;
"#;

const NORMALIZED_REFERENCE_PUBLISH: &str = r#"
ALTER TABLE normalized_refs RENAME TO refs;
ALTER TABLE normalized_ref_keys RENAME TO ref_keys;
ALTER TABLE normalized_edges RENAME TO edges;
"#;

// Shared by fresh databases and explicit-write migration; these indexes do
// not change graph identity, payloads, or schema-1 snapshot compatibility.
const STORAGE_INDICES: &[(&str, &str)] = &[
    (
        "nodes_label",
        "CREATE INDEX nodes_label ON nodes(label, id)",
    ),
    ("nodes_file", "CREATE INDEX nodes_file ON nodes(file, id)"),
    (
        "node_aliases_binding",
        "CREATE INDEX node_aliases_binding ON node_aliases(binding_key, node_key)",
    ),
    (
        "refs_source",
        "CREATE INDEX refs_source ON refs(source_key)",
    ),
    ("refs_owner", "CREATE INDEX refs_owner ON refs(owner_key)"),
    (
        "refs_unresolved_relation",
        "CREATE INDEX refs_unresolved_relation ON refs(source_key, relation, id) WHERE resolved_target_key IS NULL",
    ),
    (
        "ref_keys_binding",
        "CREATE INDEX ref_keys_binding ON ref_keys(binding_key, ref_key)",
    ),
    (
        "nodes_qualified",
        "CREATE INDEX nodes_qualified ON nodes(qualified_name, id) WHERE qualified_name IS NOT NULL",
    ),
    (
        "nodes_binding",
        "CREATE INDEX nodes_binding ON nodes(binding_key, id) WHERE binding_key IS NOT NULL",
    ),
    (
        "nodes_owner",
        "CREATE INDEX nodes_owner ON nodes(owner_key) WHERE owner_key IS NOT NULL",
    ),
    (
        "edges_source_relation",
        "CREATE INDEX edges_source_relation ON edges(source_key, relation, id)",
    ),
    (
        "edges_target_relation",
        "CREATE INDEX edges_target_relation ON edges(target_key, relation, id)",
    ),
    // Undirected edges must be visited from either endpoint in stable ID order.
    // Partial indexes keep that path fast without duplicating every directed
    // edge in the overwhelmingly directed native graph.
    (
        "edges_source_direction",
        "CREATE INDEX edges_source_direction ON edges(source_key, id) WHERE directed=0",
    ),
    (
        "edges_target_direction",
        "CREATE INDEX edges_target_direction ON edges(target_key, id) WHERE directed=0",
    ),
    (
        "edges_source_direction_relation",
        "CREATE INDEX edges_source_direction_relation ON edges(source_key, relation, id) WHERE directed=0",
    ),
    (
        "edges_target_direction_relation",
        "CREATE INDEX edges_target_direction_relation ON edges(target_key, relation, id) WHERE directed=0",
    ),
    (
        "edges_owner",
        "CREATE INDEX edges_owner ON edges(owner_key) WHERE owner_key IS NOT NULL",
    ),
];

const OBSOLETE_STORAGE_INDICES: &[&str] =
    &["refs_unresolved_source", "edges_source", "edges_target"];

/// A concurrent writer committed after this handle captured its baseline.
#[derive(Debug)]
pub struct StaleStore;
impl std::fmt::Display for StaleStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("graph changed since this Store was opened; retry from a fresh Store")
    }
}
impl std::error::Error for StaleStore {}

// Only called inside an explicit write transaction, after its generation/kind
// checks (or during creation). Index replacement rolls back with the graph on
// failure and does not advance the logical graph generation on its own.

// Called only after the immediate transaction's baseline/kind/root checks.
// Return projection changes separately: physical-only upgrades do not publish
// a new graph generation. Backfilled aliases must reach the caller's rebind set.

// Search enrichment is another additive schema-1 extension. Migration runs
// only during an explicit write, under the same transaction/generation check.

pub(crate) fn cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
}

#[derive(Clone, Copy)]
enum InitialBinding {
    Unique(i64),
    Ambiguous,
}

// A fresh native graph has no readers or old facts to preserve. Load its rows
// without maintaining derived B-trees and FTS postings for every insert, then
// publish those structures once. Integer identities are carried in memory so
// the initial graph also avoids millions of repeated text-key subqueries.

pub(crate) fn generation(conn: &Connection) -> Result<u64> {
    let value: i64 = conn.query_row(
        "SELECT generation FROM metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    u64::try_from(value).context("invalid negative generation")
}

#[cfg(test)]
mod compaction_tests;

mod store_apply_native_inner;
mod store_connection;
mod store_create;

mod read_payloads;
use read_payloads::*;
mod initial_resolution;
use initial_resolution::*;
