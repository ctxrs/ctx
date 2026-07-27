use std::{
    fs::{self, FileTimes, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    import_openclaw_history, ImportProfile, OpenClawImportOptions, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult,
};

use super::{native_path, without_file_ids};
use crate::provider::importer::{
    provider_scoped_source_uuid, provider_source_event_uuid, provider_source_identity,
    provider_source_session_uuid,
};
use crate::OPENCLAW_SOURCE_FORMAT;

const MACHINE: &str = "openclaw-nativepath-test-machine";
const SUCCESS_BODY: &str = "OPENCLAW_SUCCESS_BODY_MUST_NOT_ENTER_CORE";
const FAILURE_BODY: &str = "OPENCLAW_FAILURE_BODY";

#[test]
fn nativepath_lifecycle_covers_restart_mutations_and_disappearance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("fresh", "user", "fresh OpenClaw prompt"),
        ],
        "fresh label",
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 1);
    let session = openclaw_session(&store);
    let routed_event = store.events_for_session(session.id).unwrap()[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    append_record(
        &transcript,
        &message("append", "assistant", "appended OpenClaw answer"),
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(append.imported_events, 1);

    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("rewrite", "user", &"rewritten OpenClaw content ".repeat(32)),
        ],
        "rewrite label",
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    write_fixture(
        &transcript,
        &[header("session-1"), message("short", "user", "short")],
        "short label",
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = transcript.with_extension("replacement");
    write_fixture(
        &replacement,
        &[
            header("session-1"),
            message("replacement", "assistant", "replacement generation"),
        ],
        "replacement label",
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn nativepath_is_core_first_and_replays_outputs_independently() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-output"),
            message("prompt", "user", "run the command"),
            tool_result("success", 0, SUCCESS_BODY),
            tool_result("failure", 13, FAILURE_BODY),
        ],
        "output label",
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    let core_events = store
        .events_for_session(openclaw_session(&store).id)
        .unwrap();
    assert_eq!(core_events.len(), 2);
    let output_event = core_events
        .iter()
        .find(|event| {
            matches!(
                event.event_type,
                EventType::ToolOutput | EventType::CommandOutput
            )
        })
        .unwrap();
    assert_eq!(output_event.payload["result_outcome"], json!("failure"));
    assert_eq!(output_event.payload["exit_code"], json!(13));
    assert!(output_event.payload.get("body").is_none());
    assert!(output_event.payload.get("output_preview").is_none());
    let encoded = serde_json::to_string(&core_events).unwrap();
    assert!(!encoded.contains(SUCCESS_BODY));
    assert!(!encoded.contains(FAILURE_BODY));
    assert!(store
        .search_event_hits(SUCCESS_BODY, 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits(FAILURE_BODY, 10)
        .unwrap()
        .is_empty());
    assert!(!store
        .search_event_hits("run the command", 10)
        .unwrap()
        .is_empty());
    let conn = Connection::open(&store_path).unwrap();
    let durable_events: String = conn
        .query_row(
            "SELECT COALESCE(group_concat(payload_json || metadata_json, ''), '')
             FROM events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let durable_search: String = conn
        .query_row(
            "SELECT COALESCE(group_concat(preview_text, ''), '')
             FROM event_search_lookup",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let forbidden_keys: i64 = conn
        .query_row(
            "SELECT
                 (SELECT COUNT(*)
                  FROM events, json_tree(events.payload_json)
                  WHERE json_tree.key IN ('body', 'output_preview'))
               + (SELECT COUNT(*)
                  FROM events, json_tree(events.metadata_json)
                  WHERE json_tree.key IN ('body', 'output_preview'))",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(forbidden_keys, 0);
    assert!(!durable_events.contains(SUCCESS_BODY));
    assert!(!durable_events.contains(FAILURE_BODY));
    assert!(!durable_search.contains(SUCCESS_BODY));
    assert!(!durable_search.contains(FAILURE_BODY));
    drop(conn);
    let pages_after_fresh = sink.pages.load(Ordering::SeqCst);

    let noop = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

    let replay_store_path = temp.path().join("replay.sqlite");
    let mut replay_store = Store::open(&replay_store_path).unwrap();
    let replay_sink = Arc::new(RecordingSink::new(replay_store_path));
    let replay = import(
        &root,
        &mut replay_store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay_store.list_sessions().unwrap().is_empty());
    assert_eq!(replay_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 0);

    let failing_store_path = temp.path().join("failing.sqlite");
    let mut failing_store = Store::open(&failing_store_path).unwrap();
    let failing_sink = Arc::new(FailingSink::default());
    let core_survives = import(
        &root,
        &mut failing_store,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(
        core_survives.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(failing_store.list_sessions().unwrap().len(), 1);
    assert!(failing_sink.behind.load(Ordering::SeqCst));
}

#[test]
fn pro_replay_waits_for_openclaw_append_rewrite_and_replacement_core() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-authority"),
            message("initial", "user", "initial"),
            tool_result("initial", 0, "initial-output"),
        ],
        "initial label",
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let sink = Arc::new(RecordingSink::new(store_path));

    append_record(&transcript, &tool_result("append", 0, "append-output"));
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);

    let pages_after_append = sink.pages.load(Ordering::SeqCst);
    write_fixture(
        &transcript,
        &[
            header("session-authority"),
            message("rewrite", "user", "rewrite"),
            tool_result("rewrite", 0, "rewrite-output"),
        ],
        "rewrite label",
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_append);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert!(sink.pages.load(Ordering::SeqCst) > pages_after_append);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 3);

    let pages_after_rewrite = sink.pages.load(Ordering::SeqCst);
    let replacement = transcript.with_file_name("replacement.jsonl");
    write_fixture(
        &replacement,
        &[
            header("session-authority"),
            message("replacement", "user", "replacement"),
            tool_result("replacement", 0, "replacement-output"),
        ],
        "replacement label",
    );
    fs::remove_file(&transcript).unwrap();
    fs::rename(&replacement, &transcript).unwrap();
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_rewrite);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert!(sink.pages.load(Ordering::SeqCst) > pages_after_rewrite);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 4);
}

#[test]
fn nativepath_retries_incomplete_tail_and_reports_corrupt_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(&transcript, &[header("session-tail")], "tail label");
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    write!(
        file,
        "{{\"type\":\"message\",\"id\":\"tail\",\"timestamp\":\"2026-07-25T12:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"tail"
    )
    .unwrap();
    drop(file);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let incomplete = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(incomplete.failed, 0);
    assert!(store
        .events_for_session(openclaw_session(&store).id)
        .unwrap()
        .is_empty());

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(file, " completed\"}}}}").unwrap();
    writeln!(file, "{{malformed-openclaw-record").unwrap();
    drop(file);
    let completed = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(completed.imported_events, 1);
    assert_eq!(completed.failed, 1);
    assert_eq!(
        store
            .events_for_session(openclaw_session(&store).id)
            .unwrap()
            .len(),
        1
    );
    let replay = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 1);
}

#[test]
fn generation_zero_adopts_released_source_session_event_and_cursor_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("legacy-message", "user", "released generation zero"),
        ],
        "migration label",
    );
    let store_path = temp.path().join("migration.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.imported_events, 1);

    let canonical_transcript = fs::canonicalize(&transcript).unwrap();
    let raw_source_path = canonical_transcript.display().to_string();
    let source_root = root.display().to_string();
    let provider_session_id = "personal-agent/session-1";
    let expected_source_identity = provider_source_identity(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .unwrap();
    let expected_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenClaw,
        provider_session_id,
        OPENCLAW_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let expected_session_id =
        provider_source_session_uuid(&expected_source_identity, provider_session_id);
    let expected_event_id = provider_source_event_uuid(expected_source_id, 1);
    let source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.descriptor.provider == CaptureProvider::OpenClaw)
        .unwrap();
    assert_eq!(source.id, expected_source_id);
    assert_eq!(
        source.descriptor.source_identity.as_deref(),
        Some(expected_source_identity.as_str())
    );
    assert_eq!(openclaw_session(&store).id, expected_session_id);
    assert_eq!(
        store.events_for_session(expected_session_id).unwrap()[0].id,
        expected_event_id
    );
    assert_eq!(
        native_path::committed_generation_for_test(&store, MACHINE, &transcript).unwrap(),
        0
    );

    native_path::install_released_cursor_for_test(&mut store, MACHINE, &transcript).unwrap();
    drop(store);
    let conn = Connection::open(&store_path).unwrap();
    conn.execute("DELETE FROM capture_source_provider_routes", [])
        .unwrap();
    conn.execute("DELETE FROM provider_source_locators", [])
        .unwrap();
    drop(conn);
    let mut store = Store::open(&store_path).unwrap();
    assert!(store
        .authorized_source_route_for_event(expected_event_id)
        .is_err());

    let unchanged = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        unchanged.work_result(),
        ProviderImportWorkResult::Changed,
        "an unchanged released cursor still needs an atomic NativePath route/cursor install"
    );
    assert_eq!(unchanged.imported_events, 0);
    assert_eq!(openclaw_session(&store).id, expected_session_id);
    assert_eq!(
        store.events_for_session(expected_session_id).unwrap()[0].id,
        expected_event_id
    );
    assert!(store
        .authorized_source_route_for_event(expected_event_id)
        .is_ok());
    assert_eq!(
        native_path::committed_generation_for_test(&store, MACHINE, &transcript).unwrap(),
        0
    );

    native_path::install_released_cursor_for_test(&mut store, MACHINE, &transcript).unwrap();
    append_record(
        &transcript,
        &message(
            "legacy-append",
            "assistant",
            "released cursor append survives revision change",
        ),
    );
    let appended = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(
        native_path::committed_generation_for_test(&store, MACHINE, &transcript).unwrap(),
        0
    );
    let events = store.events_for_session(expected_session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .find(|event| event.payload["text"] == "released generation zero")
            .unwrap()
            .id,
        expected_event_id
    );
    assert_eq!(
        events
            .iter()
            .find(|event| {
                event.payload["text"] == "released cursor append survives revision change"
            })
            .unwrap()
            .id,
        provider_source_event_uuid(expected_source_id, 2)
    );
}

#[test]
fn absent_file_ids_use_verified_prefix_across_changed_mtime_append() {
    without_file_ids(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let transcript = transcript_path(&root);
        write_fixture(
            &transcript,
            &[
                header("session-1"),
                message("initial", "user", "no file id initial"),
            ],
            "no file id label",
        );
        let mut store = Store::open(temp.path().join("no-file-id.sqlite")).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).imported_events,
            1
        );
        let session_id = openclaw_session(&store).id;
        let previous_mtime = fs::metadata(&transcript).unwrap().modified().unwrap();
        append_record(
            &transcript,
            &message("append", "assistant", "no file id verified append"),
        );
        let file = OpenOptions::new().write(true).open(&transcript).unwrap();
        file.set_times(FileTimes::new().set_modified(previous_mtime + Duration::from_secs(7)))
            .unwrap();

        let appended = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(appended.imported_events, 1);
        assert_eq!(openclaw_session(&store).id, session_id);
        assert_eq!(
            native_path::committed_generation_for_test(&store, MACHINE, &transcript).unwrap(),
            0,
            "verified exact-path continuity must remain an append"
        );
        assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
    });
}

#[test]
fn non_object_json_is_rejected_per_record_for_mixed_and_all_invalid_sources() {
    let temp = tempfile::tempdir().unwrap();
    let mixed_root = temp.path().join("mixed");
    let mixed_transcript = transcript_path(&mixed_root);
    write_raw_fixture(
        &mixed_transcript,
        &[
            serde_json::to_string(&header("session-1")).unwrap(),
            "\"invalid scalar\"".to_owned(),
            serde_json::to_string(&message("valid", "user", "valid sibling survives")).unwrap(),
            "[\"invalid\", \"array\"]".to_owned(),
            "{malformed".to_owned(),
        ],
        "mixed label",
    );
    let mut mixed_store = Store::open(temp.path().join("mixed.sqlite")).unwrap();
    let mixed = import(&mixed_root, &mut mixed_store, ImportProfile::CoreOnly);
    assert_eq!(mixed.imported_events, 1);
    assert_eq!(mixed.failed, 3);
    assert_eq!(
        mixed
            .failures
            .iter()
            .filter(|failure| failure.error.contains("must be a JSON object"))
            .count(),
        2
    );
    assert!(!mixed_store
        .search_event_hits("valid sibling survives", 10)
        .unwrap()
        .is_empty());

    let invalid_root = temp.path().join("all-invalid");
    let invalid_transcript = transcript_path(&invalid_root);
    write_raw_fixture(
        &invalid_transcript,
        &[
            "null".to_owned(),
            "false".to_owned(),
            "[1, 2, 3]".to_owned(),
        ],
        "invalid label",
    );
    let mut invalid_store = Store::open(temp.path().join("invalid.sqlite")).unwrap();
    let invalid = import(&invalid_root, &mut invalid_store, ImportProfile::CoreOnly);
    assert_eq!(invalid.failed, 3);
    assert_eq!(invalid.accepted_content_records, 0);
    assert_eq!(
        invalid_store
            .list_sessions()
            .unwrap()
            .into_iter()
            .flat_map(|session| invalid_store.events_for_session(session.id).unwrap())
            .count(),
        0
    );
    let replay = import(&invalid_root, &mut invalid_store, ImportProfile::CoreOnly);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 3);
}

#[test]
fn nativepath_65th_acquisition_unit_starts_another_page() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let mut records =
        Vec::with_capacity(crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS + 1);
    records.push(header("session-unit-boundary"));
    for index in 0..crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS {
        records.push(message(
            &format!("message-{index:03}"),
            "user",
            &format!("normalization unit {index}"),
        ));
    }
    write_fixture(&transcript, &records, "unit boundary");
    let imported_at = "2026-07-25T12:30:00Z".parse().unwrap();
    let accounting =
        native_path::acquisition_page_accounting_for_test(&transcript, imported_at).unwrap();
    assert_eq!(
        accounting
            .iter()
            .map(|(units, _)| *units)
            .collect::<Vec<_>>(),
        vec![
            crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS,
            1
        ]
    );
    assert!(accounting.iter().all(|(units, bytes)| {
        *units <= crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS
            && *bytes <= crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES
    }));

    let mut store = Store::open(temp.path().join("unit-boundary.sqlite")).unwrap();
    let imported = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        imported.imported_events,
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS
    );
}

#[test]
fn nativepath_acquisition_pages_split_before_eight_mib() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let retained_field_bytes =
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES / 2 + 64 * 1024;
    let mut first_message = message("large-1", "user", "first");
    first_message["parentId"] = Value::String("a".repeat(retained_field_bytes));
    let mut second_message = message("large-2", "assistant", "second");
    second_message["parentId"] = Value::String("b".repeat(retained_field_bytes));
    write_fixture(
        &transcript,
        &[
            header("session-byte-boundary"),
            first_message,
            second_message,
        ],
        "byte boundary",
    );
    let imported_at = "2026-07-25T12:30:00Z".parse().unwrap();
    let accounting =
        native_path::acquisition_page_accounting_for_test(&transcript, imported_at).unwrap();
    assert_eq!(accounting.len(), 2);
    assert!(
        accounting.iter().map(|(_, bytes)| *bytes).sum::<usize>()
            > crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES
    );
    assert!(accounting.iter().all(|(units, bytes)| {
        *units <= crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS
            && *bytes <= crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES
    }));

    let mut store = Store::open(temp.path().join("byte-boundary.sqlite")).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).imported_events,
        2
    );
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_core_before_page: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "openclaw-nativepath-test-materializer-v1"
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
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

#[derive(Default)]
struct FailingSink {
    behind: AtomicBool,
}

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "openclaw-nativepath-failing-materializer-v1"
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
        Err(ProOutputSinkError::new(
            "intentional_test_failure",
            "output sink failure",
        ))
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    import_openclaw_history(
        root,
        store,
        OpenClawImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:30:00Z".parse().unwrap(),
            import_profile,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap()
}

fn openclaw_session(store: &Store) -> ctx_history_core::Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::OpenClaw
                && session.role_hint.as_deref() != Some("relationship_placeholder")
        })
        .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agents/personal-agent/sessions/session-1.jsonl")
}

fn header(id: &str) -> Value {
    json!({
        "type": "session",
        "id": id,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/openclaw",
    })
}

fn message(id: &str, role: &str, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": role,
            "content": content,
        }
    })
}

fn tool_result(id: &str, exit_code: i32, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "message": {
            "role": "tool",
            "name": "bash",
            "tool_call_id": format!("call-{id}"),
            "exit_code": exit_code,
            "duration_ms": 17,
            "content": content,
            "input": {"command": format!("command-{id}")},
        }
    })
}

fn write_fixture(path: &Path, records: &[Value], label: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
    fs::write(
        path.parent().unwrap().join("sessions.json"),
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": label,
            },
            "session-output": {
                "sessionId": "session-output",
                "label": label,
            },
            "session-tail": {
                "sessionId": "session-tail",
                "label": label,
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn write_raw_fixture(path: &Path, records: &[String], label: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, records.join("\n") + "\n").unwrap();
    fs::write(
        path.parent().unwrap().join("sessions.json"),
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": label,
            }
        })
        .to_string(),
    )
    .unwrap();
}
