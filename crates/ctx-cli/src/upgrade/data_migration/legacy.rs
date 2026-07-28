use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::{
    database_path,
    platform_security::{restrict_private_file, verify_private_file},
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    migration_directory, AvailableProviderSource, LegacyProjectionSummary, MigrationMarker,
    RELEASED_STORE_SCHEMA_VERSION,
};

pub(crate) const MAX_MIGRATION_CHUNK_ROWS: usize = 64;
pub(crate) const MAX_MIGRATION_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_LEGACY_PREVIEW_CHARS: usize = 2_048;
pub(crate) const MAX_LEGACY_METADATA_CHARS: usize = 4_096;
pub(crate) const MAX_AVAILABLE_SOURCES: usize = 256;

const LEGACY_PROJECTION_SCHEMA_VERSION: i64 = 1;
const LEGACY_PROJECTION_FILE: &str = "legacy-read-only-v0.sqlite";
const LEGACY_STAGE_PREFIX: &str = "legacy-read-only-v0.stage.";

const LEGACY_PROJECTION_DDL: &str = r#"
PRAGMA journal_mode = DELETE;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE projection_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    migration_id TEXT NOT NULL,
    legacy_fingerprint_json TEXT NOT NULL,
    source_inventory_sha256 TEXT NOT NULL,
    last_event_seq INTEGER NOT NULL,
    examined_events INTEGER NOT NULL,
    source_backed_events INTEGER NOT NULL,
    legacy_only_events INTEGER NOT NULL,
    chain_sha256 TEXT NOT NULL,
    complete INTEGER NOT NULL CHECK (complete IN (0, 1))
);

CREATE TABLE legacy_sessions (
    legacy_session_id TEXT PRIMARY KEY NOT NULL,
    provider TEXT NOT NULL,
    external_session_id TEXT,
    started_at_ms INTEGER
);

CREATE TABLE legacy_events (
    legacy_event_id TEXT PRIMARY KEY NOT NULL,
    legacy_event_seq INTEGER NOT NULL UNIQUE,
    legacy_session_id TEXT,
    provider TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    role TEXT,
    preview_text TEXT NOT NULL CHECK (length(preview_text) <= 2048),
    legacy_locator_json TEXT NOT NULL,
    FOREIGN KEY (legacy_session_id) REFERENCES legacy_sessions(legacy_session_id)
);

CREATE INDEX idx_legacy_events_session_seq
ON legacy_events(legacy_session_id, legacy_event_seq);

PRAGMA user_version = 1;
"#;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct FileObservation {
    present: bool,
    bytes: u64,
    modified_ns: u128,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct LegacyFingerprint {
    user_version: i64,
    database: FileObservation,
    wal: FileObservation,
    events: u64,
    max_event_seq: i64,
}

#[derive(Debug)]
struct LegacyRow {
    seq: i64,
    event_id: String,
    session_id: Option<String>,
    session_provider: Option<String>,
    capture_provider: Option<String>,
    source_format: Option<String>,
    raw_source_path: Option<String>,
    source_root: Option<String>,
    external_session_id: Option<String>,
    session_started_at_ms: Option<i64>,
    occurred_at_ms: i64,
    event_type: String,
    role: Option<String>,
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyProjectionInspection {
    pub(crate) user_version: i64,
    pub(crate) complete: bool,
    pub(crate) examined_events: u64,
    pub(crate) source_backed_events: u64,
    pub(crate) legacy_only_events: u64,
    pub(crate) last_event_seq: i64,
    pub(crate) chain_sha256: String,
    pub(crate) columns: Vec<String>,
}

pub(super) fn build_or_resume(
    data_root: &Path,
    marker: &MigrationMarker,
    available_sources: &[AvailableProviderSource],
    chunk_limit: Option<usize>,
) -> Result<Option<LegacyProjectionSummary>> {
    let legacy_path = database_path(data_root.to_path_buf());
    let final_path = final_path(data_root);
    let legacy = open_legacy_read_only(&legacy_path)?;
    validate_legacy_schema(&legacy)?;
    let legacy_fingerprint = fingerprint(&legacy, &legacy_path)?;
    let available = AvailableSourceSet::new(available_sources)?;
    if final_path.exists() {
        let inspection = inspect_legacy_projection(&final_path)?;
        validate_published_projection(
            &final_path,
            marker,
            &legacy_fingerprint,
            available.fingerprint(),
            &inspection,
        )?;
        return Ok((inspection.legacy_only_events > 0)
            .then(|| summary_from_inspection(final_path, inspection)));
    }

    let stage_path = stage_path(data_root, &marker.migration_id);
    let mut stage = open_or_create_stage(
        &stage_path,
        marker,
        &legacy_fingerprint,
        available.fingerprint(),
    )?;
    reconcile_stage_fingerprint(&stage, &legacy_fingerprint, available.fingerprint())?;
    let mut chunks = 0_usize;

    loop {
        if chunk_limit.is_some_and(|limit| chunks >= limit) {
            return Ok(None);
        }
        let progress = stage_progress(&stage)?;
        let rows = read_chunk(&legacy, progress.last_event_seq)?;
        if rows.is_empty() {
            if !available.is_unchanged() {
                bail!(
                    "provider source availability changed while the legacy exception projection was being built; roll back the unpublished stage and retry discovery"
                );
            }
            let current = fingerprint(&legacy, &legacy_path)?;
            if current != legacy_fingerprint {
                bail!(
                    "released ctx Store changed while its read-only legacy projection was being built; retry after the legacy writer is stopped"
                );
            }
            mark_complete(&mut stage)?;
            drop(stage);
            validate_stage(&stage_path)?;
            let inspection = inspect_legacy_projection(&stage_path)?;
            if inspection.legacy_only_events == 0 {
                discard_sqlite_family(&stage_path)?;
                return Ok(None);
            }
            publish_stage(&stage_path, &final_path)?;
            let inspection = inspect_legacy_projection(&final_path)?;
            return Ok(Some(summary_from_inspection(final_path, inspection)));
        }
        apply_chunk(&mut stage, &rows, &available)?;
        chunks = chunks.saturating_add(1);
    }
}

pub(super) fn stage_summary(
    data_root: &Path,
    migration_id: &str,
) -> Result<Option<LegacyProjectionSummary>> {
    let path = stage_path(data_root, migration_id);
    if !path.exists() {
        return Ok(None);
    }
    let inspection = inspect_legacy_projection(&path)?;
    Ok(Some(summary_from_inspection(path, inspection)))
}

#[allow(dead_code)] // Reached through the separately-owned recovery hook.
pub(super) fn discard_unpublished_stage(data_root: &Path, migration_id: &str) -> Result<()> {
    let path = stage_path(data_root, migration_id);
    discard_sqlite_family(&path)
}

fn discard_sqlite_family(path: &Path) -> Result<()> {
    for candidate in sqlite_family(path) {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove migration stage {}", candidate.display()))
            }
        }
    }
    Ok(())
}

pub(crate) fn inspect_legacy_projection(path: &Path) -> Result<LegacyProjectionInspection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open legacy projection read-only {}", path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let user_version = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let (
        complete,
        examined_events,
        source_backed_events,
        legacy_only_events,
        last_event_seq,
        chain_sha256,
    ) = conn.query_row(
        "SELECT complete, examined_events, source_backed_events,
                legacy_only_events, last_event_seq, chain_sha256
           FROM projection_state WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)? != 0,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let columns = table_columns(&conn, "legacy_events")?;
    Ok(LegacyProjectionInspection {
        user_version,
        complete,
        examined_events,
        source_backed_events,
        legacy_only_events,
        last_event_seq,
        chain_sha256,
        columns,
    })
}

fn open_legacy_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open released ctx Store read-only {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF; BEGIN;")?;
    Ok(conn)
}

fn validate_legacy_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != RELEASED_STORE_SCHEMA_VERSION {
        bail!(
            "legacy projection reader expected schema {RELEASED_STORE_SCHEMA_VERSION}, found {version}"
        );
    }
    for table in ["capture_sources", "sessions", "events"] {
        if !table_exists(conn, table)? {
            bail!("released ctx Store is missing required table {table}");
        }
    }
    let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        bail!("released ctx Store failed quick_check: {quick_check}");
    }
    Ok(())
}

fn fingerprint(conn: &Connection, path: &Path) -> Result<LegacyFingerprint> {
    let user_version = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let (events, max_event_seq) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(seq), -1) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let wal = sidecar_path(path, "-wal");
    Ok(LegacyFingerprint {
        user_version,
        database: observe_file(path)?,
        wal: observe_file(&wal)?,
        events,
        max_event_seq,
    })
}

fn observe_file(path: &Path) -> Result<FileObservation> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileObservation {
                present: false,
                bytes: 0,
                modified_ns: 0,
            })
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", path.display()));
        }
    };
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(FileObservation {
        present: true,
        bytes: metadata.len(),
        modified_ns,
    })
}

fn open_or_create_stage(
    path: &Path,
    marker: &MigrationMarker,
    fingerprint: &LegacyFingerprint,
    source_inventory_sha256: &str,
) -> Result<Connection> {
    if path.exists() {
        verify_private_file(path)?;
        let conn = Connection::open(path)
            .with_context(|| format!("resume legacy migration stage {}", path.display()))?;
        configure_stage_connection(&conn)?;
        return Ok(conn);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("create legacy migration stage {}", path.display()))?;
    restrict_private_file(path)?;
    conn.execute_batch(LEGACY_PROJECTION_DDL)?;
    configure_stage_connection(&conn)?;
    conn.execute(
        "INSERT INTO projection_state (
             singleton, schema_version, migration_id, legacy_fingerprint_json,
             source_inventory_sha256, last_event_seq, examined_events,
             source_backed_events, legacy_only_events, chain_sha256, complete
         ) VALUES (1, ?1, ?2, ?3, ?4, -1, 0, 0, 0, ?5, 0)",
        params![
            LEGACY_PROJECTION_SCHEMA_VERSION,
            marker.migration_id,
            serde_json::to_string(fingerprint)?,
            source_inventory_sha256,
            hex_sha256(b"ctx.legacy-projection.v0\0"),
        ],
    )?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(conn)
}

fn configure_stage_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(())
}

fn reconcile_stage_fingerprint(
    stage: &Connection,
    expected: &LegacyFingerprint,
    source_inventory_sha256: &str,
) -> Result<()> {
    let (encoded, observed_sources): (String, String) = stage.query_row(
        "SELECT legacy_fingerprint_json, source_inventory_sha256
           FROM projection_state WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let observed: LegacyFingerprint = serde_json::from_str(&encoded)?;
    if observed != *expected {
        bail!(
            "released ctx Store changed since this legacy projection attempt began; the existing stage is preserved for diagnosis"
        );
    }
    if observed_sources != source_inventory_sha256 {
        bail!(
            "provider source availability changed since this legacy projection attempt began; roll back the unpublished stage and retry discovery"
        );
    }
    Ok(())
}

fn validate_published_projection(
    path: &Path,
    marker: &MigrationMarker,
    legacy_fingerprint: &LegacyFingerprint,
    source_inventory_sha256: &str,
    inspection: &LegacyProjectionInspection,
) -> Result<()> {
    if !inspection.complete {
        bail!(
            "published legacy migration projection {} is incomplete",
            path.display()
        );
    }
    if !fs::metadata(path)?.permissions().readonly() {
        bail!(
            "published legacy migration projection {} is not read-only",
            path.display()
        );
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let (migration_id, encoded_fingerprint, observed_sources): (String, String, String) = conn
        .query_row(
            "SELECT migration_id, legacy_fingerprint_json, source_inventory_sha256
               FROM projection_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if migration_id != marker.migration_id {
        bail!(
            "published legacy migration projection {} belongs to a different migration attempt",
            path.display()
        );
    }
    if serde_json::from_str::<LegacyFingerprint>(&encoded_fingerprint)? != *legacy_fingerprint {
        bail!(
            "released ctx Store changed after legacy exception projection {} was published",
            path.display()
        );
    }
    if observed_sources != source_inventory_sha256 {
        bail!(
            "provider source availability changed after legacy exception projection {} was published",
            path.display()
        );
    }
    Ok(())
}

#[derive(Debug)]
struct StageProgress {
    last_event_seq: i64,
    examined_events: u64,
    source_backed_events: u64,
    legacy_only_events: u64,
    chain_sha256: String,
}

fn stage_progress(conn: &Connection) -> Result<StageProgress> {
    conn.query_row(
        "SELECT last_event_seq, examined_events, source_backed_events,
                legacy_only_events, chain_sha256
           FROM projection_state WHERE singleton = 1",
        [],
        |row| {
            Ok(StageProgress {
                last_event_seq: row.get(0)?,
                examined_events: row.get(1)?,
                source_backed_events: row.get(2)?,
                legacy_only_events: row.get(3)?,
                chain_sha256: row.get(4)?,
            })
        },
    )
    .map_err(Into::into)
}

fn read_chunk(conn: &Connection, after_seq: i64) -> Result<Vec<LegacyRow>> {
    let has_lookup = table_exists(conn, "event_search_lookup")?
        && table_columns(conn, "event_search_lookup")?
            .iter()
            .any(|column| column == "preview_text");
    let lookup_join = if has_lookup {
        "LEFT JOIN event_search_lookup AS lookup ON lookup.event_id = event.id"
    } else {
        ""
    };
    let preview = if has_lookup {
        "substr(COALESCE(lookup.preview_text, event.payload_json, ''), 1, 2048)"
    } else {
        "COALESCE(substr(event.payload_json, 1, 2048), '')"
    };
    let metadata_limit = MAX_LEGACY_METADATA_CHARS + 1;
    let sql = format!(
        "SELECT event.seq,
                substr(event.id, 1, {metadata_limit}),
                substr(event.session_id, 1, {metadata_limit}),
                substr(session.provider, 1, {metadata_limit}),
                substr(source.provider, 1, {metadata_limit}),
                substr(source.source_format, 1, {metadata_limit}),
                substr(source.raw_source_path, 1, {metadata_limit}),
                substr(source.source_root, 1, {metadata_limit}),
                substr(session.external_session_id, 1, {metadata_limit}),
                session.started_at_ms, event.occurred_at_ms,
                substr(event.event_type, 1, {metadata_limit}),
                substr(event.role, 1, {metadata_limit}), {preview}
           FROM events AS event
           LEFT JOIN sessions AS session ON session.id = event.session_id
           LEFT JOIN capture_sources AS source ON source.id = event.capture_source_id
           {lookup_join}
          WHERE event.seq > ?1
            AND event.deleted_at_ms IS NULL
          ORDER BY event.seq
          LIMIT {MAX_MIGRATION_CHUNK_ROWS}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([after_seq], |row| {
        Ok(LegacyRow {
            seq: row.get(0)?,
            event_id: row.get(1)?,
            session_id: row.get(2)?,
            session_provider: row.get(3)?,
            capture_provider: row.get(4)?,
            source_format: row.get(5)?,
            raw_source_path: row.get(6)?,
            source_root: row.get(7)?,
            external_session_id: row.get(8)?,
            session_started_at_ms: row.get(9)?,
            occurred_at_ms: row.get(10)?,
            event_type: row.get(11)?,
            role: row.get(12)?,
            preview: bounded_preview(row.get::<_, String>(13)?),
        })
    })?;
    let mut result = Vec::new();
    let mut bytes = 0_usize;
    for row in rows {
        let row = row?;
        validate_row_metadata_bounds(&row)?;
        let row_bytes = estimated_row_bytes(&row);
        if !result.is_empty() && bytes.saturating_add(row_bytes) > MAX_MIGRATION_CHUNK_BYTES {
            break;
        }
        if row_bytes > MAX_MIGRATION_CHUNK_BYTES {
            bail!(
                "legacy event {} exceeds the bounded migration chunk envelope",
                row.event_id
            );
        }
        bytes = bytes.saturating_add(row_bytes);
        result.push(row);
    }
    Ok(result)
}

fn validate_row_metadata_bounds(row: &LegacyRow) -> Result<()> {
    for (label, value) in [
        ("event id", Some(row.event_id.as_str())),
        ("session id", row.session_id.as_deref()),
        ("session provider", row.session_provider.as_deref()),
        ("capture provider", row.capture_provider.as_deref()),
        ("source format", row.source_format.as_deref()),
        ("raw source path", row.raw_source_path.as_deref()),
        ("source root", row.source_root.as_deref()),
        ("external session id", row.external_session_id.as_deref()),
        ("event type", Some(row.event_type.as_str())),
        ("role", row.role.as_deref()),
    ] {
        if value.is_some_and(|value| value.chars().count() > MAX_LEGACY_METADATA_CHARS) {
            bail!(
                "legacy {label} exceeds the {MAX_LEGACY_METADATA_CHARS}-character migration bound"
            );
        }
    }
    Ok(())
}

fn estimated_row_bytes(row: &LegacyRow) -> usize {
    [
        Some(row.event_id.as_str()),
        row.session_id.as_deref(),
        row.session_provider.as_deref(),
        row.capture_provider.as_deref(),
        row.source_format.as_deref(),
        row.raw_source_path.as_deref(),
        row.source_root.as_deref(),
        row.external_session_id.as_deref(),
        Some(row.event_type.as_str()),
        row.role.as_deref(),
        Some(row.preview.as_str()),
    ]
    .into_iter()
    .flatten()
    .fold(256_usize, |total, value| total.saturating_add(value.len()))
}

fn apply_chunk(
    conn: &mut Connection,
    rows: &[LegacyRow],
    available: &AvailableSourceSet,
) -> Result<()> {
    let before = stage_progress(conn)?;
    let tx = conn.transaction()?;
    let mut examined = before.examined_events;
    let mut source_backed = before.source_backed_events;
    let mut legacy_only = before.legacy_only_events;
    let mut chain = decode_digest(&before.chain_sha256)?;
    let mut last_seq = before.last_event_seq;

    for row in rows {
        let source_available = available.contains(row);
        chain = row_chain_digest(chain, row, source_available);
        examined = examined.saturating_add(1);
        last_seq = row.seq;
        if source_available {
            source_backed = source_backed.saturating_add(1);
            continue;
        }
        legacy_only = legacy_only.saturating_add(1);
        insert_legacy_row(&tx, row)?;
    }
    tx.execute(
        "UPDATE projection_state
            SET last_event_seq = ?1,
                examined_events = ?2,
                source_backed_events = ?3,
                legacy_only_events = ?4,
                chain_sha256 = ?5
          WHERE singleton = 1",
        params![last_seq, examined, source_backed, legacy_only, hex(&chain)],
    )?;
    tx.commit()?;
    Ok(())
}

fn insert_legacy_row(tx: &Transaction<'_>, row: &LegacyRow) -> Result<()> {
    let provider = row
        .session_provider
        .as_deref()
        .or(row.capture_provider.as_deref())
        .unwrap_or("unknown");
    if let Some(session_id) = &row.session_id {
        tx.execute(
            "INSERT OR IGNORE INTO legacy_sessions (
                 legacy_session_id, provider, external_session_id, started_at_ms
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id,
                provider,
                row.external_session_id,
                row.session_started_at_ms
            ],
        )?;
    }
    let locator = serde_json::json!({
        "kind": "released_ctx_store_event_v0",
        "legacy_event_id": row.event_id,
        "legacy_event_seq": row.seq,
        "legacy_session_id": row.session_id,
        "store_schema_version": RELEASED_STORE_SCHEMA_VERSION,
    });
    tx.execute(
        "INSERT INTO legacy_events (
             legacy_event_id, legacy_event_seq, legacy_session_id, provider,
             occurred_at_ms, event_type, role, preview_text, legacy_locator_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.event_id,
            row.seq,
            row.session_id,
            provider,
            row.occurred_at_ms,
            row.event_type,
            row.role,
            row.preview,
            serde_json::to_string(&locator)?,
        ],
    )?;
    Ok(())
}

fn mark_complete(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE projection_state SET complete = 1 WHERE singleton = 1",
        [],
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn validate_stage(path: &Path) -> Result<()> {
    let inspection = inspect_legacy_projection(path)?;
    if inspection.user_version != LEGACY_PROJECTION_SCHEMA_VERSION || !inspection.complete {
        bail!("legacy migration stage did not reach a complete supported schema");
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("legacy migration stage failed integrity_check: {integrity}");
    }
    let rows: u64 = conn.query_row("SELECT COUNT(*) FROM legacy_events", [], |row| row.get(0))?;
    if rows != inspection.legacy_only_events {
        bail!(
            "legacy migration stage row count mismatch: expected {}, found {rows}",
            inspection.legacy_only_events
        );
    }
    Ok(())
}

fn publish_stage(stage: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        bail!(
            "legacy migration projection target {} appeared during publication",
            target.display()
        );
    }
    fs::rename(stage, target).with_context(|| {
        format!(
            "publish read-only legacy projection {} to {}",
            stage.display(),
            target.display()
        )
    })?;
    restrict_private_file(target)?;
    let mut permissions = fs::metadata(target)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(target, permissions)?;
    verify_private_file(target)?;
    if let Some(parent) = target.parent() {
        sync_parent(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync migration directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

fn summary_from_inspection(
    path: PathBuf,
    inspection: LegacyProjectionInspection,
) -> LegacyProjectionSummary {
    LegacyProjectionSummary {
        path,
        examined_events: inspection.examined_events,
        source_backed_events: inspection.source_backed_events,
        legacy_only_events: inspection.legacy_only_events,
        last_event_seq: inspection.last_event_seq,
        chain_sha256: inspection.chain_sha256,
    }
}

fn final_path(data_root: &Path) -> PathBuf {
    migration_directory(data_root).join(LEGACY_PROJECTION_FILE)
}

fn stage_path(data_root: &Path, migration_id: &str) -> PathBuf {
    migration_directory(data_root).join(format!("{LEGACY_STAGE_PREFIX}{migration_id}.sqlite"))
}

fn sqlite_family(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        sidecar_path(path, "-wal"),
        sidecar_path(path, "-shm"),
    ]
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(Into::into)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let escaped = table.replace('\'', "''");
    let mut stmt = conn.prepare(&format!("PRAGMA table_info('{escaped}')"))?;
    let rows = stmt.query_map([], |row| row.get(1))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn bounded_preview(value: String) -> String {
    value.chars().take(MAX_LEGACY_PREVIEW_CHARS).collect()
}

struct AvailableSourceSet {
    exact: HashSet<(String, String, PathBuf)>,
    observations: Vec<SourceObservation>,
    fingerprint: String,
}

struct SourceObservation {
    path: PathBuf,
    present: bool,
    canonical_path: Option<PathBuf>,
}

impl AvailableSourceSet {
    fn new(sources: &[AvailableProviderSource]) -> Result<Self> {
        if sources.len() > MAX_AVAILABLE_SOURCES {
            bail!(
                "provider discovery returned {} sources; migration accepts at most {MAX_AVAILABLE_SOURCES}",
                sources.len()
            );
        }
        let mut exact = HashSet::new();
        let mut observations = Vec::with_capacity(sources.len());
        let mut fingerprint_rows = Vec::with_capacity(sources.len());
        for source in sources {
            let source_path = source.path.to_string_lossy();
            for (label, value) in [
                ("provider", source.provider.as_str()),
                ("source format", source.source_format.as_str()),
                ("source path", source_path.as_ref()),
            ] {
                if value.chars().count() > MAX_LEGACY_METADATA_CHARS {
                    bail!(
                        "{label} exceeds the {MAX_LEGACY_METADATA_CHARS}-character migration bound"
                    );
                }
            }
            let present = source.path.exists();
            let canonical_path = present
                .then(|| fs::canonicalize(&source.path).ok())
                .flatten();
            if present {
                let original = (
                    source.provider.clone(),
                    source.source_format.clone(),
                    source.path.clone(),
                );
                exact.insert(original);
                if let Some(path) = &canonical_path {
                    exact.insert((
                        source.provider.clone(),
                        source.source_format.clone(),
                        path.clone(),
                    ));
                }
            }
            fingerprint_rows.push(serde_json::to_string(&(
                source.provider.as_str(),
                source.source_format.as_str(),
                source_path.as_ref(),
                present,
                canonical_path
                    .as_deref()
                    .map(Path::to_string_lossy)
                    .unwrap_or_default(),
            ))?);
            observations.push(SourceObservation {
                path: source.path.clone(),
                present,
                canonical_path,
            });
        }
        fingerprint_rows.sort_unstable();
        let fingerprint = hex_sha256(fingerprint_rows.join("\n").as_bytes());
        Ok(Self {
            exact,
            observations,
            fingerprint,
        })
    }

    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn is_unchanged(&self) -> bool {
        self.observations.iter().all(|observation| {
            observation.path.exists() == observation.present
                && (!observation.present
                    || fs::canonicalize(&observation.path).ok() == observation.canonical_path)
        })
    }

    fn contains(&self, row: &LegacyRow) -> bool {
        let Some(provider) = row.capture_provider.as_deref() else {
            return false;
        };
        let Some(source_format) = row.source_format.as_deref() else {
            return false;
        };
        [row.raw_source_path.as_deref(), row.source_root.as_deref()]
            .into_iter()
            .flatten()
            .any(|value| {
                let path = PathBuf::from(value);
                (path.exists()
                    && self.exact.contains(&(
                        provider.to_owned(),
                        source_format.to_owned(),
                        path.clone(),
                    )))
                    || fs::canonicalize(&path).ok().is_some_and(|canonical| {
                        self.exact.contains(&(
                            provider.to_owned(),
                            source_format.to_owned(),
                            canonical,
                        ))
                    })
            })
    }
}

fn row_chain_digest(previous: [u8; 32], row: &LegacyRow, source_available: bool) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.legacy-projection.row.v0\0");
    digest.update(previous);
    digest.update(row.seq.to_be_bytes());
    digest_field(&mut digest, row.event_id.as_bytes());
    digest_optional(&mut digest, row.session_id.as_deref());
    digest_optional(&mut digest, row.session_provider.as_deref());
    digest_optional(&mut digest, row.capture_provider.as_deref());
    digest_optional(&mut digest, row.source_format.as_deref());
    digest_optional(&mut digest, row.raw_source_path.as_deref());
    digest_optional(&mut digest, row.source_root.as_deref());
    digest.update(row.occurred_at_ms.to_be_bytes());
    digest_field(&mut digest, row.event_type.as_bytes());
    digest_optional(&mut digest, row.role.as_deref());
    digest_field(&mut digest, row.preview.as_bytes());
    digest.update([u8::from(source_available)]);
    digest.finalize().into()
}

fn digest_optional(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest_field(digest, value.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hex_sha256(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(value: &[u8]) -> String {
    let mut result = String::with_capacity(value.len() * 2);
    for byte in value {
        use std::fmt::Write as _;
        let _ = write!(&mut result, "{byte:02x}");
    }
    result
}

fn decode_digest(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(anyhow!("legacy projection chain digest has invalid length"));
    }
    let mut result = [0_u8; 32];
    for (index, target) in result.iter_mut().enumerate() {
        *target = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| anyhow!("legacy projection chain digest is not hexadecimal"))?;
    }
    Ok(result)
}
