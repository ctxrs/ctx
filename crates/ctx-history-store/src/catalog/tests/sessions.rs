use super::*;

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
fn bounded_catalog_query_stops_after_the_max_plus_one_sentinel() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let sessions = (0..4)
        .map(|index| {
            catalog_session(
                &format!("/home/user/.codex/sessions/{index}.jsonl"),
                &format!("codex-session-{index}"),
                index,
            )
        })
        .collect::<Vec<_>>();
    store.upsert_catalog_sessions(&sessions).unwrap();
    // If the bounded query decodes beyond LIMIT max+1, this fourth row fails
    // JSON decoding before the intended cardinality error can be returned.
    store
        .conn
        .execute(
            "UPDATE catalog_sessions SET metadata_json = '{' WHERE source_path = ?1",
            [&sessions[3].source_path],
        )
        .unwrap();

    assert!(matches!(
        store.list_catalog_sessions_for_source_bounded(
            CaptureProvider::Codex,
            &sessions[0].source_root,
            2
        ),
        Err(StoreError::CatalogSessionLimitExceeded {
            observed: 3,
            maximum: 2
        })
    ));
}

#[test]
fn catalog_upsert_clears_completion_metadata_but_preserves_append_checkpoint() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/rollout.jsonl";
    let mut initial = catalog_session(source_path, "codex-session-1", cataloged_at_ms);
    initial.metadata["inventory_file_change_token_v1"] = serde_json::json!("initial-token");
    store
        .upsert_catalog_sessions(std::slice::from_ref(&initial))
        .unwrap();
    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(&initial, None, Some(3), cataloged_at_ms + 10)
        .unwrap();

    store
        .upsert_catalog_sessions(std::slice::from_ref(&initial))
        .unwrap();
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 1);

    let mut changed = catalog_session(source_path, "codex-session-1", cataloged_at_ms + 1);
    changed.file_size_bytes = 43;
    changed.metadata["inventory_file_change_token_v1"] = serde_json::json!("appended-token");
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
        let session = catalog_session(source_path, source_path, cataloged_at_ms);
        store
            .upsert_catalog_sessions(std::slice::from_ref(&session))
            .unwrap();
        store
            .upsert_session(&imported_session(source_path))
            .unwrap();
        store
            .mark_catalog_source_observation_indexed(&session, None, Some(3), cataloged_at_ms + 10)
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
fn catalog_upsert_repends_same_stat_file_when_observation_token_changes() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/rewritten.jsonl";
    let mut session = catalog_session(source_path, "codex-session-rewritten", cataloged_at_ms);
    session.metadata["inventory_file_change_token_v1"] = serde_json::json!("first-token");
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();
    store
        .upsert_session(&imported_session("codex-session-rewritten"))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(&session, None, Some(3), cataloged_at_ms + 10)
        .unwrap();

    let mut unchanged = session.clone();
    unchanged.cataloged_at_ms += 1;
    store.upsert_catalog_sessions(&[unchanged]).unwrap();
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 1);

    let mut rewritten = session;
    rewritten.cataloged_at_ms += 2;
    rewritten.metadata["inventory_file_change_token_v1"] = serde_json::json!("second-token");
    store.upsert_catalog_sessions(&[rewritten]).unwrap();

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 1);
    let (status, indexed_size, checkpoint_size): (String, Option<i64>, Option<i64>) = store
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
