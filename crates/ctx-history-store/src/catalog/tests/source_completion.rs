use super::*;

#[test]
fn stale_source_file_completion_cannot_complete_newer_same_stat_observation() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.openhands";
    let first = SourceImportFile {
        provider: CaptureProvider::OpenHands,
        source_format: "openhands_file_events".into(),
        source_root: root.into(),
        source_path: format!("{root}/v1_conversations/conversation/0001-message.json"),
        file_size_bytes: 42,
        file_modified_at_ms: 100,
        observed_at_ms: 1_000,
        metadata: serde_json::json!({
            "inventory_file_change_token_v1": {
                "algorithm": "unix-stat-v1",
                "dev": 1,
                "ino": 2,
                "mtime": 3,
                "mtime_nsec": 4,
                "ctime": 5,
                "ctime_nsec": 6,
                "size": 42,
            },
        }),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&first))
        .unwrap();

    let mut second = first.clone();
    second.observed_at_ms += 1;
    second.metadata["inventory_file_change_token_v1"]["ctime_nsec"] = serde_json::json!(7);
    store
        .upsert_source_import_files(std::slice::from_ref(&second))
        .unwrap();

    assert_observation_conflict(
        store.mark_source_import_file_indexed(&first, 2_000),
        "indexed",
        first.provider,
        &first.source_path,
    );
    assert_observation_conflict(
        store.mark_source_import_file_failed(&first, "stale failure", 2_001),
        "failed",
        first.provider,
        &first.source_path,
    );
    assert_eq!(
        store
            .list_pending_source_import_files(second.provider, root)
            .unwrap(),
        vec![second.clone()]
    );
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.failed, 0);

    store
        .mark_source_import_file_indexed(&second, 2_002)
        .unwrap();
    assert!(store
        .list_pending_source_import_files(second.provider, root)
        .unwrap()
        .is_empty());
}

#[test]
fn multiple_source_file_completion_matches_roll_back_before_conflict() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: CaptureProvider::OpenHands,
        source_format: "openhands_file_events".into(),
        source_root: "/home/user/.openhands".into(),
        source_path: "/home/user/.openhands/conversation/0001-message.json".into(),
        file_size_bytes: 42,
        file_modified_at_ms: 100,
        observed_at_ms: 1_000,
        metadata: serde_json::json!({"inventory_file_change_token_v1": "current-token"}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .conn
        .execute_batch(
            r#"
            ALTER TABLE source_import_files RENAME TO source_import_files_unique;
            CREATE TABLE source_import_files AS
                SELECT * FROM source_import_files_unique WHERE 0;
            INSERT INTO source_import_files SELECT * FROM source_import_files_unique;
            INSERT INTO source_import_files SELECT * FROM source_import_files_unique;
            "#,
        )
        .unwrap();

    assert_observation_conflict(
        store.mark_source_import_file_failed(&file, "must roll back", 2_000),
        "failed",
        file.provider,
        &file.source_path,
    );

    let untouched: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE indexed_status = 'pending' AND indexed_at_ms IS NULL AND indexed_error IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(untouched, 2);
}
