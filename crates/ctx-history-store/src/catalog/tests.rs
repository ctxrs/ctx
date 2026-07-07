use std::{fs, time::Duration};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource,
    CaptureSourceDescriptor, CaptureSourceKind, EntityTimestamps, Event, EventRole, EventType,
    Fidelity, RedactionState, Session, SessionStatus, SyncMetadata, SyncState, Visibility,
};
use rusqlite::{ffi::ErrorCode, params};
use uuid::Uuid;

use crate::catalog::{
    CatalogIndexedStatus, CatalogSession, CatalogSourceIndexUpdate, SourceImportFile,
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

fn imported_session(external_session_id: &str) -> Session {
    Session {
        id: new_id(),
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some(external_session_id.into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    }
}

fn source_scoped_imported_session(external_session_id: &str, source_id: Uuid) -> Session {
    Session {
        capture_source_id: Some(source_id),
        ..imported_session(external_session_id)
    }
}

fn imported_source(source_id: Uuid, source_root: &str, external_session_id: &str) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "test-machine".into(),
            process_id: None,
            cwd: Some("/repo".into()),
            raw_source_path: Some(format!("{source_root}/session.jsonl")),
            source_format: Some("codex_session_jsonl".into()),
            source_root: Some(source_root.into()),
            source_identity: None,
            external_session_id: Some(external_session_id.into()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync_metadata(),
    }
}

fn session_event(session_id: Uuid, index: u64) -> Event {
    Event {
        id: new_id(),
        seq: index,
        history_record_id: None,
        session_id: Some(session_id),
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time() + chrono::Duration::seconds(index as i64),
        capture_source_id: None,
        payload: serde_json::json!({"index": index}),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::LocalPreview,
        sync: sync_metadata(),
    }
}

fn artifact_record(id: Uuid, byte_size: u64) -> Artifact {
    Artifact {
        id,
        kind: ArtifactKind::Markdown,
        blob_hash: format!("{:064x}", 1),
        blob_path: format!("{OBJECTS_DIR}/00/test-artifact"),
        byte_size,
        media_type: Some("text/markdown".to_owned()),
        preview_text: Some("artifact preview".to_owned()),
        redaction_state: RedactionState::LocalPreview,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    }
}

fn assert_sql_conversion_error<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(result, Err(StoreError::Sql(_))),
        "expected sqlite conversion error, got {result:?}"
    );
}

#[test]
fn catalog_session_upsert_skips_unchanged_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
        cataloged_at_ms,
    );
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    let after_insert: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    let mut recataloged = session.clone();
    recataloged.cataloged_at_ms += 1_000;
    store
        .upsert_catalog_sessions(std::slice::from_ref(&recataloged))
        .unwrap();
    let after_noop: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_noop, after_insert);

    let mut changed = recataloged;
    changed.file_size_bytes += 1;
    changed.cataloged_at_ms += 1_000;
    store
        .upsert_catalog_sessions(std::slice::from_ref(&changed))
        .unwrap();
    let after_changed: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert!(after_changed > after_noop);
}

#[test]
fn events_for_session_window_returns_bounded_neighbors() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session = imported_session("window-session");
    store.upsert_session(&session).unwrap();
    let events = (0..10)
        .map(|index| {
            let event = session_event(session.id, index);
            store.upsert_event(&event).unwrap();
            event
        })
        .collect::<Vec<_>>();

    let middle = store
        .events_for_session_window(&events[5], 2, 3)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(middle, vec![3, 4, 5, 6, 7, 8]);

    let first = store
        .events_for_session_window(&events[0], 50, 1)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(first, vec![0, 1]);

    let last = store
        .events_for_session_window(&events[9], 1, 50)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(last, vec![8, 9]);
}

#[test]
fn sessions_by_external_session_limited_caps_ambiguity_scan() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    for index in 0..5 {
        let mut session = imported_session("shared-provider-session");
        session.started_at = fixed_time() + chrono::Duration::seconds(index);
        store.upsert_session(&session).unwrap();
    }

    let matches = store
        .sessions_by_external_session_limited(CaptureProvider::Codex, "shared-provider-session", 2)
        .unwrap();

    assert_eq!(matches.len(), 2);
    assert_eq!(
        matches[0].external_session_id.as_deref(),
        Some("shared-provider-session")
    );
}

#[test]
fn search_index_optimize_is_safe_on_initialized_store() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store.optimize_search_index().unwrap();
}

#[test]
fn catalog_sessions_count_indexed_and_stale_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap()
            .len(),
        1
    );

    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);

    store
        .mark_catalog_source_stale(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            cataloged_at_ms + 1,
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 0);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.pending, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        1
    );
}

#[test]
fn catalog_import_planning_requires_current_index_state_and_matching_session() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 0);

    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert!(pending.is_empty());
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);
}

#[test]
fn catalog_import_planning_scopes_matching_sessions_by_source_root() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let first_root = "/home/user/.codex/first/sessions";
    let second_root = "/home/user/.codex/second/sessions";
    let first_path = "/home/user/.codex/first/sessions/rollout.jsonl";
    let second_path = "/home/user/.codex/second/sessions/rollout.jsonl";
    let external_session_id = "shared-provider-session";
    store
        .upsert_catalog_sessions(&[
            catalog_session_for_root(first_root, first_path, external_session_id, cataloged_at_ms),
            catalog_session_for_root(
                second_root,
                second_path,
                external_session_id,
                cataloged_at_ms,
            ),
        ])
        .unwrap();
    for (source_root, source_path) in [(first_root, first_path), (second_root, second_path)] {
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root,
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();
    }

    let first_source_id = new_id();
    store
        .upsert_capture_source(&imported_source(
            first_source_id,
            first_root,
            external_session_id,
        ))
        .unwrap();
    store
        .upsert_session(&source_scoped_imported_session(
            external_session_id,
            first_source_id,
        ))
        .unwrap();

    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, first_root)
        .unwrap()
        .is_empty());
    let second_pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, second_root)
        .unwrap();
    assert_eq!(second_pending.len(), 1);
    assert_eq!(second_pending[0].source_path, second_path);
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 1);
}

#[test]
fn catalog_import_mark_failed_records_error_and_remains_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();

    let changed = store
        .mark_catalog_source_failed(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "bad json",
            cataloged_at_ms + 10,
        )
        .unwrap();
    assert_eq!(changed, 1);

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.pending, 1);
    let (status, error, indexed_at_ms): (String, Option<String>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_error, indexed_at_ms FROM catalog_sessions WHERE source_path = ?1",
            ["/home/user/.codex/sessions/2026/06/24/rollout.jsonl"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, CatalogIndexedStatus::Failed.as_str());
    assert_eq!(error.as_deref(), Some("bad json"));
    assert_eq!(indexed_at_ms, Some(cataloged_at_ms + 10));
}

#[test]
fn catalog_upsert_clears_completion_metadata_but_preserves_append_checkpoint() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/rollout.jsonl";
    store
        .upsert_catalog_sessions(&[catalog_session(
            source_path,
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();
    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    store
        .upsert_catalog_sessions(&[catalog_session(
            source_path,
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 1);

    let mut changed = catalog_session(source_path, "codex-session-1", cataloged_at_ms + 1);
    changed.file_size_bytes = 43;
    store.upsert_catalog_sessions(&[changed]).unwrap();

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 1);
    let (
        status,
        indexed_at_ms,
        indexed_size,
        indexed_mtime,
        indexed_event_count,
        checkpoint_at_ms,
        checkpoint_size,
        checkpoint_mtime,
        checkpoint_event_count,
    ): CatalogSessionCheckpointRow = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
            [source_path],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
    assert_eq!(indexed_at_ms, None);
    assert_eq!(indexed_size, None);
    assert_eq!(indexed_mtime, None);
    assert_eq!(indexed_event_count, None);
    assert_eq!(checkpoint_at_ms, Some(cataloged_at_ms + 10));
    assert_eq!(checkpoint_size, Some(42));
    assert_eq!(checkpoint_mtime, Some(cataloged_at_ms));
    assert_eq!(checkpoint_event_count, Some(3));

    let checkpoint = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.last_imported_file_size_bytes, Some(42));
    assert_eq!(
        checkpoint.last_imported_file_modified_at_ms,
        Some(cataloged_at_ms)
    );
    assert_eq!(checkpoint.last_imported_file_sha256, None);
    assert_eq!(checkpoint.last_imported_event_count, Some(3));
    assert_eq!(checkpoint.last_imported_at_ms, Some(cataloged_at_ms + 10));
}

#[test]
fn catalog_upsert_invalidates_checkpoint_for_shrink_and_same_size_change() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    for (source_path, file_size_bytes) in [
        ("/home/user/.codex/sessions/2026/06/24/shrink.jsonl", 41_u64),
        (
            "/home/user/.codex/sessions/2026/06/24/same-size.jsonl",
            42_u64,
        ),
    ] {
        store
            .upsert_catalog_sessions(&[catalog_session(source_path, source_path, cataloged_at_ms)])
            .unwrap();
        store
            .upsert_session(&imported_session(source_path))
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        let mut changed = catalog_session(source_path, source_path, cataloged_at_ms + 1);
        changed.file_size_bytes = file_size_bytes;
        store.upsert_catalog_sessions(&[changed]).unwrap();

        let (status, indexed_size, checkpoint_size): (String, Option<i64>, Option<i64>) =
            store
                .conn
                .query_row(
                    "SELECT indexed_status, indexed_file_size_bytes, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = ?1",
                    [source_path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
        assert_eq!(indexed_size, None);
        assert_eq!(checkpoint_size, None);
    }
}

#[test]
fn catalog_index_checkpoint_event_count_can_be_unknown() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/unknown-count.jsonl";
    store
        .upsert_catalog_sessions(&[catalog_session(
            source_path,
            "codex-session-unknown-count",
            cataloged_at_ms,
        )])
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: Some("abc123"),
                event_count: None,
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    let checkpoint = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.last_imported_event_count, None);
    assert_eq!(
        checkpoint.last_imported_file_sha256.as_deref(),
        Some("abc123")
    );
}

#[test]
fn source_import_manifest_upsert_ignores_observed_at_for_unchanged_files() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let mut file = SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: "claude_projects_jsonl_tree".into(),
        source_root: "/home/user/.claude/projects".into(),
        source_path: "/home/user/.claude/projects/session.jsonl".into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        observed_at_ms,
        metadata: serde_json::json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: "/home/user/.claude/projects",
                source_path: "/home/user/.claude/projects/session.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();
    let after_indexed: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    file.observed_at_ms += 1_000;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    let after_noop: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_noop, after_indexed);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, "/home/user/.claude/projects")
        .unwrap()
        .is_empty());
}

#[test]
fn source_import_file_counts_track_pending_indexed_failed_and_stale() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.claude/projects";
    let files = ["indexed.jsonl", "pending.jsonl", "failed.jsonl"]
        .into_iter()
        .map(|name| SourceImportFile {
            provider: CaptureProvider::Claude,
            source_format: "claude_projects_jsonl_tree".into(),
            source_root: root.into(),
            source_path: format!("{root}/{name}"),
            file_size_bytes: 42,
            file_modified_at_ms: observed_at_ms,
            observed_at_ms,
            metadata: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    store.upsert_source_import_files(&files).unwrap();
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &files[0].source_path,
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();
    store
        .mark_source_import_file_failed(
            CaptureProvider::Claude,
            root,
            &files[2].source_path,
            "bad json",
            observed_at_ms + 20,
        )
        .unwrap();
    store
        .mark_source_import_missing_paths_stale(
            CaptureProvider::Claude,
            root,
            &[files[0].source_path.clone(), files[2].source_path.clone()],
            observed_at_ms + 30,
        )
        .unwrap();

    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 1);

    let mut changed_indexed = files[0].clone();
    changed_indexed.file_size_bytes = 43;
    changed_indexed.observed_at_ms = observed_at_ms + 40;
    store
        .upsert_source_import_files(&[changed_indexed])
        .unwrap();

    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 2);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 1);
}

#[test]
fn catalog_schema_includes_import_state_columns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    assert!(schema.contains("indexed_at_ms INTEGER"));
    assert!(schema.contains("indexed_file_size_bytes INTEGER"));
    assert!(schema.contains("indexed_file_modified_at_ms INTEGER"));
    assert!(schema.contains("indexed_status TEXT NOT NULL DEFAULT 'pending'"));
    assert!(schema.contains("indexed_error TEXT"));
    assert!(schema.contains("indexed_event_count INTEGER"));
    assert!(schema.contains("last_imported_at_ms INTEGER"));
    assert!(schema.contains("last_imported_file_size_bytes INTEGER"));
    assert!(schema.contains("last_imported_file_modified_at_ms INTEGER"));
    assert!(schema.contains("last_imported_file_sha256 TEXT"));
    assert!(schema.contains("last_imported_event_count INTEGER"));
}

#[test]
fn raw_sql_query_reads_stable_views() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    for view in [
        "CREATE VIEW ctx_sessions",
        "CREATE VIEW ctx_events",
        "CREATE VIEW ctx_files_touched",
        "CREATE VIEW ctx_sources",
    ] {
        assert!(schema.contains(view), "schema missing {view}");
    }

    let result = store
        .raw_sql_query(
            "SELECT COUNT(*) AS session_count FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "session_count");
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
}

#[test]
fn ctx_files_touched_resolves_session_from_source_id() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = "018f45d0-0000-7000-8000-000000080001";
    let source_id = "018f45d0-0000-7000-8000-000000080002";
    let session_id = "018f45d0-0000-7000-8000-000000080003";
    let touch_id = "018f45d0-0000-7000-8000-000000080004";
    let detached_source_id = "018f45d0-0000-7000-8000-000000080005";
    let detached_touch_id = "018f45d0-0000-7000-8000-000000080006";

    store
        .conn
        .execute(
            r#"
            INSERT INTO history_records
            (id, title, last_activity_at_ms, created_at_ms, updated_at_ms, body, created_at, updated_at)
            VALUES (?1, 'Touched file view record', 1, 1, 1, '', '', '')
            "#,
            [record_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/tmp/session.jsonl', 'codex-session-1', 1, 'imported')
            "#,
            [source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'opencode', 'test-machine', '/tmp/opencode.db', 'opencode-session-1', 1, 'imported')
            "#,
            [detached_source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO sessions
            (
                id, history_record_id, capture_source_id, provider, external_session_id,
                agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, 'codex', 'codex-session-1', 'primary', 1, 'imported', 'imported', 1, 1, 1)
            "#,
            params![session_id, record_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'src/main.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![touch_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'detached.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![detached_touch_id, detached_source_id],
        )
        .unwrap();

    let result = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'src/main.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "codex".to_owned(),
            bytes: 5,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Text {
            value: "codex-session-1".to_owned(),
            bytes: 15,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][2],
        RawSqlValue::Text {
            value: session_id.to_owned(),
            bytes: session_id.len(),
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][3],
        RawSqlValue::Text {
            value: record_id.to_owned(),
            bytes: record_id.len(),
            truncated: false,
        }
    );

    let detached = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'detached.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(detached.returned_rows, 1);
    assert_eq!(
        detached.rows[0][0],
        RawSqlValue::Text {
            value: "opencode".to_owned(),
            bytes: 8,
            truncated: false,
        }
    );
    assert_eq!(
        detached.rows[0][1],
        RawSqlValue::Text {
            value: "opencode-session-1".to_owned(),
            bytes: 18,
            truncated: false,
        }
    );
    assert_eq!(detached.rows[0][2], RawSqlValue::Null);
    assert_eq!(detached.rows[0][3], RawSqlValue::Null);
}

#[test]
fn raw_sql_query_rejects_writes_parameters_and_multiple_statements() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        store
            .raw_sql_query("", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlEmpty
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT ?1", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlHasParameters
    ));
    assert!(matches!(
        store
            .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlNotReadOnly
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::Sql(rusqlite::Error::MultipleStatement)
    ));
}

#[test]
fn raw_sql_query_caps_rows_and_values() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
        .raw_sql_query(
            "SELECT 'abcdef' AS text_value, X'01020304' AS blob_value UNION ALL SELECT 'ghijkl', X'05060708'",
            RawSqlOptions {
                max_rows: 1,
                max_value_bytes: 3,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.columns[0].name, "text_value");
    assert_eq!(result.columns[1].name, "blob_value");
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "abc".to_owned(),
            bytes: 6,
            truncated: true,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Blob {
            bytes: 4,
            preview_hex: "010203".to_owned(),
            truncated: true,
        }
    );
    assert!(result.truncated.rows);
    assert!(result.truncated.values);
}

#[test]
fn row_readers_reject_negative_unsigned_columns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let bad_process_id = new_id();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                external_session_id, started_at_ms, fidelity, sync_version
            )
            VALUES (?1, 'provider_import', 'codex', 'test-machine', -1, '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', 0)
            "#,
            params![bad_process_id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_capture_source(bad_process_id));

    let bad_sync_version = new_id();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, cwd, raw_source_path,
                external_session_id, started_at_ms, fidelity, sync_version
            )
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', -1)
            "#,
            params![bad_sync_version.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_capture_source(bad_sync_version));

    let event = Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({"text": "negative seq marker"}),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::LocalPreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();
    store
        .conn
        .execute(
            "UPDATE events SET seq = -1 WHERE id = ?1",
            params![event.id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_event(event.id));
    assert_sql_conversion_error(store.search_event_hits("negative seq marker", 1));

    let artifact = artifact_record(new_id(), 1);
    store.upsert_artifact(&artifact).unwrap();
    store
        .conn
        .execute(
            "UPDATE artifacts SET byte_size = -1 WHERE id = ?1",
            params![artifact.id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.list_artifacts());
}

#[test]
fn raw_sql_query_rejects_excessive_result_preview_budget() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
        .map(|index| format!("1 AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let err = store
        .raw_sql_query(
            &format!("SELECT {many_columns}"),
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::RawSqlResultBudgetTooLarge {
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
            ..
        }
    ));
}

#[test]
fn raw_sql_query_budgets_against_actual_column_count() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
        .raw_sql_query(
            "SELECT 1",
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
}

#[test]
fn raw_sql_query_times_out_long_running_queries() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
        .raw_sql_query(
            r#"
            WITH RECURSIVE numbers(x) AS (
                SELECT 1
                UNION ALL
                SELECT x + 1 FROM numbers WHERE x < 100000000
            )
            SELECT sum(x) FROM numbers
            "#,
            RawSqlOptions {
                timeout: Duration::from_millis(1),
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::RawSqlTimedOut { .. }));
}

#[test]
fn raw_sql_query_enforces_sqlite_value_length_limit() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
        .raw_sql_query(
            "SELECT length(randomblob(200000))",
            RawSqlOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Sql(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::TooBig
    ));
}
