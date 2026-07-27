use std::{
    fs, io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_store::{RawSqlOptions, RawSqlValue};
use serde_json::json;

use crate::{
    provider::importer::{released_jsonl_initial_position_for_test, BoundedParserCheckpoint},
    test_support_paths::tempdir,
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderImportWorkResult,
};

use super::*;

fn write_session(path: &Path, session_id: &str, messages: &[&str]) {
    let history = messages
        .iter()
        .enumerate()
        .map(|(ordinal, message)| {
            if ordinal == 0 {
                json!({
                    "id": format!("item-{ordinal}"),
                    "timestamp": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": message},
                    "toolCallStates": [{
                        "toolCallId": "call-0",
                        "toolCall": {
                            "id": "call-0",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": "{\"command\":\"printf test\"}",
                            }
                        },
                        "status": "done",
                        "output": [{
                            "name": "Result",
                            "content": "SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE",
                        }],
                    }],
                })
            } else {
                json!({
                    "id": format!("item-{ordinal}"),
                    "timestamp": "2026-01-01T00:00:01Z",
                    "message": {"role": "user", "content": message},
                })
            }
        })
        .collect::<Vec<_>>();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": session_id,
            "title": format!("Session {session_id}"),
            "createdAt": "2026-01-01T00:00:00Z",
            "history": history,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_output_session(path: &Path) {
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": "stable",
            "title": "Output replay",
            "createdAt": "2026-01-01T00:00:00Z",
            "history": [
                {
                    "id": "request",
                    "timestamp": "2026-01-01T00:00:00Z",
                    "message": {"role": "user", "content": "run it"},
                },
                {
                    "id": "tool",
                    "timestamp": "2026-01-01T00:00:01Z",
                    "message": {"role": "assistant", "content": ""},
                    "toolCallStates": [{
                        "toolCallId": "call-0",
                        "toolCall": {
                            "id": "call-0",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": "{\"command\":\"printf test\"}",
                            }
                        },
                        "status": "done",
                        "output": [{
                            "name": "Result",
                            "content": "SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE",
                        }],
                    }],
                },
            ],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_touch_session(path: &Path, touch_paths: &[String]) {
    let mut patch = String::from("*** Begin Patch\n");
    for touch_path in touch_paths {
        patch.push_str(&format!("*** Update File: {touch_path}\n"));
    }
    patch.push_str("*** End Patch\n");
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": "touch-stable",
            "title": "Touch rewrite",
            "createdAt": "2026-01-01T00:00:00Z",
            "history": [{
                "id": "touch-event",
                "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "assistant", "content": ""},
                "toolCallStates": [{
                    "toolCallId": "touch-call",
                    "toolCall": {
                        "id": "touch-call",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": patch,
                        }
                    },
                    "status": "done",
                }],
            }],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn import(root: &Path, store: &mut Store) -> Result<ProviderImportSummary> {
    import_with_profile(root, store, ImportProfile::CoreOnly)
}

fn import_with_profile(
    root: &Path,
    store: &mut Store,
    import_profile: ImportProfile,
) -> Result<ProviderImportSummary> {
    import_with_options(
        root,
        store,
        ProviderImportOptions {
            import_profile,
            ..Default::default()
        },
    )
}

fn import_with_work_limit(
    root: &Path,
    store: &mut Store,
    capture_work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    import_with_options(
        root,
        store,
        ProviderImportOptions {
            capture_work_limit,
            ..Default::default()
        },
    )
}

fn import_with_options(
    root: &Path,
    store: &mut Store,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    import_continue_nativepath_history(
        root,
        store,
        ProviderAdapterContext {
            machine_id: "continue-nativepath-test".to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: DateTime::<Utc>::from_timestamp(1_767_225_600, 0).unwrap(),
        },
        options,
    )
}

fn events(store: &Store) -> Vec<Event> {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect()
}

fn visible_touch_paths(store: &Store) -> Vec<String> {
    store
        .raw_sql_query(
            "SELECT path FROM ctx_files_touched ORDER BY path",
            RawSqlOptions::default(),
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| match row.into_iter().next().unwrap() {
            RawSqlValue::Text { value, .. } => value,
            value => panic!("expected text file-touch path, got {value:?}"),
        })
        .collect()
}

#[derive(Default)]
struct ReplaySink {
    fail: AtomicBool,
    behind: AtomicUsize,
    materialized_pages: AtomicUsize,
    materialized_outputs: AtomicUsize,
    behind_errors: Mutex<Vec<String>>,
    output_bodies: Mutex<Vec<Vec<u8>>>,
    progress: Mutex<Option<ProOutputProgress>>,
}

impl ProOutputSink for ReplaySink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "continue-production-test-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.materialized_pages.fetch_add(1, Ordering::SeqCst);
        self.materialized_outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "continue_test_sink_failure",
                "intentional output failure",
            ));
        }
        self.output_bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(page.next_safe_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: u32::try_from(page.observations.len()).unwrap(),
            replayed: false,
        })
    }

    fn mark_behind(&self, error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
        self.behind_errors.lock().unwrap().push(error.to_string());
    }
}

#[test]
fn pro_failure_cannot_block_core_and_later_replay_recovers_output() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    write_output_session(&root.join("session.json"));
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let sink = Arc::new(ReplaySink::default());
    sink.fail.store(true, Ordering::SeqCst);

    let core =
        import_with_profile(&root, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(events(&store).len(), 2);
    assert!(events(&store).iter().all(|event| {
        !event
            .payload
            .to_string()
            .contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE")
    }));
    assert_ne!(sink.behind.load(Ordering::SeqCst), 0);

    sink.fail.store(false, Ordering::SeqCst);
    import_with_profile(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    let output_bodies = sink
        .output_bodies
        .lock()
        .unwrap()
        .iter()
        .map(|body| String::from_utf8_lossy(body).into_owned())
        .collect::<Vec<_>>();
    assert!(
        output_bodies
            .iter()
            .any(|body| body.contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE")),
        "pages={}, outputs={}, behind={:?}, bodies={output_bodies:?}",
        sink.materialized_pages.load(Ordering::SeqCst),
        sink.materialized_outputs.load(Ordering::SeqCst),
        sink.behind_errors.lock().unwrap(),
    );
    assert_eq!(events(&store).len(), 2);
    let materialized_pages = sink.materialized_pages.load(Ordering::SeqCst);
    import_with_profile(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(
        sink.materialized_pages.load(Ordering::SeqCst),
        materialized_pages
    );
}

#[test]
fn production_core_lifecycle_is_idempotent_private_and_restorable() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    write_session(&source, "stable", &["first"]);
    let fresh = import(&root, &mut store).unwrap();
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let first_events = events(&store);
    assert_eq!(first_events.len(), 1);
    assert!(!first_events[0]
        .payload
        .to_string()
        .contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE"));

    let no_op = import(&root, &mut store).unwrap();
    assert_eq!(no_op.work_result(), ProviderImportWorkResult::NoOp);

    write_session(&source, "stable", &["first", "appended"]);
    let append = import(&root, &mut store).unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(events(&store).len(), 2);

    write_session(&source, "stable", &["rewritten", "appended"]);
    let rewrite = import(&root, &mut store).unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert!(events(&store)
        .iter()
        .any(|event| event.payload.to_string().contains("rewritten")));

    write_session(&source, "stable", &["truncated"]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    write_session(&source, "stable", &["rewritten", "appended"]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::write(&source, br#"{"sessionId":"stable","history":["#).unwrap();
    let incomplete = import(&root, &mut store).unwrap();
    assert_eq!(incomplete.failed, 1);
    assert!(store
        .authorized_source_route_for_event(first_events[0].id)
        .is_ok());

    write_session(&source, "stable", &["rewritten", "appended"]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::NoOp
    );
    fs::remove_file(&source).unwrap();
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(first_events[0].id)
        .is_err());
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_session(&source, "stable", &["rewritten", "appended"]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(first_events[0].id)
        .is_ok());

    fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    fs::create_dir(&root).unwrap();
    write_session(&source, "stable", &["rewritten", "appended"]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(first_events[0].id)
        .is_ok());
}

#[test]
fn missing_source_retires_validated_pre_026_cursor_before_first_migration() {
    for remove_root in [false, true] {
        let temp = tempdir().unwrap();
        let root = temp.path().join("continue");
        fs::create_dir(&root).unwrap();
        let source = root.join("session.json");
        write_session(&source, "pre-026", &["legacy"]);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        import(&root, &mut store).unwrap();
        let event_id = events(&store)[0].id;
        let locator = provider_path_identity(&fs::canonicalize(&source).unwrap()).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Continue,
            CONTINUE_CLI_SOURCE_FORMAT,
            &locator,
        );
        let mut stored = store
            .get_sync_cursor(None, "continue-nativepath-test", &stream)
            .unwrap()
            .unwrap();
        let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
        let source_revision = ContinueNativeStoreCursor::decode(committed.provider_cursor())
            .unwrap()
            .source_revision;
        let released = CertifiedProviderCursor::new(
            source_revision,
            1,
            1,
            released_jsonl_initial_position_for_test(),
            BoundedParserCheckpoint::from_serializable(&()).unwrap(),
        )
        .unwrap()
        .encode()
        .unwrap();
        stored.cursor = released.clone();
        store.upsert_sync_cursor(&stored).unwrap();

        if remove_root {
            fs::remove_dir_all(&root).unwrap();
        } else {
            fs::remove_file(&source).unwrap();
        }
        let retired = import(&root, &mut store).unwrap();
        assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
        assert!(store.authorized_source_route_for_event(event_id).is_err());
        let published = store
            .get_sync_cursor(None, "continue-nativepath-test", &stream)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_native_path_committed_cursor(&published.cursor)
                .unwrap()
                .provider_cursor(),
            released
        );
    }
}

#[test]
fn missing_source_rejects_unvalidated_raw_cursor_without_retiring_route() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    write_session(&source, "unreleased", &["cursor"]);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import(&root, &mut store).unwrap();
    let event_id = events(&store)[0].id;
    let locator = provider_path_identity(&fs::canonicalize(&source).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &locator,
    );
    let mut stored = store
        .get_sync_cursor(None, "continue-nativepath-test", &stream)
        .unwrap()
        .unwrap();
    stored.cursor = "unreleased-continue-offset:7".to_owned();
    store.upsert_sync_cursor(&stored).unwrap();
    fs::remove_dir_all(&root).unwrap();

    assert!(matches!(
        import(&root, &mut store),
        Err(CaptureError::InvalidPayload(message))
            if message.contains("released migration cursor")
    ));
    assert!(store.authorized_source_route_for_event(event_id).is_ok());
}

#[test]
fn missing_source_retirement_survives_store_reopen() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    let store_path = temp.path().join("work.sqlite");
    write_session(&source, "reopen", &["persisted"]);
    let mut store = Store::open(&store_path).unwrap();
    import(&root, &mut store).unwrap();
    let event_id = events(&store)[0].id;
    drop(store);

    fs::remove_file(&source).unwrap();
    let mut reopened = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut reopened).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(reopened
        .authorized_source_route_for_event(event_id)
        .is_err());
}

#[test]
fn source_permission_failure_preserves_kind_and_is_not_a_record_rejection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    write_session(&source, "permission", &["unreadable"]);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    inject_continue_io_failure(
        ContinueInjectedIoOperation::SourceRead,
        source,
        io::Error::from(io::ErrorKind::PermissionDenied),
    );

    let error = import(&root, &mut store).unwrap_err();
    clear_continue_io_failure();
    assert!(matches!(
        error,
        CaptureError::Io(ref source) if source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(events(&store).is_empty());
}

#[cfg(unix)]
#[test]
fn path_spool_enospc_is_system_io() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    write_session(&source, "enospc", &["spool"]);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    inject_continue_io_failure(
        ContinueInjectedIoOperation::SpoolWrite,
        source,
        io::Error::from_raw_os_error(libc::ENOSPC),
    );

    let error = import(&root, &mut store).unwrap_err();
    clear_continue_io_failure();
    assert!(matches!(
        error,
        CaptureError::SystemIo {
            operation: "write Continue path spool",
            source,
        } if source.raw_os_error() == Some(libc::ENOSPC)
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn missing_root_retirement_honors_one_safe_group() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    for ordinal in 0..3 {
        write_session(
            &root.join(format!("session-{ordinal}.json")),
            &format!("bounded-{ordinal}"),
            &[&format!("event-{ordinal}")],
        );
    }
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import(&root, &mut store).unwrap();
    let event_ids = events(&store)
        .into_iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert_eq!(event_ids.len(), 3);
    fs::remove_dir_all(&root).unwrap();

    for expected_authorized in [2, 1, 0] {
        let retired =
            import_with_work_limit(&root, &mut store, CaptureWorkLimit::OneSafeGroup).unwrap();
        assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(retired.skipped_sessions, 1);
        assert_eq!(
            retired.work_remaining,
            expected_authorized != 0,
            "work_remaining must describe another missing route"
        );
        assert_eq!(
            event_ids
                .iter()
                .filter(|event_id| store.authorized_source_route_for_event(**event_id).is_ok())
                .count(),
            expected_authorized
        );
    }
    assert_eq!(
        import_with_work_limit(&root, &mut store, CaptureWorkLimit::OneSafeGroup)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn oversized_touch_event_is_rejected_before_store_publication() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let oversized = (0..=CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT)
        .map(|index| format!("src/oversized-{index}.rs"))
        .collect::<Vec<_>>();

    write_touch_session(&source, &oversized);
    let rejected = import(&root, &mut store).unwrap();
    assert_eq!(rejected.failed, 1);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(events(&store).is_empty());
    assert!(visible_touch_paths(&store).is_empty());

    write_touch_session(&source, &["src/bounded.rs".to_owned()]);
    let bounded = import(&root, &mut store).unwrap();
    assert_eq!(bounded.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(visible_touch_paths(&store), ["src/bounded.rs"]);
}

#[test]
fn touch_only_rewrite_retires_surplus_touch_without_stale_blame() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("continue");
    fs::create_dir(&root).unwrap();
    let source = root.join("session.json");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_paths = ["src/one.rs".to_owned(), "src/two.rs".to_owned()];

    write_touch_session(&source, &first_paths);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    let first_event = events(&store).pop().unwrap();
    let first_hash = first_event.payload["provider_event_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(visible_touch_paths(&store), ["src/one.rs", "src/two.rs"]);
    assert!(!first_event.payload.to_string().contains("src/one.rs"));

    write_touch_session(&source, &["src/one.rs".to_owned()]);
    assert_eq!(
        import(&root, &mut store).unwrap().work_result(),
        ProviderImportWorkResult::Changed
    );
    let rewritten_event = events(&store).pop().unwrap();
    assert_eq!(rewritten_event.id, first_event.id);
    assert_ne!(
        rewritten_event.payload["provider_event_hash"]
            .as_str()
            .unwrap(),
        first_hash
    );
    assert_eq!(visible_touch_paths(&store), ["src/one.rs"]);
    assert!(store.file_touch_scope("src/two.rs").unwrap().is_empty());
    assert!(store
        .file_touch_scope("src/one.rs")
        .unwrap()
        .event_ids
        .contains(&rewritten_event.id));
    assert!(!rewritten_event.payload.to_string().contains("src/one.rs"));
}
