#[allow(unused_imports)]
use super::*;

pub(crate) fn catalog_session(
    source_path: &str,
    external_session_id: &str,
    mtime_ms: i64,
) -> CatalogSession {
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

pub(crate) fn imported_session(external_session_id: &str) -> Session {
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

#[test]
pub(crate) fn catalog_session_upsert_skips_unchanged_rows() {
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
pub(crate) fn catalog_upsert_clears_completion_metadata_but_preserves_append_checkpoint() {
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
pub(crate) fn catalog_index_checkpoint_event_count_can_be_unknown() {
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
