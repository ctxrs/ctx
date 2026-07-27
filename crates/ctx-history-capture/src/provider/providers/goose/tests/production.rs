use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions,
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
    assert!(core.contains("bounded failure diagnostic"));

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
