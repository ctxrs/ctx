use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    EventType, Fidelity, Session, SessionStatus,
};
use ctx_history_store::Store;
use rusqlite::{params, Connection};
use serde_json::json;
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            provider_scoped_source_uuid, provider_source_event_import_identity,
            provider_source_identity, provider_source_session_uuid, provider_sync_metadata,
            timestamps,
        },
        normalization::text_id_index,
    },
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::super::import_goose_nativepath;
use super::{create_goose_tables, insert_message, insert_session};

fn context(source_path: &std::path::Path, root: &std::path::Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "goose-nativepath-production".to_owned(),
        source_path: Some(source_path.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

fn insert_output(conn: &Connection, id: i64, session_id: &str, result: &str, exit_code: i64) {
    conn.execute(
        "insert into messages (
            id, message_id, session_id, role, content_json, created_timestamp,
            timestamp, tokens, metadata_json
         ) values (?1, ?2, ?3, 'tool', ?4, ?5, '2026-07-18T00:00:01Z', null, null)",
        params![
            id,
            format!("output-{id}"),
            session_id,
            json!([{
                "type": "toolResponse",
                "toolCallId": format!("call-{id}"),
                "toolResult": result,
                "status": if exit_code == 0 { "success" } else { "failure" },
                "exitCode": exit_code
            }])
            .to_string(),
            1_784_332_800_i64.saturating_add(id),
        ],
    )
    .unwrap();
}

fn seed_v025_goose_event(
    store: &Store,
    source_path: &std::path::Path,
    root: &std::path::Path,
    session_identity: &str,
    native_order: i64,
    created_timestamp: i64,
) -> (Uuid, Uuid) {
    let raw_source_path = std::fs::canonicalize(source_path)
        .unwrap()
        .display()
        .to_string();
    let source_identity = provider_source_identity(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        Some(&root.display().to_string()),
        Some(&raw_source_path),
        None,
        &serde_json::Value::Null,
    )
    .unwrap();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Goose,
        session_identity,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let session_id = provider_source_session_uuid(&source_identity, session_identity);
    let observed_at = DateTime::<Utc>::UNIX_EPOCH;
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Goose,
                machine_id: "goose-nativepath-production".to_owned(),
                process_id: None,
                cwd: Some("/workspace/goose".to_owned()),
                raw_source_path: Some(raw_source_path),
                source_format: Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned()),
                source_root: Some(root.display().to_string()),
                source_identity: Some(source_identity),
                external_session_id: Some(session_identity.to_owned()),
            },
            started_at: observed_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"release": "v0.25.0"})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Goose,
            external_session_id: Some(session_identity.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: observed_at,
            ended_at: None,
            timestamps: timestamps(observed_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"release": "v0.25.0"})),
        })
        .unwrap();

    let provider_message_identity = format!("message-{native_order}");
    let legacy_index = u64::try_from(created_timestamp.max(0))
        .unwrap()
        .saturating_mul(4_096)
        .saturating_add(text_id_index(&provider_message_identity, 0) % 4_096);
    let identity =
        provider_source_event_import_identity(source_id, legacy_index, &provider_message_identity);
    store
        .upsert_event(&Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: None,
            occurred_at: observed_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": "goose",
                "provider_session_id": session_identity,
                "provider_event_index": legacy_index,
                "provider_event_hash": provider_message_identity,
                "text": "released v0.25 payload"
            }),
            payload_blob_id: None,
            dedupe_key: Some(identity.dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"release": "v0.25.0"})),
        })
        .unwrap();
    (session_id, identity.id)
}

#[derive(Default)]
struct TestOutputSink {
    fail: bool,
    bodies: Mutex<Vec<String>>,
    behind: Mutex<bool>,
}

impl ProOutputSink for TestOutputSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "goose-production-test-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| String::from_utf8_lossy(&observation.content).into_owned()),
        );
        if self.fail {
            return Err(ProOutputSinkError::new("test_failure", "sink unavailable"));
        }
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        *self.behind.lock().unwrap() = true;
    }
}

#[test]
fn production_upgrades_v025_source_scoped_events_across_unchanged_append_and_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "v025-upgrade");
    insert_message(&source, 1, "v025-upgrade", "unchanged native content");
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (session_id, released_event_id) = seed_v025_goose_event(
        &store,
        &source_path,
        temp.path(),
        "v025-upgrade",
        1,
        1_784_332_801,
    );

    import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let unchanged = store.events_for_session(session_id).unwrap();
    assert_eq!(unchanged.len(), 1);
    assert_eq!(unchanged[0].id, released_event_id);
    assert_eq!(
        unchanged[0]
            .payload
            .get("text")
            .and_then(|value| value.as_str()),
        Some("unchanged native content")
    );
    assert!(unchanged[0]
        .sync
        .metadata
        .pointer("/metadata/event_path")
        .is_none());

    let source = Connection::open(&source_path).unwrap();
    insert_message(&source, 2, "v025-upgrade", "appended native content");
    drop(source);
    import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let appended = store.events_for_session(session_id).unwrap();
    assert_eq!(appended.len(), 2);
    assert!(appended.iter().any(|event| event.id == released_event_id));
    assert!(serde_json::to_string(&appended)
        .unwrap()
        .contains("appended native content"));

    let source = Connection::open(&source_path).unwrap();
    source
        .execute(
            "update messages
             set content_json = ?1
             where id = 1",
            [json!([{"type": "text", "text": "rewritten native content"}]).to_string()],
        )
        .unwrap();
    drop(source);
    import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let rewritten = store.events_for_session(session_id).unwrap();
    assert_eq!(rewritten.len(), 2);
    let released = rewritten
        .iter()
        .find(|event| event.id == released_event_id)
        .unwrap();
    assert_eq!(
        released
            .payload
            .get("text")
            .and_then(|value| value.as_str()),
        Some("rewritten native content")
    );
}

#[test]
fn production_records_rejected_sessions_and_their_children_without_publication_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "accepted-parent");
    insert_message(&source, 1, "accepted-parent", "published child");
    source
        .execute(
            "insert into sessions(id, name) values ('rejected-parent', x'2a')",
            [],
        )
        .unwrap();
    insert_message(&source, 2, "rejected-parent", "rejected child");
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failed, 2);
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("unsupported SQLite storage classes")));
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("missing_session")));
    assert!(store
        .session_by_external_session(CaptureProvider::Goose, "rejected-parent")
        .unwrap()
        .is_none());
}

#[test]
fn production_core_is_idempotent_rewrites_and_excludes_successful_outputs() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "native-production");
    insert_message(&source, 1, "native-production", "before rewrite");
    insert_output(
        &source,
        2,
        "native-production",
        "successful body must stay out of Core",
        0,
    );
    insert_output(
        &source,
        3,
        "native-production",
        "bounded failure diagnostic",
        9,
    );
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session = store
        .session_by_external_session(CaptureProvider::Goose, "native-production")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    let core = serde_json::to_string(&events).unwrap();
    assert!(!core.contains("successful body must stay out of Core"));
    assert!(!core.contains("bounded failure diagnostic"));

    let replay = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);

    let source = Connection::open(&source_path).unwrap();
    source
        .execute(
            "update messages
             set content_json = ?1
             where id = 1",
            [json!([{"type": "text", "text": "after rewrite"}]).to_string()],
        )
        .unwrap();
    insert_message(&source, 4, "native-production", "appended");
    drop(source);

    let changed = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(changed.imported_events, 1);
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 3);
    let core = serde_json::to_string(&events).unwrap();
    assert!(core.contains("after rewrite"));
    assert!(!core.contains("before rewrite"));
    assert!(core.contains("appended"));
}

#[test]
fn production_missing_source_retires_route_without_deleting_history() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "missing-source");
    insert_message(&source, 1, "missing-source", "preserved");
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Goose, "missing-source")
        .unwrap()
        .unwrap();

    std::fs::remove_file(&source_path).unwrap();
    let retired = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        retired.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn production_supports_late_pro_replay_and_pro_failure_never_blocks_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "pro-lifecycle");
    insert_message(&source, 1, "pro-lifecycle", "core-before-pro");
    insert_output(&source, 2, "pro-lifecycle", "late successful output", 0);
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Goose, "pro-lifecycle")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    let late_sink = Arc::new(TestOutputSink::default());
    let replay_options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(late_sink.clone()),
        ..ProviderImportOptions::default()
    };
    let replay = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        replay_options,
    )
    .unwrap();
    assert_eq!(replay.imported, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
    assert!(late_sink
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body.contains("late successful output")));

    let source = Connection::open(&source_path).unwrap();
    insert_message(&source, 3, "pro-lifecycle", "core survives Pro failure");
    insert_output(&source, 4, "pro-lifecycle", "unavailable Pro output", 0);
    drop(source);

    let failing_sink = Arc::new(TestOutputSink {
        fail: true,
        ..TestOutputSink::default()
    });
    let combined_options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(failing_sink.clone()),
        ..ProviderImportOptions::default()
    };
    let combined = import_goose_nativepath(
        &source_path,
        &mut store,
        context(&source_path, temp.path()),
        combined_options,
    )
    .unwrap();
    assert_eq!(combined.imported_events, 1);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
    assert!(*failing_sink.behind.lock().unwrap());
}
