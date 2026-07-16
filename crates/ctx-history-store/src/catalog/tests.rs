use std::{collections::BTreeMap, fs, time::Duration};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource,
    CaptureSourceDescriptor, CaptureSourceKind, EntityTimestamps, Event, EventRole, EventType,
    Fidelity, Session, SessionStatus, SyncMetadata, SyncState, Visibility,
    PROVIDER_MATERIAL_SOURCE_FORMATS,
};
use rusqlite::{ffi::ErrorCode, params};
use uuid::Uuid;

use crate::catalog::{
    expected_material_source_format, CatalogIndexedStatus, CatalogSession,
    CatalogSourceIndexUpdate, ImportPendingReason, ImportWorkClass, SourceImportFile,
    SourceImportFileIndexUpdate,
};
use crate::connection::timestamp_ms;
use crate::object_store::OBJECTS_DIR;
use crate::raw_sql::{RawSqlOptions, RawSqlValue};
use crate::{
    Result, Store, StoreError, RAW_SQL_MAX_COLUMNS_CAP, RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
    RAW_SQL_MAX_ROWS_CAP,
};

type CatalogSessionCheckpointRow = (
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-catalog-")
        .tempdir_in(root)
        .unwrap()
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}

fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

fn catalog_session(source_path: &str, external_session_id: &str, mtime_ms: i64) -> CatalogSession {
    CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".into(),
        source_root: "/home/user/.codex/sessions".into(),
        source_path: source_path.into(),
        external_session_id: Some(external_session_id.into()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        external_agent_id: None,
        cwd: Some("/repo".into()),
        session_started_at_ms: Some(mtime_ms),
        file_size_bytes: 42,
        file_modified_at_ms: mtime_ms,
        import_revision: 1,
        cataloged_at_ms: mtime_ms,
        metadata: serde_json::json!({"catalog_scope": "session_meta"}),
    }
}

fn catalog_session_for_root(
    source_root: &str,
    source_path: &str,
    external_session_id: &str,
    mtime_ms: i64,
) -> CatalogSession {
    CatalogSession {
        source_root: source_root.into(),
        ..catalog_session(source_path, external_session_id, mtime_ms)
    }
}

fn source_import_file(
    provider: CaptureProvider,
    source_format: &str,
    source_root: &str,
    source_path: &str,
    observed_at_ms: i64,
) -> SourceImportFile {
    SourceImportFile {
        provider,
        source_format: source_format.into(),
        source_root: source_root.into(),
        source_path: source_path.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        import_revision: 1,
        observed_at_ms,
        metadata: serde_json::json!({}),
    }
}

fn upsert_catalog_inventory(store: &Store, sessions: &[CatalogSession]) {
    let mut groups = BTreeMap::<(String, String), Vec<CatalogSession>>::new();
    for session in sessions {
        groups
            .entry((
                session.provider.as_str().to_owned(),
                session.source_root.clone(),
            ))
            .or_default()
            .push(session.clone());
    }
    for ((_, source_root), sessions) in groups {
        let provider = sessions[0].provider;
        let generation = store
            .allocate_catalog_inventory_generation(provider, &source_root)
            .unwrap();
        store
            .upsert_catalog_sessions(generation, &sessions)
            .unwrap();
    }
}

fn upsert_source_inventory(store: &Store, files: &[SourceImportFile]) {
    let mut groups = BTreeMap::<(String, String), Vec<SourceImportFile>>::new();
    for file in files {
        groups
            .entry((file.provider.as_str().to_owned(), file.source_root.clone()))
            .or_default()
            .push(file.clone());
    }
    for ((_, source_root), files) in groups {
        let provider = files[0].provider;
        let generation = store
            .allocate_source_import_inventory_generation(provider, &source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &files)
            .unwrap();
    }
}

fn current_inventory_generation(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    inventory_family: &str,
) -> u64 {
    store
        .conn
        .query_row(
            "SELECT current_generation FROM import_inventory_generations WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3",
            params![provider.as_str(), source_root, inventory_family],
            |row| row.get(0),
        )
        .unwrap()
}

fn current_catalog_generation(store: &Store, provider: CaptureProvider, source_root: &str) -> u64 {
    current_inventory_generation(store, provider, source_root, "catalog_sessions")
}

fn current_source_generation(store: &Store, provider: CaptureProvider, source_root: &str) -> u64 {
    current_inventory_generation(store, provider, source_root, "source_import_files")
}

fn insert_matching_checkpoint(store: &Store, file: &SourceImportFile) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO provider_file_checkpoints (
                provider, source_format, source_root, source_path, import_revision,
                checkpoint_version, stable_file_identity, committed_byte_offset,
                committed_complete_line_count, head_sha256, boundary_sha256, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'test-file', ?6, 0, ?7, ?8, ?9)
            "#,
            params![
                file.provider.as_str(),
                &file.source_format,
                &file.source_root,
                &file.source_path,
                i64::from(file.import_revision),
                file.file_size_bytes,
                "a".repeat(64),
                "b".repeat(64),
                file.observed_at_ms,
            ],
        )
        .unwrap();
}

include!("tests/pending_reasons.rs");
include!("tests/catalog_imports.rs");
include!("tests/source_imports.rs");
include!("tests/queries.rs");
