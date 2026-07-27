use super::*;

#[test]
fn repeated_exact_catalog_completion_is_a_physical_noop() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/06/24/noop.jsonl",
        "codex-session-noop",
        cataloged_at_ms,
    );
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(
            &session,
            Some("same-prefix"),
            Some(3),
            cataloged_at_ms + 10,
        )
        .unwrap();
    let before = store.conn.total_changes();

    store
        .mark_catalog_source_observation_indexed(
            &session,
            Some("same-prefix"),
            Some(3),
            cataloged_at_ms + 20,
        )
        .unwrap();

    assert_eq!(store.conn.total_changes(), before);
    assert_eq!(
        store
            .catalog_source_index_state(
                CaptureProvider::Codex,
                &session.source_root,
                &session.source_path,
            )
            .unwrap()
            .unwrap()
            .last_imported_at_ms,
        Some(cataloged_at_ms + 10)
    );
}

#[test]
fn catalog_import_planning_requires_current_index_state_and_matching_session() {
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
    store
        .mark_catalog_source_observation_indexed(&session, None, Some(3), cataloged_at_ms + 10)
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
    let sessions = [
        catalog_session_for_root(first_root, first_path, external_session_id, cataloged_at_ms),
        catalog_session_for_root(
            second_root,
            second_path,
            external_session_id,
            cataloged_at_ms,
        ),
    ];
    store.upsert_catalog_sessions(&sessions).unwrap();
    for session in &sessions {
        store
            .mark_catalog_source_observation_indexed(session, None, Some(3), cataloged_at_ms + 10)
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
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
        cataloged_at_ms,
    );
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();

    store
        .mark_catalog_source_observation_failed(&session, "bad json", cataloged_at_ms + 10)
        .unwrap();

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
fn catalog_index_checkpoint_event_count_can_be_unknown() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/unknown-count.jsonl";
    let session = catalog_session(source_path, "codex-session-unknown-count", cataloged_at_ms);
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(
            &session,
            Some("abc123"),
            None,
            cataloged_at_ms + 10,
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

fn observation_catalog_session(source_path: &str, cataloged_at_ms: i64) -> CatalogSession {
    CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/home/user/.codex/sessions".to_owned(),
        source_path: source_path.to_owned(),
        external_session_id: Some("codex-session-observation".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: None,
        file_size_bytes: 42,
        file_modified_at_ms: cataloged_at_ms,
        cataloged_at_ms,
        metadata: serde_json::json!({"catalog_scope": "session_meta"}),
    }
}

#[test]
fn stale_catalog_completion_cannot_complete_newer_same_stat_observation() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = 1_782_213_600_000;
    let source_path = "/home/user/.codex/sessions/2026/06/24/raced.jsonl";
    let mut original = observation_catalog_session(source_path, cataloged_at_ms);
    original.metadata["inventory_file_change_token_v1"] = serde_json::json!("original-token");
    store
        .upsert_catalog_sessions(std::slice::from_ref(&original))
        .unwrap();

    let mut newer = original.clone();
    newer.cataloged_at_ms += 1;
    newer.metadata["inventory_file_change_token_v1"] = serde_json::json!("newer-token");
    store.upsert_catalog_sessions(&[newer]).unwrap();

    assert_observation_conflict(
        store.mark_catalog_source_observation_indexed(
            &original,
            Some("stale-prefix-hash"),
            Some(3),
            cataloged_at_ms + 10,
        ),
        "indexed",
        original.provider,
        &original.source_path,
    );
    assert_observation_conflict(
        store.mark_catalog_source_observation_failed(
            &original,
            "stale failure",
            cataloged_at_ms + 10,
        ),
        "failed",
        original.provider,
        &original.source_path,
    );

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.pending, 1);
}

#[test]
fn catalog_observation_completion_preserves_legacy_unversioned_behavior() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = 1_782_213_600_000;
    let source_path = "/home/user/.codex/sessions/2026/06/24/legacy.jsonl";
    let original = observation_catalog_session(source_path, cataloged_at_ms);
    store
        .upsert_catalog_sessions(std::slice::from_ref(&original))
        .unwrap();

    let mut recataloged = original.clone();
    recataloged.cataloged_at_ms += 1;
    recataloged.file_size_bytes += 1;
    store
        .upsert_catalog_sessions(std::slice::from_ref(&recataloged))
        .unwrap();

    store
        .mark_catalog_source_observation_failed(&original, "legacy failure", cataloged_at_ms + 10)
        .unwrap();

    let mut versioned = recataloged;
    versioned.cataloged_at_ms += 1;
    versioned.metadata["inventory_file_change_token_v1"] = serde_json::json!("versioned-token");
    store.upsert_catalog_sessions(&[versioned]).unwrap();

    assert_observation_conflict(
        store.mark_catalog_source_observation_failed(
            &original,
            "stale legacy failure",
            cataloged_at_ms + 20,
        ),
        "failed",
        original.provider,
        &original.source_path,
    );
}

#[test]
fn catalog_completion_without_local_projection_is_converged_only_for_thin_planning() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = 1_782_213_600_000;
    let session = observation_catalog_session(
        "/home/user/.codex/sessions/2026/06/24/thin.jsonl",
        cataloged_at_ms,
    );
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(
            &session,
            Some("thin-prefix-hash"),
            Some(3),
            cataloged_at_ms + 10,
        )
        .unwrap();

    assert_eq!(store.catalog_session_counts().unwrap().pending, 1);
    assert!(store
        .list_pending_catalog_sessions_without_local_projection(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
        )
        .unwrap()
        .is_empty());
    let thin_counts = store
        .catalog_session_counts_without_local_projection()
        .unwrap();
    assert_eq!(thin_counts.indexed, 1);
    assert_eq!(thin_counts.pending, 0);
}

#[test]
fn multiple_catalog_completion_matches_roll_back_before_conflict() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = 1_782_213_600_000;
    let mut session = observation_catalog_session(
        "/home/user/.codex/sessions/2026/06/24/duplicate.jsonl",
        cataloged_at_ms,
    );
    session.metadata["inventory_file_change_token_v1"] = serde_json::json!("current-token");
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    store
        .conn
        .execute_batch(
            r#"
            ALTER TABLE catalog_sessions RENAME TO catalog_sessions_unique;
            CREATE TABLE catalog_sessions AS
                SELECT * FROM catalog_sessions_unique WHERE 0;
            INSERT INTO catalog_sessions SELECT * FROM catalog_sessions_unique;
            INSERT INTO catalog_sessions SELECT * FROM catalog_sessions_unique;
            "#,
        )
        .unwrap();

    assert_observation_conflict(
        store.mark_catalog_source_observation_indexed(
            &session,
            Some("must-roll-back"),
            Some(3),
            cataloged_at_ms + 10,
        ),
        "indexed",
        session.provider,
        &session.source_path,
    );

    let untouched: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE indexed_status = 'pending' AND indexed_at_ms IS NULL AND last_imported_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(untouched, 2);
}
