use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;

use crate::{
    CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult,
};

use super::{import_crush_nativepath, projection::crush_normalized_result_content};

fn create_crush_tables(conn: &Connection) {
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            parent_session_id text,
            title text,
            prompt_tokens integer,
            completion_tokens integer,
            cost real,
            created_at integer,
            updated_at integer,
            summary_message_id text
        );
        create table messages (
            id text primary key,
            session_id text not null,
            role text not null,
            parts text not null,
            created_at integer,
            updated_at integer,
            provider text,
            model text,
            is_summary_message integer not null default 0
        );
        create table files (
            session_id text,
            path text not null,
            version text,
            created_at integer,
            updated_at integer
        );
        create table read_files (
            session_id text not null,
            path text not null,
            read_at integer
        );",
    )
    .unwrap();
}

fn insert_session(conn: &Connection, id: &str, parent: Option<&str>) {
    conn.execute(
        "insert into sessions (
            id, parent_session_id, title, prompt_tokens, completion_tokens, cost,
            created_at, updated_at, summary_message_id
         ) values (?1, ?2, 'Crush test', 1, 1, 0.0, 1000, 2000, null)",
        (id, parent),
    )
    .unwrap();
}

fn insert_message(
    conn: &Connection,
    id: &str,
    session_id: &str,
    role: &str,
    parts: &str,
    created_at: i64,
) {
    conn.execute(
        "insert into messages (
            id, session_id, role, parts, created_at, updated_at, provider, model,
            is_summary_message
         ) values (?1, ?2, ?3, ?4, ?5, ?5, 'test', 'model', 0)",
        (id, session_id, role, parts, created_at),
    )
    .unwrap();
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "crush-nativepath-tests".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-25T12:00:00Z".parse::<DateTime<Utc>>().unwrap(),
    }
}

fn session_events(store: &Store, external_id: &str) -> Vec<ctx_history_core::Event> {
    let session = store
        .session_by_external_session(CaptureProvider::Crush, external_id)
        .unwrap()
        .unwrap();
    store.events_for_session(session.id).unwrap()
}

#[derive(Default)]
struct RecordingSink {
    progress: Mutex<Option<ProOutputProgress>>,
    bodies: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    behind: AtomicUsize,
    fail_pages: bool,
}

impl RecordingSink {
    fn failing() -> Self {
        Self {
            fail_pages: true,
            ..Self::default()
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "crush-nativepath-test-materializer-v1"
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
        if self.fail_pages {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "test output sink rejected the page",
            ));
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn result_content_uses_only_ordered_schema_owned_fields() {
    let parts = json!([
        {"type": "text", "data": {"output": "not a result"}},
        {"type": "tool_result", "data": {
            "content": "tool body",
            "output": "lower priority"
        }},
        {"type": "shell_command", "data": {
            "stdout": "shell body",
            "stderr": "lower priority"
        }},
        {"type": "unknown", "data": {"output": "not discovered"}}
    ]);
    assert_eq!(
        crush_normalized_result_content(&parts),
        Some("tool body\nshell body".to_owned())
    );
}

#[test]
fn nativepath_publishes_provider_owned_touch_drafts_canonically() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-touches", None);
    let patch = "*** Begin Patch\n*** Update File: src/patch.rs\n@@\n-old\n+new\n*** Update File: src/patch.rs\n@@\n-old\n+new\n*** End Patch";
    insert_message(
        &conn,
        "message-touch",
        "session-touches",
        "assistant",
        &json!([{
            "type": "tool_call",
            "data": {
                "name": "apply_patch",
                "input": patch,
                "path": "src/structured-fallback.rs"
            }
        }])
        .to_string(),
        1_753_444_800_001,
    );
    conn.execute(
        "insert into files (session_id, path, version, created_at, updated_at)
         values ('session-touches', 'src/file-table.rs', 'v1', 1002, 1003)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into read_files (session_id, path, read_at)
         values ('session-touches', 'src/read-table.rs', 1004)",
        [],
    )
    .unwrap();
    drop(conn);

    let store_path = temp.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let summary = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);

    let conn = Connection::open(&store_path).unwrap();
    let mut statement = conn
        .prepare(
            "select path, change_kind, event_id, metadata_json
             from files_touched
             order by path",
        )
        .unwrap();
    let touches = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(touches.len(), 3);
    assert_eq!(
        touches
            .iter()
            .map(|touch| touch.0.as_str())
            .collect::<Vec<_>>(),
        ["src/file-table.rs", "src/patch.rs", "src/read-table.rs"]
    );

    let patch_touch = touches
        .iter()
        .find(|touch| touch.0 == "src/patch.rs")
        .unwrap();
    assert_eq!(patch_touch.1.as_deref(), Some("modified"));
    let patch_metadata: serde_json::Value = serde_json::from_str(&patch_touch.3).unwrap();
    let provider_event_index = patch_metadata["provider_event_index"].as_u64().unwrap();
    assert!(provider_event_index > (u64::MAX >> 16));
    assert_eq!(patch_metadata["provider_touch_index"].as_u64(), Some(0));
    assert_eq!(patch_metadata["metadata"]["source"], "apply_patch_update");

    let file_touch = touches
        .iter()
        .find(|touch| touch.0 == "src/file-table.rs")
        .unwrap();
    assert_eq!(file_touch.1.as_deref(), Some("modified"));
    assert!(file_touch.2.is_none());
    let file_metadata: serde_json::Value = serde_json::from_str(&file_touch.3).unwrap();
    assert_eq!(file_metadata["metadata"]["source"], "crush_files");

    let read_touch = touches
        .iter()
        .find(|touch| touch.0 == "src/read-table.rs")
        .unwrap();
    assert_eq!(read_touch.1.as_deref(), Some("read"));
    assert!(read_touch.2.is_none());
    let read_metadata: serde_json::Value = serde_json::from_str(&read_touch.3).unwrap();
    assert_eq!(read_metadata["metadata"]["source"], "crush_read_files");
}

#[test]
fn nativepath_core_is_idempotent_and_later_pro_replay_is_independent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-a", None);
    insert_message(
        &conn,
        "message-user",
        "session-a",
        "user",
        &json!([{"type": "text", "data": {"text": "hello"}}]).to_string(),
        1001,
    );
    insert_message(
        &conn,
        "message-output",
        "session-a",
        "tool",
        &json!([{
            "type": "tool_result",
            "data": {"id": "call-a", "content": "PRIVATE-SUCCESS-BODY", "success": true}
        }])
        .to_string(),
        1002,
    );
    drop(conn);

    let store_path = temp.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let first = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    let events = session_events(&store, "session-a");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Message);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("PRIVATE-SUCCESS-BODY"));

    let noop = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    let sink = Arc::new(RecordingSink::default());
    let replay = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.bodies.lock().unwrap().as_slice(),
        [b"PRIVATE-SUCCESS-BODY".to_vec()]
    );
    let pages = sink.pages.load(Ordering::SeqCst);

    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages);
}

#[test]
fn pro_failure_never_blocks_or_rolls_back_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-core-first", None);
    insert_message(
        &conn,
        "message-output",
        "session-core-first",
        "tool",
        &json!([{
            "type": "tool_result",
            "data": {"content": "SUCCESS-ONLY-IN-PRO", "success": true}
        }])
        .to_string(),
        1001,
    );
    drop(conn);

    let sink = Arc::new(RecordingSink::failing());
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let summary = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions {
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .session_by_external_session(CaptureProvider::Crush, "session-core-first")
        .unwrap()
        .is_some());
    assert!(session_events(&store, "session-core-first").is_empty());
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
}

#[test]
fn bounded_restart_corrupt_row_rewrite_and_disappearance_are_safe() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-life", None);
    insert_message(
        &conn,
        "message-corrupt",
        "session-life",
        "assistant",
        "{incomplete",
        1001,
    );
    insert_message(
        &conn,
        "message-valid",
        "session-life",
        "assistant",
        &json!([{"type": "text", "data": {"text": "generation one"}}]).to_string(),
        1002,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let one_group = ProviderImportOptions {
        capture_work_limit: CaptureWorkLimit::OneSafeGroup,
        ..ProviderImportOptions::default()
    };
    let mut saw_failure = false;
    for _ in 0..16 {
        let summary = import_crush_nativepath(
            &source_path,
            &mut store,
            context(&source_path),
            one_group.clone(),
        )
        .unwrap();
        saw_failure |= summary.failed != 0;
        if !summary.work_remaining {
            break;
        }
    }
    assert!(saw_failure);
    assert_eq!(session_events(&store, "session-life").len(), 1);

    let conn = Connection::open(&source_path).unwrap();
    insert_message(
        &conn,
        "message-append",
        "session-life",
        "assistant",
        &json!([{"type": "text", "data": {"text": "appended"}}]).to_string(),
        1003,
    );
    drop(conn);
    let append = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(session_events(&store, "session-life").len(), 2);

    let conn = Connection::open(&source_path).unwrap();
    conn.execute("delete from messages", []).unwrap();
    insert_message(
        &conn,
        "message-replacement",
        "session-life",
        "assistant",
        &json!([{"type": "text", "data": {"text": "generation two"}}]).to_string(),
        2001,
    );
    drop(conn);
    let rewrite = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(
        session_events(&store, "session-life").len(),
        3,
        "a source rewrite must not erase historical Core events"
    );

    let replacement_path = temp.path().join("replacement.db");
    let replacement = Connection::open(&replacement_path).unwrap();
    create_crush_tables(&replacement);
    insert_session(&replacement, "session-life", None);
    insert_message(
        &replacement,
        "message-replaced-file",
        "session-life",
        "assistant",
        &json!([{"type": "text", "data": {"text": "replacement file"}}]).to_string(),
        3001,
    );
    drop(replacement);
    std::fs::rename(&replacement_path, &source_path).unwrap();
    let replaced = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(session_events(&store, "session-life").len(), 4);

    std::fs::remove_file(&source_path).unwrap();
    let retired = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(session_events(&store, "session-life").len(), 4);

    let repeated = import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}
