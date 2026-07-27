use super::*;

#[test]
fn source_import_inventory_control_roundtrips_through_store_encoding() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.claude/projects";
    let file = SourceImportInventoryControl::new(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        root,
        42,
        900,
        1_000,
    )
    .reconciling_missing_file(
        Some("/home/user/.claude/projects/session-064.jsonl"),
        65,
        4_096,
    )
    .unwrap();

    assert!(file.is_inventory_control().unwrap());
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    let persisted = store
        .conn
        .query_row(
            source_import_file_select_sql(
                "WHERE provider = 'claude' AND source_root = source_path",
            )
            .as_str(),
            [],
            source_import_file_from_row,
        )
        .unwrap();

    assert_eq!(persisted, file);
    assert!(persisted.is_inventory_control().unwrap());
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
}

#[test]
fn source_import_inventory_control_rejects_malformed_missing_and_wrong_type_fields() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let valid = complete_inventory_control(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        "/home/user/.claude/projects",
        1_000,
    );
    let mut malformed_phase = valid.clone();
    malformed_phase.metadata["inventory_phase_v1"] = serde_json::json!("finishing");
    let mut missing_field = valid.clone();
    missing_field
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("inventory_stale_keyset_v1");
    let mut missing_marker = valid.clone();
    missing_marker
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("inventory_control_v1");
    let mut wrong_type = valid.clone();
    wrong_type.metadata["source_files"] = serde_json::json!("0");
    let mut wrong_marker_type = valid;
    wrong_marker_type.metadata["inventory_control_v1"] = serde_json::json!(1);

    for (case, file) in [
        ("malformed phase", malformed_phase),
        ("missing field", missing_field),
        ("missing marker", missing_marker),
        ("wrong field type", wrong_type),
        ("wrong marker type", wrong_marker_type),
    ] {
        assert!(
            file.is_inventory_control().is_err(),
            "{case} unexpectedly validated"
        );
        assert!(
            store
                .upsert_source_import_files(std::slice::from_ref(&file))
                .is_err(),
            "{case} unexpectedly persisted"
        );
    }
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
}

#[test]
fn source_import_manifest_upsert_refreshes_observation_without_repending() {
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
        .mark_source_import_file_indexed(&file, observed_at_ms + 10)
        .unwrap();
    file.observed_at_ms += 1_000;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    let refreshed_observed_at_ms: i64 = store
        .conn
        .query_row(
            "SELECT observed_at_ms FROM source_import_files WHERE source_path = ?1",
            params![file.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(refreshed_observed_at_ms, file.observed_at_ms);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, "/home/user/.claude/projects")
        .unwrap()
        .is_empty());
}

#[test]
fn source_import_pending_files_use_bounded_stable_keyset_pages() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.claude/projects";
    let files = (0..130)
        .rev()
        .map(|index| SourceImportFile {
            provider: CaptureProvider::Claude,
            source_format: "claude_projects_jsonl_tree".into(),
            source_root: root.into(),
            source_path: format!("{root}/session-{index:03}.jsonl"),
            file_size_bytes: index,
            file_modified_at_ms: index as i64,
            observed_at_ms: 1,
            metadata: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
    store.upsert_source_import_files(&files).unwrap();

    let mut after_source_path = None;
    let mut page_sizes = Vec::new();
    let mut source_paths = Vec::new();
    loop {
        let page = store
            .list_pending_source_import_files_page(
                CaptureProvider::Claude,
                root,
                after_source_path.as_deref(),
            )
            .unwrap();
        if page.is_empty() {
            break;
        }
        page_sizes.push(page.len());
        after_source_path = page.last().map(|file| file.source_path.clone());
        source_paths.extend(page.into_iter().map(|file| file.source_path));
    }

    assert_eq!(page_sizes, vec![64, 64, 2]);
    assert_eq!(source_paths.len(), 130);
    assert!(source_paths.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn source_import_observation_tokens_advance_past_persisted_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.claude/projects";
    store
        .upsert_source_import_files(&[
            complete_inventory_control(
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                root,
                100,
            ),
            SourceImportFile {
                provider: CaptureProvider::Claude,
                source_format: "claude_projects_jsonl_tree".into(),
                source_root: root.into(),
                source_path: format!("{root}/session.jsonl"),
                file_size_bytes: 1,
                file_modified_at_ms: 1,
                observed_at_ms: 1_000,
                metadata: serde_json::json!({}),
            },
        ])
        .unwrap();

    assert_eq!(
        store
            .next_source_import_observed_at_ms(CaptureProvider::Claude, root, 50)
            .unwrap(),
        101
    );
}

#[test]
fn source_root_inventory_change_token_marks_same_stat_source_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.hermes/state.db";
    let mut file = SourceImportFile {
        provider: CaptureProvider::Hermes,
        source_format: "hermes_state_sqlite".into(),
        source_root: root.into(),
        source_path: root.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        observed_at_ms,
        metadata: serde_json::json!({
            "inventory_unit": "source_root",
            "source_files": 1,
            "change_token_v1": "before",
        }),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(&file, observed_at_ms + 1)
        .unwrap();
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Hermes, root)
        .unwrap()
        .is_empty());

    file.metadata["change_token_v1"] = serde_json::json!("after");
    file.observed_at_ms += 1;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Hermes, root)
            .unwrap(),
        vec![file]
    );
}

#[test]
fn source_file_change_token_marks_same_stat_file_pending_and_remains_resettable() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.openhands";
    let mut file = SourceImportFile {
        provider: CaptureProvider::OpenHands,
        source_format: "openhands_file_events".into(),
        source_root: root.into(),
        source_path: format!("{root}/v1_conversations/conversation/0001-message.json"),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        observed_at_ms,
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
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(&file, observed_at_ms + 1)
        .unwrap();

    file.observed_at_ms += 1;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    assert!(store
        .list_pending_source_import_files(CaptureProvider::OpenHands, root)
        .unwrap()
        .is_empty());

    store
        .reset_source_import_files_pending(std::slice::from_ref(&file))
        .unwrap();
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::OpenHands, root)
            .unwrap(),
        vec![file.clone()]
    );
    store
        .mark_source_import_file_indexed(&file, observed_at_ms + 2)
        .unwrap();

    file.metadata["inventory_file_change_token_v1"]["ctime_nsec"] = serde_json::json!(7);
    file.observed_at_ms += 1;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::OpenHands, root)
            .unwrap(),
        vec![file]
    );
}

#[test]
fn source_import_format_change_marks_same_stat_source_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/agent/state.db";
    let mut file = SourceImportFile {
        provider: CaptureProvider::Custom,
        source_format: "old_format".into(),
        source_root: root.into(),
        source_path: root.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        observed_at_ms,
        metadata: serde_json::json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(&file, observed_at_ms + 1)
        .unwrap();

    file.source_format = "new_format".into();
    file.observed_at_ms += 1;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Custom, root)
            .unwrap(),
        vec![file]
    );
}
