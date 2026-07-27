use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EntityTimestamps, SyncCursor};
use ctx_history_store::{
    decode_native_path_committed_cursor, ProviderSourceLocatorObservation, Store,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    complete_content::{
        VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    native_source::NativePosition,
    provider::importer::{
        provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
        CertifiedProviderCursor,
    },
    CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::{
    import_forgecode_nativepath,
    nativepath::{
        legacy_source_revision,
        source::{
            discover_forgecode_source, ForgeCodeDiscovery, ForgeCodeFrontier, ForgeCodeScanner,
        },
    },
};

const SUCCESS_SENTINEL: &str = "forgecode-success-body-must-stay-out-of-core";

#[test]
fn scanner_pages_messages_and_separates_success_output() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let mut messages = (0..18)
        .map(|index| {
            json!({
                "message": {
                    "text": {
                        "role": "user",
                        "content": format!("message-{index}")
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    messages.push(success_output(SUCCESS_SENTINEL));
    write_source(&source_path, "conversation-pages", Value::Array(messages));

    let source = live_source(&source_path);
    let mut scanner = ForgeCodeScanner::new(
        source,
        ForgeCodeFrontier::initial(),
        context(&source_path),
        true,
    )
    .unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].events.len(), 16);
    assert_eq!(pages[1].events.len(), 2);
    assert_eq!(pages[1].outputs.len(), 1);
    assert_eq!(pages[1].outputs[0].content, SUCCESS_SENTINEL.as_bytes());
    assert!(pages
        .iter()
        .flat_map(|page| &page.events)
        .all(|event| !event.event.payload.to_string().contains(SUCCESS_SENTINEL)));
    assert!(pages
        .iter()
        .filter_map(|page| page.row.as_ref())
        .all(|row| {
            row.context_metadata
                .as_object()
                .is_none_or(|metadata| !metadata.contains_key("messages"))
        }));
    assert!(pages.last().unwrap().terminal);
}

#[test]
fn malformed_row_is_bounded_and_does_not_hide_healthy_sibling() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(&conn, "broken", Some("{not-json"), Some("[not-json"));
    insert_row(
        &conn,
        "healthy",
        Some(
            &json!({
                "messages": [{
                    "message": {"text": {"role": "assistant", "content": "healthy"}}
                }]
            })
            .to_string(),
        ),
        None,
    );
    drop(conn);

    let mut scanner = ForgeCodeScanner::new(
        live_source(&source_path),
        ForgeCodeFrontier::initial(),
        context(&source_path),
        false,
    )
    .unwrap();
    let first = scanner.next_page().unwrap().unwrap();
    let second = scanner.next_page().unwrap().unwrap();

    assert_eq!(first.rejections.len(), 2);
    assert!(first
        .rejections
        .iter()
        .all(|failure| failure.error.len() <= 4 * 1024));
    assert!(first.events.is_empty());
    assert_eq!(second.events.len(), 1);
    assert!(second.terminal);
}

#[test]
fn invalid_utf8_rows_are_rejected_between_healthy_siblings() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        "healthy-before",
        Some(&json!({"messages": [text_message("before")]}).to_string()),
        None,
    );
    insert_invalid_utf8_rows(&conn);
    insert_row(
        &conn,
        "healthy-after",
        Some(&json!({"messages": [text_message("after")]}).to_string()),
        None,
    );
    drop(conn);
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();

    let summary = import_core(&source_path, &mut store).unwrap();

    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.failures.len(), 6);
    assert!(summary
        .failures
        .iter()
        .all(|failure| failure.error.contains("is not valid UTF-8")));
    assert_eq!(event_count(&store), 2);
}

#[test]
fn three_mib_success_output_is_counted_once_and_replayed_only_to_pro() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let output = "o".repeat(3 * 1024 * 1024);
    write_source(
        &source_path,
        "conversation-three-mib",
        json!([success_output(&output), text_message("healthy-core")]),
    );
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();

    let summary = import_core(&source_path, &mut store).unwrap();
    assert_eq!(summary.imported_events, 1);
    assert!(summary.failures.is_empty());

    let sink = Arc::new(RecordingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(sink.clone()),
        ..Default::default()
    };

    let replay =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert!(replay.failures.is_empty());
    let contents = sink.contents.lock().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].len(), output.len());
    assert_eq!(contents[0], output.as_bytes());
    assert!(store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .all(|event| !event.payload.to_string().contains(&output[..128])));
}

#[test]
fn four_mib_output_boundary_is_accepted_and_larger_output_is_rejected() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let accepted = "a".repeat(4 * 1024 * 1024);
    let rejected = "r".repeat(4 * 1024 * 1024 + 1);
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        "accepted-boundary",
        Some(
            &json!({
                "messages": [success_output(&accepted), text_message("accepted-sibling")]
            })
            .to_string(),
        ),
        None,
    );
    insert_row(
        &conn,
        "rejected-boundary",
        Some(
            &json!({
                "messages": [success_output(&rejected), text_message("rejected-sibling")]
            })
            .to_string(),
        ),
        None,
    );
    insert_row(
        &conn,
        "healthy-after-boundaries",
        Some(&json!({"messages": [text_message("healthy-after")]}).to_string()),
        None,
    );
    drop(conn);
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(sink.clone()),
        ..Default::default()
    };

    let summary =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.failures.len(), 1);
    assert!(summary.failures[0].error.contains("transient-output limit"));
    let contents = sink.contents.lock().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].len(), accepted.len());
    assert_eq!(contents[0], accepted.as_bytes());
    assert_eq!(event_count(&store), 3);
}

#[test]
fn oversized_singleton_message_is_rejected_and_later_row_survives() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        &"oversized-singleton-identity".repeat(20_000),
        Some(
            &json!({
                "messages": [success_output(&"x".repeat(4 * 1024 * 1024))]
            })
            .to_string(),
        ),
        None,
    );
    insert_row(
        &conn,
        "healthy-after-oversized",
        Some(&json!({"messages": [text_message("healthy-after")]}).to_string()),
        None,
    );
    drop(conn);
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(sink.clone()),
        ..Default::default()
    };

    let summary =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failures.len(), 1);
    assert!(summary.failures[0].error.contains("retained-page limit"));
    assert!(sink.contents.lock().unwrap().is_empty());
    assert_eq!(event_count(&store), 1);
}

#[test]
fn core_replay_append_rewrite_and_disappearance_are_idempotent() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_root = directory.path().join("forge-root");
    fs::create_dir(&source_root).unwrap();
    let source_path = source_root.join(".forge.db");
    write_source(
        &source_path,
        "conversation-mutations",
        json!([text_message("one")]),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let first = import_core(&source_root, &mut store).unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(event_count(&store), 1);

    let replay = import_core(&source_root, &mut store).unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(event_count(&store), 1);

    replace_messages(
        &source_path,
        json!([text_message("one"), text_message("two")]),
    );
    import_core(&source_root, &mut store).unwrap();
    assert_eq!(event_count(&store), 2);

    replace_messages(
        &source_path,
        json!([text_message("one-rewritten"), text_message("two")]),
    );
    import_core(&source_root, &mut store).unwrap();
    assert_eq!(event_count(&store), 4);

    fs::remove_file(&source_path).unwrap();
    fs::remove_dir(&source_root).unwrap();
    let retirement = import_core(&source_root, &mut store).unwrap();
    assert_eq!(
        retirement.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    let replay = import_core(&source_root, &mut store).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
}

#[test]
fn restart_resumes_the_committed_message_frontier_without_duplicates() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    write_source(
        &source_path,
        "conversation-restart",
        Value::Array(
            (0..20)
                .map(|index| text_message(&format!("restart-{index}")))
                .collect(),
        ),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let first = import_forgecode_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(first.work_remaining);
    assert_eq!(event_count(&store), 16);
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let resumed = import_core(&source_path, &mut store).unwrap();
    assert_eq!(resumed.imported_events, 4);
    assert_eq!(event_count(&store), 20);
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let replay = import_core(&source_path, &mut store).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(event_count(&store), 20);
}

#[test]
fn deleted_custom_sqlite_filenames_retire_exact_routes_after_restart() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_paths = [
        directory.path().join("forge-history.sqlite"),
        directory.path().join("forge-history"),
        directory.path().join("FORGE-HISTORY.DB"),
    ];
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    for (index, source_path) in source_paths.iter().enumerate() {
        write_source(
            source_path,
            &format!("custom-filename-{index}"),
            json!([text_message(&format!("custom-{index}"))]),
        );
        let imported = import_core(source_path, &mut store).unwrap();
        assert_eq!(imported.imported_events, 1);
    }
    assert_eq!(event_count(&store), source_paths.len());
    for source_path in &source_paths {
        fs::remove_file(source_path).unwrap();
    }
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    for source_path in &source_paths {
        let retired = import_core(source_path, &mut store).unwrap();
        assert_eq!(
            retired.work_result(),
            crate::ProviderImportWorkResult::Changed
        );
        let replay = import_core(source_path, &mut store).unwrap();
        assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    }
    assert_eq!(event_count(&store), source_paths.len());
}

#[test]
fn released_policy_five_route_can_retire_before_nativepath_replay() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    write_source(
        &source_path,
        "released-delete",
        json!([text_message("released")]),
    );
    let source = live_source(&source_path);
    let source_revision = legacy_source_revision(&source);
    let canonical_path = fs::canonicalize(&source_path).unwrap();
    let path_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let canonical_source_identity = "released-forgecode-canonical".to_owned();
    let imported_at = context(&source_path).imported_at;
    let store_path = directory.path().join("ctx.sqlite");
    let store = Store::open(&store_path).unwrap();
    store
        .reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::ForgeCode,
            source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: "forgecode-nativepath-test".to_owned(),
            locator_identity: format!("forgecode-sqlite:{path_identity}"),
            cursor_stream: stream.clone(),
            proposed_source_identity: canonical_source_identity,
            raw_source_path: Some(canonical_path.display().to_string()),
            source_revision: source_revision.clone(),
            observed_at_ms: imported_at.timestamp_millis(),
        })
        .unwrap();
    let released_cursor = CertifiedProviderCursor::new(
        source_revision,
        1,
        5,
        NativePosition::new("forgecode-conversation-rowid-v1", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "forgecode-nativepath-test".to_owned(),
            stream: stream.clone(),
            cursor: released_cursor,
            last_synced_at: Some(imported_at),
            timestamps: EntityTimestamps {
                created_at: imported_at,
                updated_at: imported_at,
            },
        })
        .unwrap();
    drop(store);
    fs::remove_file(&source_path).unwrap();

    let mut store = Store::open(&store_path).unwrap();
    let retired = import_core(&source_path, &mut store).unwrap();
    assert_eq!(
        retired.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    let committed = store
        .get_sync_cursor(None, "forgecode-nativepath-test", &stream)
        .unwrap()
        .unwrap();
    assert!(decode_native_path_committed_cursor(&committed.cursor).is_ok());
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let replay = import_core(&source_path, &mut store).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
}

#[test]
fn output_failure_does_not_stop_later_core_pages() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let mut messages = vec![success_output(SUCCESS_SENTINEL)];
    messages.extend((0..20).map(|index| text_message(&format!("core-{index}"))));
    write_source(
        &source_path,
        "conversation-pro-failure",
        Value::Array(messages),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let failing = Arc::new(FailingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(failing.clone()),
        ..Default::default()
    };

    let summary =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert_eq!(summary.imported_events, 20);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.failures[0].error,
        "ForgeCode Pro output is behind committed Core"
    );
    assert!(failing.behind.load(Ordering::SeqCst) > 0);
    assert_eq!(event_count(&store), 20);
    assert!(store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .all(|event| !event.payload.to_string().contains(SUCCESS_SENTINEL)));

    let replay = Arc::new(RecordingSink::default());
    let replay_options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(replay.clone()),
        ..Default::default()
    };
    let replay_summary = import_forgecode_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        replay_options,
    )
    .unwrap();
    assert_eq!(replay_summary.failed, 0);
    assert_eq!(
        replay.contents.lock().unwrap().as_slice(),
        [SUCCESS_SENTINEL.as_bytes()]
    );
    assert_eq!(event_count(&store), 20);
}

#[test]
fn pro_can_activate_after_core_and_replay_success_body() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    write_source(
        &source_path,
        "conversation-later-pro",
        json!([text_message("core"), success_output(SUCCESS_SENTINEL)]),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import_core(&source_path, &mut store).unwrap();
    assert_eq!(event_count(&store), 1);

    let sink = Arc::new(RecordingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(sink.clone()),
        ..Default::default()
    };
    let replay =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SUCCESS_SENTINEL.as_bytes()]
    );
    assert_eq!(event_count(&store), 1);
}

#[test]
fn truncated_message_keeps_the_existing_verified_source_locator() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let body = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 1);
    write_source(
        &source_path,
        "conversation-complete-content",
        json!([text_message(&body)]),
    );
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();
    import_core(&source_path, &mut store).unwrap();

    let session = store.list_sessions().unwrap().pop().unwrap();
    let event = store.events_for_session(session.id).unwrap().pop().unwrap();
    assert!(event
        .sync
        .metadata
        .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_some());
}

#[test]
fn core_never_persists_result_body_content() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let long_message = "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 1);
    write_source(
        &source_path,
        "conversation-no-core-result-locator",
        json!([
            text_message(&long_message),
            success_output(SUCCESS_SENTINEL),
            failed_output("forgecode-failed-output-body-must-stay-out-of-core")
        ]),
    );
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(sink.clone()),
        ..Default::default()
    };
    import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options).unwrap();

    let events = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| {
        event
            .sync
            .metadata
            .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
            .and_then(VerifiedContentLocatorsV1::from_metadata_value)
            .and_then(|locators| {
                locators
                    .locator(VerifiedContentRole::MessageBody)
                    .map(|_| ())
            })
            .is_some()
    }));
    for event in &events {
        let encoded = serde_json::to_string(event).unwrap();
        assert!(!encoded.contains(SUCCESS_SENTINEL));
        assert!(!encoded.contains("forgecode-failed-output-body-must-stay-out-of-core"));
    }
    let contents = sink.contents.lock().unwrap();
    assert_eq!(contents.len(), 2);
    assert!(contents
        .iter()
        .any(|content| content == SUCCESS_SENTINEL.as_bytes()));
    assert!(contents
        .iter()
        .any(|content| { content == b"forgecode-failed-output-body-must-stay-out-of-core" }));
}

#[test]
fn missing_root_resolves_to_the_canonical_database_locator() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let missing_root = directory.path().join("missing-forge-root");
    match discover_forgecode_source(&missing_root).unwrap() {
        ForgeCodeDiscovery::Missing(missing) => {
            assert_eq!(missing.preferred_path, missing_root.join(".forge.db"));
        }
        ForgeCodeDiscovery::Live(_) => panic!("missing root was discovered as live"),
    }
}

#[derive(Default)]
struct FailingSink {
    behind: AtomicUsize,
}

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "forgecode-failing-test-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        _page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        Err(ProOutputSinkError::new("test_failure", "expected"))
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct RecordingSink {
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "forgecode-recording-test-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().unwrap().insert(
            page.source.clone(),
            ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            },
        );
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

fn import_core(path: &Path, store: &mut Store) -> crate::Result<crate::ProviderImportSummary> {
    import_forgecode_nativepath(path, store, context(path), ProviderImportOptions::default())
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "forgecode-nativepath-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

fn live_source(path: &Path) -> super::nativepath::source::ForgeCodeSourceObservation {
    match discover_forgecode_source(path).unwrap() {
        ForgeCodeDiscovery::Live(source) => source,
        ForgeCodeDiscovery::Missing(_) => panic!("fixture source is missing"),
    }
}

fn event_count(store: &Store) -> usize {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|session| store.events_for_session(session.id).unwrap().len())
        .sum()
}

fn write_source(path: &Path, conversation_id: &str, messages: Value) {
    let conn = Connection::open(path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        conversation_id,
        Some(&json!({"initiator": "forge", "messages": messages}).to_string()),
        Some(&json!({"files_accessed": ["Cargo.toml"]}).to_string()),
    );
}

fn create_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE conversations (
            conversation_id TEXT NOT NULL,
            title TEXT,
            workspace_id INTEGER NOT NULL,
            context TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            metrics TEXT
        );",
    )
    .unwrap();
}

fn insert_row(
    conn: &Connection,
    conversation_id: &str,
    context: Option<&str>,
    metrics: Option<&str>,
) {
    conn.execute(
        "INSERT INTO conversations
         (conversation_id, title, workspace_id, context, created_at, updated_at, metrics)
         VALUES (?1, 'test', 7, ?2, '2026-01-01T00:00:00Z',
                 '2026-01-01T00:00:01Z', ?3)",
        rusqlite::params![conversation_id, context, metrics],
    )
    .unwrap();
}

fn insert_invalid_utf8_rows(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO conversations VALUES
            (CAST(X'80' AS TEXT), 'test', 7, '{\"messages\":[]}',
             '2026-01-01T00:00:00Z', NULL, NULL);
         INSERT INTO conversations VALUES
            ('bad-title', CAST(X'80' AS TEXT), 7, '{\"messages\":[]}',
             '2026-01-01T00:00:00Z', NULL, NULL);
         INSERT INTO conversations VALUES
            ('bad-context', 'test', 7, CAST(X'80' AS TEXT),
             '2026-01-01T00:00:00Z', NULL, NULL);
         INSERT INTO conversations VALUES
            ('bad-created', 'test', 7, '{\"messages\":[]}',
             CAST(X'80' AS TEXT), NULL, NULL);
         INSERT INTO conversations VALUES
            ('bad-updated', 'test', 7, '{\"messages\":[]}',
             '2026-01-01T00:00:00Z', CAST(X'80' AS TEXT), NULL);
         INSERT INTO conversations VALUES
            ('bad-metrics', 'test', 7, '{\"messages\":[]}',
             '2026-01-01T00:00:00Z', NULL, CAST(X'80' AS TEXT));",
    )
    .unwrap();
}

fn replace_messages(path: &Path, messages: Value) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "UPDATE conversations SET context = ?1, updated_at = ?2",
        rusqlite::params![
            json!({"initiator": "forge", "messages": messages}).to_string(),
            "2026-01-01T00:00:02Z",
        ],
    )
    .unwrap();
}

fn text_message(text: &str) -> Value {
    json!({"message": {"text": {"role": "user", "content": text}}})
}

fn success_output(text: &str) -> Value {
    json!({
        "message": {
            "tool": {
                "name": "shell",
                "call_id": "call-success",
                "output": {
                    "is_error": false,
                    "values": [{"text": text}]
                }
            }
        }
    })
}

fn failed_output(text: &str) -> Value {
    json!({
        "message": {
            "tool": {
                "name": "shell",
                "call_id": "call-failure",
                "output": {
                    "is_error": true,
                    "values": [{"text": text}]
                }
            }
        }
    })
}
