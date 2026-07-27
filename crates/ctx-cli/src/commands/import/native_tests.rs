use super::*;
use crate::commands::import::inventory_import_sources;
use crate::provider_sources::explicit_path_source;
use ctx_history_capture::{
    import_cline_task_json_history, ClineTaskJsonImportOptions, ImportProfile,
    OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError,
};
use ctx_history_core::{
    new_id, CaptureProvider, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState,
    Visibility,
};
use ctx_history_store::{SourceImportFile, StoreError};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-native-import-")
        .tempdir_in(temp_root)
        .unwrap()
}

fn assert_openhands_pinned_reader_allows_cli_cursor(manifested: bool) {
    let temp = tempdir();
    let source_root = temp.path().join("openhands-user");
    let conversation = source_root.join("v1_conversations").join("cli-bulk-guard");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("0001-message.json"),
        json!({
            "id": "cli-bulk-guard-event",
            "timestamp": "2026-07-18T12:00:00Z",
            "source": "user",
            "llm_message": {"role": "user", "content": "guard ownership"}
        })
        .to_string(),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::OpenHands, source_root);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let preinventory = if manifested {
        inventory_source_import_files(&store, &source, false).unwrap();
        SourcePreinventory::SourceImportManifest
    } else {
        SourcePreinventory::None
    };
    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();

    let imported =
        import_one_source_inner(&mut store, &source, None, false, !manifested, &preinventory)
            .expect("pinned readers do not block the bounded maintenance handoff");
    assert_eq!(imported.imported_events, 1);
    let cursor_count: i64 = reader
        .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        cursor_count, 0,
        "the pinned read snapshot remains unchanged"
    );
    let current = Connection::open(&db_path).unwrap();
    let cursor_count: i64 = current
        .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cursor_count, 1, "the writer published its cursor");
    let maintenance_pending: i64 = current
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = 'event_search_maintenance_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(maintenance_pending, 1);
    reader.execute_batch("ROLLBACK").unwrap();

    let retry =
        import_one_source_inner(&mut store, &source, None, false, !manifested, &preinventory)
            .expect("retry finishes pending maintenance and publishes the cursor");
    assert_eq!(retry.imported_events, 0);
    assert_eq!(retry.failed, 0);
    let cursor_count: i64 = reader
        .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cursor_count, 1);
    let bulk_state_count: i64 = reader
        .query_row(
            "SELECT COUNT(*) FROM search_projection_stats WHERE key LIKE 'event_search_bulk_mode_v1%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bulk_state_count, 0);
    if manifested {
        assert!(store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string(),)
            .unwrap()
            .is_empty());
    }
}

fn assert_source_import_observation_conflict(
    error: &anyhow::Error,
    operation: &'static str,
    provider: CaptureProvider,
    source_path: &Path,
) {
    assert!(
        error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<StoreError>(),
                Some(StoreError::SourceImportObservationConflict {
                    operation: actual_operation,
                    provider: actual_provider,
                    source_path: actual_source_path,
                }) if *actual_operation == operation
                    && actual_provider == provider.as_str()
                    && actual_source_path == &source_path.display().to_string()
            )
        }),
        "unexpected inventory race error: {error:#}"
    );
    assert_eq!(import_error_scope(error), ImportFailureScope::System);
}

fn pending_source_root_observation(store: &Store, source: &SourceInfo) -> SourceImportFile {
    let source_path = source.path.display().to_string();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_path.clone(),
        source_path,
        file_size_bytes: 42,
        file_modified_at_ms: 100,
        observed_at_ms: 1_000,
        metadata: json!({"inventory_unit": "source_root", "change_token_v1": "original"}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    file
}

fn race_inventory_observation_after_history_record_insert(db_path: &Path) {
    let connection = Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER race_inventory_observation_after_history_record_insert
            AFTER INSERT ON history_records
            BEGIN
                UPDATE source_import_files
                SET observed_at_ms = observed_at_ms + 1;
            END;
            "#,
        )
        .unwrap();
}

fn assert_inventory_race_winner_remains_pending(db_path: &Path, original: &SourceImportFile) {
    let connection = Connection::open(db_path).unwrap();
    let state: (i64, String, Option<String>) = connection
        .query_row(
            r#"
            SELECT observed_at_ms, indexed_status, indexed_error
            FROM source_import_files
            WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
            "#,
            rusqlite::params![
                original.provider.as_str(),
                original.source_root,
                original.source_path
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        state,
        (original.observed_at_ms + 1, "pending".to_owned(), None)
    );
}

#[test]
fn direct_source_import_does_not_nest_nativepath_maintenance() {
    assert_openhands_pinned_reader_allows_cli_cursor(false);
}

#[test]
fn manifested_source_import_does_not_nest_nativepath_maintenance() {
    assert_openhands_pinned_reader_allows_cli_cursor(true);
}

#[test]
fn manifested_source_import_collapses_file_groups_into_one_fts_handoff() {
    let temp = tempdir();
    let source_root = temp.path().join("openhands-user");
    let conversation = source_root
        .join("v1_conversations")
        .join("collapsed-bulk-guard");
    fs::create_dir_all(&conversation).unwrap();
    for index in 0..7 {
        fs::write(
            conversation.join(format!("{index:04}-message.json")),
            json!({
                "id": format!("collapsed-bulk-guard-event-{index}"),
                "timestamp": "2026-07-18T12:00:00Z",
                "source": "user",
                "llm_message": {
                    "role": "user",
                    "content": format!("guard ownership {index}")
                }
            })
            .to_string(),
        )
        .unwrap();
    }
    let source = explicit_path_source(CaptureProvider::OpenHands, source_root);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    inventory_source_import_files(&store, &source, false).unwrap();

    let imported = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        false,
        &SourcePreinventory::SourceImportManifest,
    )
    .unwrap();
    assert_eq!(imported.imported_events, 7);

    let maintenance_groups: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT value FROM search_projection_stats \
             WHERE key = 'event_search_maintenance_v1:groups'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        maintenance_groups, 1,
        "one source import should schedule one FTS maintenance handoff"
    );
}

#[test]
fn manifested_codex_groups_publish_before_the_manifest_page_finishes() {
    let temp = tempdir();
    let source_path = temp.path().join("multi-group-session.jsonl");
    let mut transcript = format!(
        "{}\n",
        json!({
            "timestamp": "2026-07-18T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "019f5a54-67de-7422-9841-e9872df75f44",
                "timestamp": "2026-07-18T12:00:00Z",
                "cwd": "/workspace/ctx",
                "originator": "codex-cli"
            }
        })
    );
    for index in 0..300 {
        transcript.push_str(&format!(
            "{}\n",
            json!({
                "timestamp": "2026-07-18T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": [{
                        "type": if index % 2 == 0 { "input_text" } else { "output_text" },
                        "text": format!("bounded manifest message {index}")
                    }]
                }
            })
        ));
    }
    fs::write(&source_path, transcript).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    inventory_source_import_files(&store, &source, false).unwrap();

    let observed_committed_group = Arc::new(AtomicBool::new(false));
    let callback_observation = Arc::clone(&observed_committed_group);
    let callback_db = db_path.clone();
    let imported = import_one_source_inner(
        &mut store,
        &source,
        Some(Arc::new(move |progress| {
            if progress.done || progress.imported_events == 0 {
                return;
            }
            let reader = Connection::open(&callback_db).unwrap();
            let cursor_count: i64 = reader
                .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
                .unwrap();
            let event_count: i64 = reader
                .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
                .unwrap();
            callback_observation.store(cursor_count > 0 && event_count > 0, Ordering::SeqCst);
        })),
        false,
        false,
        &SourcePreinventory::SourceImportManifest,
    )
    .unwrap();

    assert_eq!(imported.imported_sessions, 1);
    assert_eq!(imported.imported_events, 300);
    assert!(
        observed_committed_group.load(Ordering::SeqCst),
        "a manifest page must not hide bounded provider-group commits"
    );
}

#[test]
fn manifested_completion_surfaces_newer_inventory_observation() {
    let temp = tempdir();
    let source_root = temp.path().join("openhands-user");
    let conversation = source_root.join("v1_conversations").join("inventory-race");
    fs::create_dir_all(&conversation).unwrap();
    let source_path = conversation.join("0001-message.json");
    fs::write(
        &source_path,
        json!({
            "id": "inventory-race-event",
            "timestamp": "2026-07-18T12:00:00Z",
            "source": "user",
            "llm_message": {"role": "user", "content": "inventory race"}
        })
        .to_string(),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::OpenHands, source_root);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    inventory_source_import_files(&store, &source, false).unwrap();
    let source_root_text = source.path.display().to_string();
    let original = store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap()
        .pop()
        .unwrap();
    let connection = Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER mutate_source_import_observation_after_event
            AFTER INSERT ON events
            BEGIN
                UPDATE source_import_files
                SET observed_at_ms = observed_at_ms + 1
                WHERE provider = 'openhands'
                  AND source_path != source_root;
            END;
            "#,
        )
        .unwrap();
    drop(connection);

    let error = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        false,
        &SourcePreinventory::SourceImportManifest,
    )
    .expect_err("stale manifested completion must surface a retryable conflict");

    assert_source_import_observation_conflict(&error, "indexed", source.provider, &source_path);
    let pending = store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_path, source_path.display().to_string());
    assert!(pending[0].observed_at_ms > original.observed_at_ms);
}

#[test]
fn source_root_completion_surfaces_newer_inventory_observation() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 42,
        file_modified_at_ms: 100,
        observed_at_ms: 1_000,
        metadata: json!({"change_token_v1": "original"}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&original))
        .unwrap();
    let mut newer = original.clone();
    newer.observed_at_ms += 1;
    newer.metadata["change_token_v1"] = json!("newer");
    store
        .upsert_source_import_files(std::slice::from_ref(&newer))
        .unwrap();
    let preinventory = SourcePreinventory::SourceRoot(original);

    let indexed_error = mark_source_root_inventory_indexed(&store, &preinventory)
        .expect_err("stale source-root indexed completion must conflict");
    assert_source_import_observation_conflict(
        &indexed_error,
        "indexed",
        source.provider,
        &source_path,
    );
    let failed_error = mark_source_root_inventory_failed(&store, &preinventory, "source error")
        .expect_err("stale source-root failed completion must conflict");
    assert_source_import_observation_conflict(
        &failed_error,
        "failed",
        source.provider,
        &source_path,
    );
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &source_path.display().to_string())
            .unwrap(),
        vec![newer]
    );
}

#[test]
fn indexed_inventory_race_cleans_replay_only_history_record_without_marking_winner() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("cline-data");
    let task = source_path.join("tasks/replay-only");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        json!({
            "taskId": "replay-only",
            "createdAt": "2026-07-18T12:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        json!([{
            "role": "user",
            "content": [{"type": "text", "text": "inventory race replay"}]
        }])
        .to_string(),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Cline, source_path.clone());
    let mut store = Store::open(&db_path).unwrap();
    let first = import_cline_task_json_history(
        &source_path,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(source_path.clone()),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert!(first.imported_events > 0);
    let original = pending_source_root_observation(&store, &source);
    race_inventory_observation_after_history_record_insert(&db_path);

    let error = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot(original.clone()),
    )
    .expect_err("newer source-root observation must win replay-only completion");

    assert_source_import_observation_conflict(&error, "indexed", source.provider, &source.path);
    assert!(!history_record_exists(&store, import_record_for_source(&source).id).unwrap());
    assert_inventory_race_winner_remains_pending(&db_path, &original);
}

#[test]
fn failed_inventory_race_cleans_rejected_history_record_without_marking_winner() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("cline-data");
    let task = source_path.join("tasks/all-rejected");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        r#"{"taskId":"all-rejected","createdAt":"2026-07-18T12:00:00Z"}"#,
    )
    .unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        "[{\"role\":\"user\"",
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Cline, source_path);
    let mut store = Store::open(&db_path).unwrap();
    let original = pending_source_root_observation(&store, &source);
    race_inventory_observation_after_history_record_insert(&db_path);

    let error = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot(original.clone()),
    )
    .expect_err("newer source-root observation must win rejected completion");

    assert_source_import_observation_conflict(&error, "failed", source.provider, &source.path);
    assert!(!history_record_exists(&store, import_record_for_source(&source).id).unwrap());
    assert_inventory_race_winner_remains_pending(&db_path, &original);
}

fn persist_indexed_root(
    store: &Store,
    source: &SourceInfo,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
) -> SourceImportFile {
    let source_root = source.path.display().to_string();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_root.clone(),
        source_path: source_root.clone(),
        file_size_bytes,
        file_modified_at_ms,
        observed_at_ms: 0,
        metadata: json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store.mark_source_import_file_indexed(&file, 1).unwrap();
    file
}

struct RecordingProOutputSink {
    observations: AtomicUsize,
    pages: AtomicUsize,
    fail_pages: bool,
    behind: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
}

impl RecordingProOutputSink {
    fn new(fail_pages: bool) -> Self {
        Self {
            observations: AtomicUsize::new(0),
            pages: AtomicUsize::new(0),
            fail_pages,
            behind: AtomicBool::new(false),
            progress: Mutex::new(None),
        }
    }
}

impl ProOutputSink for RecordingProOutputSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "cli-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        self.progress
            .lock()
            .map_err(|_| ProOutputSinkError::new("test_lock", "test progress lock poisoned"))
            .map(|progress| progress.clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.pages.fetch_add(1, Ordering::SeqCst);
        if self.fail_pages {
            return Err(ProOutputSinkError::new(
                "helper_crashed",
                "simulated Pro output failure",
            ));
        }
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        let committed_cursor = page.next_safe_cursor.clone();
        *self
            .progress
            .lock()
            .map_err(|_| ProOutputSinkError::new("test_lock", "test progress lock poisoned"))? =
            Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision,
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision,
                materializer_revision: page.materializer_revision,
                terminal: page.terminal,
            });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

#[test]
fn unchanged_codex_catalog_session_replays_on_first_pro_activation_without_core_rewrite() {
    let temp = tempdir();
    let source_root = temp.path().join("codex-sessions");
    fs::create_dir_all(&source_root).unwrap();
    let session_path = source_root.join("session.jsonl");
    let transcript = [
        json!({
            "timestamp": "2026-07-23T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "019f92f1-0000-7000-8000-000000000001",
                "timestamp": "2026-07-23T12:00:00Z",
                "cwd": "/workspace/ctx",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "catalog-call",
                "arguments": "{\"cmd\":\"printf catalog\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-23T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "catalog-call",
                "output": "unchanged catalog output"
            }
        }),
    ]
    .into_iter()
    .map(|record| format!("{record}\n"))
    .collect::<String>();
    fs::write(&session_path, transcript).unwrap();

    let source = explicit_path_source(CaptureProvider::Codex, source_root.clone());
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let first_inventory =
        inventory_import_sources(&store, vec![source.clone()], false, false).unwrap();
    assert_eq!(first_inventory.catalog.parsed_sessions, 1);
    let first_plan = first_inventory.sources.into_iter().next().unwrap();
    let first = import_one_source_for_search_refresh(
        &mut store,
        &first_plan.source,
        None,
        &first_plan.preinventory,
    )
    .unwrap();
    assert_eq!(first.failed, 0);
    assert!(first.imported_sessions > 0);
    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root.display().to_string())
        .unwrap()
        .is_empty());

    let second_inventory =
        inventory_import_sources(&store, vec![source.clone()], true, true).unwrap();
    assert_eq!(second_inventory.catalog.cached_sessions, 1);
    assert_eq!(second_inventory.catalog.parsed_sessions, 0);
    let available_catalog_rows: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row(
            "select count(*) from catalog_sessions where source_path = ?1 and is_stale = 0",
            [session_path.display().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(available_catalog_rows, 1);
    let second_plan = second_inventory.sources.into_iter().next().unwrap();
    let core_sessions_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from sessions", [], |row| row.get(0))
        .unwrap();
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(false));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let catch_up = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &second_plan.source,
        None,
        &second_plan.preinventory,
        &profile,
    )
    .unwrap();

    assert_eq!(catch_up.failed, 0);
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_sessions_before
    );
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_events_before
    );
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(!sink.behind.load(Ordering::SeqCst));
    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root.display().to_string())
        .unwrap()
        .is_empty());
}

fn imported_hermes_source() -> (tempfile::TempDir, SourceInfo, std::path::PathBuf) {
    let temp = tempdir();
    let source_path = temp.path().join("hermes.db");
    let source_db = Connection::open(&source_path).unwrap();
    source_db
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 started_at real not null
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 tool_call_id text,
                 timestamp real not null,
                 finish_reason text
             );
             insert into sessions values ('session-id', 'acp', 1782259200.0);
             insert into messages values (
                 1, 'session-id', 'tool', 'complete output', 'call-id',
                 1782259201.0, 'success'
             );",
        )
        .unwrap();
    drop(source_db);

    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    let summary =
        import_one_source_for_search_refresh(&mut store, &plan.source, None, &plan.preinventory)
            .unwrap();
    assert!(summary.imported_sessions > 0);
    let cursor_count: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert!(cursor_count > 0);
    (temp, source, db_path)
}

#[test]
fn first_pro_activation_replays_an_unchanged_core_source_then_catch_up_is_a_noop() {
    let (_temp, source, db_path) = imported_hermes_source();
    let mut store = Store::open(&db_path).unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    assert!(store
        .list_pending_source_import_files(source.provider, &source.path.display().to_string())
        .unwrap()
        .is_empty());
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(false));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let summary = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    )
    .unwrap();

    let core_events_after: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(core_events_after, core_events_before);
    assert_eq!(summary.failed, 0);
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(!sink.behind.load(Ordering::SeqCst));

    let pages_after_activation = sink.pages.load(Ordering::SeqCst);
    let second = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(
        sink.pages.load(Ordering::SeqCst),
        pages_after_activation,
        "an unchanged source at its terminal private cursor must not materialize another page"
    );
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_events_before
    );
}

#[test]
fn pro_output_failure_does_not_fail_unchanged_core_import() {
    let (_temp, source, db_path) = imported_hermes_source();
    let mut store = Store::open(&db_path).unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(true));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let result = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    );

    assert!(result.is_ok(), "{result:?}");
    let core_events_after: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(core_events_after, core_events_before);
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(sink.behind.load(Ordering::SeqCst));
}

#[test]
fn unchanged_root_source_skips_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);

    let summary = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot(file),
    )
    .unwrap();

    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 0);
}

#[test]
fn background_refresh_publishes_one_group_and_restart_resumes_pending_manifest() {
    let temp = tempdir();
    let source_path = temp.path().join("pi-session.jsonl");
    let mut lines = vec![json!({
        "type": "session",
        "id": "bounded-daemon-pi",
        "version": 3,
        "timestamp": "2026-07-20T12:00:00Z",
        "cwd": "/workspace"
    })
    .to_string()];
    for index in 0..320 {
        lines.push(
            json!({
                "type": "message",
                "id": format!("bounded-daemon-event-{index}"),
                "timestamp": "2026-07-20T12:00:01Z",
                "message": {
                    "role": "user",
                    "content": format!("bounded daemon message {index}")
                }
            })
            .to_string(),
        );
    }
    fs::write(&source_path, format!("{}\n", lines.join("\n"))).unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path.clone());
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    inventory_source_import_files(&store, &source, false).unwrap();

    let first = import_one_source_for_background_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceImportManifest,
    )
    .unwrap();

    assert!(first.work_remaining);
    assert_eq!(first.imported_events, 255);
    assert!(serde_json::to_value(&first)
        .unwrap()
        .get("work_remaining")
        .is_none());
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string())
            .unwrap()
            .len(),
        1,
        "a partial captured source must not be marked indexed"
    );
    drop(store);

    let mut restarted = Store::open(&db_path).unwrap();
    let second = import_one_source_for_background_refresh(
        &mut restarted,
        &source,
        None,
        &SourcePreinventory::SourceImportManifest,
    )
    .unwrap();

    assert!(!second.work_remaining);
    assert_eq!(second.imported_events, 65);
    assert!(restarted
        .list_pending_source_import_files(source.provider, &source.path.display().to_string())
        .unwrap()
        .is_empty());
    drop(restarted);
    let connection = Connection::open(&db_path).unwrap();
    let event_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_count, 320);
}

#[test]
fn unchanged_root_source_still_repairs_event_search_backfill() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let store = Store::open(&db_path).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);
    let event = Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({"text": "unchanged root backfill oracle"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    };
    store.upsert_event(&event).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM event_search", []).unwrap();
    drop(conn);
    let mut store = Store::open(&db_path).unwrap();
    assert!(store.event_search_projection_needs_backfill().unwrap());

    import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot(file),
    )
    .unwrap();

    assert!(!store.event_search_projection_needs_backfill().unwrap());
    assert_eq!(
        store
            .search_event_hits("unchanged root backfill oracle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn changed_root_source_does_not_skip_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    persist_indexed_root(&store, &source, 0, 0);
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let changed = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 21,
        file_modified_at_ms: 1,
        observed_at_ms: 1,
        metadata: json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&changed))
        .unwrap();

    let result = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot(changed),
    );

    assert!(
        result.is_err(),
        "changed source must reach the Hermes adapter"
    );
}

#[test]
fn full_rescan_does_not_skip_unchanged_root_source() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = persist_indexed_root(&store, &source, 21, 1);

    let result = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot(file),
    );

    assert!(result.is_err(), "full rescan must reach the Hermes adapter");
}

#[test]
fn manifested_pending_context_keeps_inventory_root_identity_and_file_input() {
    let temp = tempdir();
    let source_root = temp.path().join("projects");
    let source_path = source_root.join("child-before-parent.jsonl");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(&source_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, source_root.clone());
    let pending_file = SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: source.source_format.to_owned(),
        source_root: source_root.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 1,
        file_modified_at_ms: 1,
        observed_at_ms: 1,
        metadata: json!({}),
    };

    let context = manifest_pending_source_context(&source, &pending_file).unwrap();

    assert_eq!(context.source.path, source_root);
    assert_eq!(
        context.input_path,
        fs::canonicalize(source_path.clone()).unwrap()
    );
    assert_eq!(
        import_record_for_source(context.source).id,
        import_record_for_source(&source).id
    );
    assert_ne!(
        import_record_for_source(context.source).id,
        import_record_for_source(&explicit_path_source(CaptureProvider::Claude, source_path)).id
    );
}

#[test]
fn manifested_inventory_preserves_history_across_confirmed_missing_files() {
    let temp = tempdir();
    let source_root = temp.path().join("projects");
    fs::create_dir_all(&source_root).unwrap();
    let source_path = source_root.join("session.jsonl");
    fs::write(&source_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, source_root.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = inventory_source_import_files(&store, &source, false).unwrap();
    assert_eq!(first.files, 1);
    let source_root_text = source_root.display().to_string();
    fs::remove_file(source_path).unwrap();

    let missing_once = inventory_source_import_files(&store, &source, false).unwrap();
    assert_eq!(missing_once.files, 0);
    assert_eq!(store.source_import_file_counts().unwrap().stale, 0);
    assert!(store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap()
        .is_empty());
    let missing_noop = import_manifested_source(
        &mut store,
        &source,
        None,
        true,
        ctx_history_capture::CaptureWorkLimit::Drain,
        &ctx_history_capture::ImportProfile::CoreOnly,
    )
    .unwrap();
    assert_eq!(missing_noop.imported_sessions, 0);
    assert_eq!(missing_noop.imported_events, 0);

    let confirmed = inventory_source_import_files(&store, &source, false).unwrap();
    assert_eq!(confirmed.files, 0);
    assert_eq!(store.source_import_file_counts().unwrap().stale, 1);
}

#[test]
fn manifested_full_rescan_requeues_current_files_without_staling_them() {
    let temp = tempdir();
    let source_root = temp.path().join("projects");
    fs::create_dir_all(&source_root).unwrap();
    let source_path = source_root.join("session.jsonl");
    fs::write(&source_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, source_root.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    inventory_source_import_files(&store, &source, false).unwrap();
    let source_root_text = source_root.display().to_string();
    let file = store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap()
        .pop()
        .unwrap();
    store.mark_source_import_file_indexed(&file, 1).unwrap();
    assert!(store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap()
        .is_empty());

    let inventory = inventory_source_import_files(&store, &source, true).unwrap();

    assert_eq!(inventory.files, 1);
    assert_eq!(store.source_import_file_counts().unwrap().stale, 0);
    let pending = store
        .list_pending_source_import_files(source.provider, &source_root_text)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_path, source_path.display().to_string());
}

#[cfg(unix)]
#[test]
fn manifested_inventory_rejects_non_utf8_paths_before_persistence() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempdir();
    let source_root = temp
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'h', 0xff]));
    fs::create_dir_all(&source_root).unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, source_root);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let error = inventory_source_import_files(&store, &source, false).unwrap_err();

    assert!(error
        .to_string()
        .contains("provider transcript paths must be valid UTF-8"));
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
}

#[cfg(unix)]
#[test]
fn source_root_inventory_rejects_non_utf8_identity_before_persistence() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempdir();
    let source_root = temp
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'h', 0xff]));
    fs::create_dir_all(&source_root).unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_root);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let inventory = inventory_import_sources(&store, vec![source], false, false).unwrap();

    assert_eq!(inventory.failures.len(), 1);
    assert!(inventory.failures[0]
        .error
        .contains("provider transcript paths must be valid UTF-8"));
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
}
