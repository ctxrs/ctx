#[allow(unused_imports)]
use super::*;

pub(crate) fn synthetic_codex_session_tree(root: &Path, sessions: usize) -> u64 {
    (0..sessions)
        .map(|index| write_synthetic_codex_session(root, index, "baseline"))
        .sum()
}

pub(crate) fn write_synthetic_codex_session(root: &Path, index: usize, marker: &str) -> u64 {
    let shard = format!("{:02}", index / 1000);
    let dir = root.join("2026").join("06").join("26").join(shard);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("synthetic-session-{index:06}.jsonl"));
    let seconds = index % 86_400;
    let timestamp = format!(
        "2026-06-26T{:02}:{:02}:{:02}.000Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    let session_id = format!("synthetic-codex-session-{index:06}");
    let meta = json!({
        "timestamp": timestamp,
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": timestamp,
            "cwd": "/repo/ctx",
            "originator": "codex-cli",
            "cli_version": "0.2.0-test",
            "source": "cli",
            "model_provider": "openai"
        }
    });
    let message = json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("incremental import synthetic corpus {index:06} {marker}")
            }]
        }
    });
    let body = format!("{meta}\n{message}\n");
    fs::write(&path, body.as_bytes()).unwrap();
    body.len() as u64
}

pub(crate) fn incremental_codex_catch_up(
    root: &Path,
    store: &mut Store,
    observed_at: DateTime<Utc>,
) -> IncrementalCatchUpSummary {
    let source_root = root.display().to_string();
    let catalog = catalog_codex_session_tree(
        root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(root.to_path_buf()),
            cataloged_at: observed_at,
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)
        .unwrap();
    let pending_sessions = pending.len();
    if pending.is_empty() {
        return IncrementalCatchUpSummary {
            catalog,
            import: ProviderImportSummary::default(),
            pending_sessions,
        };
    }

    let paths = pending
        .iter()
        .map(|session| PathBuf::from(&session.source_path))
        .collect::<Vec<_>>();
    let import = import_codex_session_paths(
        paths,
        store,
        CodexSessionImportOptions {
            source_path: Some(root.to_path_buf()),
            imported_at: observed_at,
            allow_partial_failures: false,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    let indexed_at_ms = observed_at.timestamp_millis();
    for session in pending {
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                ctx_history_store::CatalogSourceIndexUpdate {
                    source_root: &session.source_root,
                    source_path: &session.source_path,
                    file_size_bytes: session.file_size_bytes,
                    file_modified_at_ms: session.file_modified_at_ms,
                    file_sha256: None,
                    event_count: Some(1),
                    indexed_at_ms,
                },
            )
            .unwrap();
    }

    IncrementalCatchUpSummary {
        catalog,
        import,
        pending_sessions,
    }
}

pub(crate) fn incremental_perf_file_count() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_FILES").unwrap_or_else(|| {
        if env_flag("CTX_CODEX_INCREMENTAL_PERF_SLOW") {
            32_000
        } else {
            5_000
        }
    })
}

pub(crate) fn incremental_perf_repeats() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_REPEATS")
        .unwrap_or(5)
        .max(1)
}

pub(crate) fn incremental_perf_noop_p95_threshold_ms(file_count: usize) -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS").unwrap_or({
        if file_count >= 30_000 {
            1_000.0
        } else {
            500.0
        }
    })
}

pub(crate) fn incremental_perf_noop_us_per_file_threshold() -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE").unwrap_or(50.0)
}

#[test]
pub(crate) fn provider_fixture_replay_imports_codex_session_tree_and_is_idempotent() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let fixture = provider_fixture("codex.jsonl");
    let mut store = Store::open(&db_path).unwrap();

    let first =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 1);
    assert_eq!(first.skipped_events, 0);

    let second =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 3);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-1");
    let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-1-subagent-a");
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(
        parent.external_session_id.as_deref(),
        Some("codex-session-1")
    );
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
    assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
    drop(store);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 1);
    let (from_session_id, to_session_id, edge_type): (String, String, String) = conn
        .query_row(
            "SELECT from_session_id, to_session_id, edge_type FROM session_edges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(from_session_id, parent_id.to_string());
    assert_eq!(to_session_id, child_id.to_string());
    assert_eq!(edge_type, "parent_child");
}

#[test]
pub(crate) fn codex_session_tree_imports_messages_and_subagent_edges() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 8);
    assert_eq!(first.imported_edges, 1);

    let second = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 8);
    assert_eq!(second.skipped_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-root");
    let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-child");
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(parent.sync.fidelity, Fidelity::Imported);
    assert_eq!(
        parent.sync.metadata["source_format"].as_str(),
        Some("codex_session_jsonl")
    );
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(child.role_hint.as_deref(), Some("worker"));

    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 6);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Message
            && event.payload.to_string().contains("Fix the onboarding bug")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Message
            && event
                .payload
                .to_string()
                .contains("checking the setup flow")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Notice
            && event.payload.to_string().contains("task_complete")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.payload.to_string().contains("exec_command")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput
            && event
                .payload
                .to_string()
                .contains("all onboarding tests passed")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary
            && event
                .payload
                .to_string()
                .contains("provider history discovery")));
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 2);
    assert!(child_events
        .iter()
        .any(|event| event.payload.to_string().contains("local history search")));
}

#[test]
pub(crate) fn codex_session_catalog_large_noop_uses_metadata_cache() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let session_count = 1_024;
    synthetic_codex_session_tree(&root, session_count);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.source_files, session_count);
    assert_eq!(first.cataloged_sessions, session_count);
    assert_eq!(first.cached_sessions, 0);
    assert_eq!(first.parsed_sessions, session_count);
    assert_eq!(first.failed_sessions, 0);

    let second = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.source_files, session_count);
    assert_eq!(second.cataloged_sessions, session_count);
    assert_eq!(second.cached_sessions, session_count);
    assert_eq!(second.parsed_sessions, 0);
    assert_eq!(second.failed_sessions, 0);

    write_synthetic_codex_session(&root, 17, "changed-size-for-incremental-refresh");
    let third = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:02:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(third.source_files, session_count);
    assert_eq!(third.cataloged_sessions, session_count);
    assert_eq!(third.cached_sessions, session_count - 1);
    assert_eq!(third.parsed_sessions, 1);
    assert_eq!(third.failed_sessions, 0);
}

#[test]
pub(crate) fn codex_session_catalog_rejects_oversized_metadata_line() {
    let temp = tempdir();
    let root = temp.path().join("sessions/2026/07/03");
    fs::create_dir_all(&root).unwrap();
    write_oversized_jsonl_line(&root.join("oversized.jsonl"));
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = catalog_codex_session_tree(
        temp.path().join("sessions"),
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(temp.path().join("sessions")),
            cataloged_at: "2026-07-03T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("provider JSONL line exceeds"),
        "{err}"
    );
}

#[test]
pub(crate) fn codex_session_catalog_marks_deleted_paths_stale_when_additions_outnumber_deletions() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    synthetic_codex_session_tree(&root, 2);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = root.display().to_string();

    let first = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.cataloged_sessions, 2);

    fs::remove_file(
        root.join("2026/06/26/00")
            .join("synthetic-session-000000.jsonl"),
    )
    .unwrap();
    write_synthetic_codex_session(&root, 2, "addition-one");
    write_synthetic_codex_session(&root, 3, "addition-two");

    let second = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.source_files, 3);
    assert_eq!(second.cataloged_sessions, 3);
    assert_eq!(
        store
            .catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)
            .unwrap(),
        1
    );
}
