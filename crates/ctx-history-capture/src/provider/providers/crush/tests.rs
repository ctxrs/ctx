use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, Event, EventRole, EventType, Fidelity};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;

use crate::{
    complete_content::{
        sqlite::SqliteCompleteContentResolver, AuthorizedSourceRoute, CompleteContentHashAuthority,
        CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
        SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::importer::{
        provider_source_event_seq, provider_source_event_uuid, provider_sync_metadata,
    },
    CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult,
    CRUSH_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    import_crush_nativepath,
    projection::{crush_normalized_result_content, CrushMessageRow},
};

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

fn event_hash(event: &Event) -> &str {
    event
        .sync
        .metadata
        .get("provider_event_hash")
        .and_then(serde_json::Value::as_str)
        .unwrap()
}

fn complete_message_request(
    source_path: &Path,
    event: &Event,
    locator: &crate::complete_content::VerifiedContentLocatorV1,
) -> CompleteMessageRequest {
    let source_id = event.capture_source_id.unwrap();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id,
                provider: CaptureProvider::Crush,
                source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: source_path.to_path_buf(),
                source_root: source_path.parent().map(Path::to_path_buf),
                source_identity: Some("crush-complete-content-test".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event.id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Crush,
        source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: locator.content_profile().to_owned(),
        source_locator: locator.source_locator(),
        provider_session_id: event
            .sync
            .metadata
            .get("provider_session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: event_hash(event).to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(locator.native_record_id().to_owned()),
        expected_record_digest: Some(locator.record_sha256().clone()),
        expected_content_ref: Some(locator.content_ref().clone()),
        indexed_text: event
            .payload
            .pointer("/body/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
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
fn core_output_privacy_uses_only_direct_typed_outcome_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-private-output", None);
    insert_message(
        &conn,
        "nested-only-output",
        "session-private-output",
        "tool",
        &json!([{
            "type": "tool_result",
            "data": {
                "content": {
                    "success": false,
                    "isError": true,
                    "status": "failed",
                    "secret": "NESTED-PRIVATE-OUTPUT"
                }
            }
        }])
        .to_string(),
        1001,
    );
    insert_message(
        &conn,
        "mixed-output",
        "session-private-output",
        "tool",
        &json!([
            {
                "type": "tool_result",
                "data": {
                    "success": true,
                    "content": "PRIVATE-SUCCESS-ARM"
                }
            },
            {
                "type": "tool_result",
                "data": {
                    "success": false,
                    "call_id": "call-mixed",
                    "content": "PRIVATE-FAILURE-ARM"
                }
            }
        ])
        .to_string(),
        1002,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();

    let events = session_events(&store, "session-private-output");
    assert_eq!(events.len(), 1, "nested body metadata must not classify");
    let failure = &events[0];
    assert_eq!(failure.event_type, EventType::ToolOutput);
    assert_eq!(
        failure.payload.pointer("/body/result_outcome"),
        Some(&json!("failure"))
    );
    assert_eq!(
        failure.payload.pointer("/body/call_id"),
        Some(&json!("call-mixed"))
    );
    let retained = serde_json::to_string(failure).unwrap();
    for forbidden in [
        "NESTED-PRIVATE-OUTPUT",
        "PRIVATE-SUCCESS-ARM",
        "PRIVATE-FAILURE-ARM",
        "output_preview",
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    ] {
        assert!(
            !retained.contains(forbidden),
            "Core retained forbidden output material: {forbidden}"
        );
    }
}

#[test]
fn full_native_ids_do_not_share_the_released_twelve_bit_identity_bucket() {
    let mut seen = vec![None::<String>; 4_096];
    let (left, right) = (0_u64..20_000)
        .find_map(|ordinal| {
            let id = format!("crush-collision-{ordinal}");
            let bucket = ((crate::fnv1a64(id.as_bytes()) & 0x0fff_ffff) % 4_096) as usize;
            if let Some(previous) = seen[bucket].as_ref() {
                return Some((previous.clone(), id));
            }
            seen[bucket] = Some(id);
            None
        })
        .expect("pigeonhole collision");
    assert_ne!(
        crate::fnv1a64(left.as_bytes()),
        crate::fnv1a64(right.as_bytes())
    );

    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-collision", None);
    for (id, text) in [(&left, "collision left"), (&right, "collision right")] {
        insert_message(
            &conn,
            id,
            "session-collision",
            "assistant",
            &json!([{"type": "text", "data": {"text": text}}]).to_string(),
            1001,
        );
    }
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let events = session_events(&store, "session-collision");
    assert_eq!(events.len(), 2);
    assert_ne!(events[0].id, events[1].id);
    let indexes = events
        .iter()
        .map(|event| {
            event.sync.metadata["provider_event_index"]
                .as_u64()
                .unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(indexes.len(), 2);
    assert!(indexes.contains(&crate::fnv1a64(left.as_bytes())));
    assert!(indexes.contains(&crate::fnv1a64(right.as_bytes())));
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
fn same_native_id_rewrite_replaces_the_normalized_fallback_event_in_place() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-rewrite", None);
    insert_message(
        &conn,
        "stable-message-id",
        "session-rewrite",
        "assistant",
        &json!([{"type": "text", "data": {"text": "first generation"}}]).to_string(),
        1001,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let first = session_events(&store, "session-rewrite").pop().unwrap();
    let first_hash = event_hash(&first).to_owned();
    assert_eq!(
        first.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );

    let conn = Connection::open(&source_path).unwrap();
    conn.execute(
        "update messages set parts = ?1, updated_at = updated_at + 1 where id = ?2",
        (
            json!([{"type": "text", "data": {"text": "second generation"}}]).to_string(),
            "stable-message-id",
        ),
    )
    .unwrap();
    drop(conn);
    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();

    let events = session_events(&store, "session-rewrite");
    assert_eq!(events.len(), 1);
    let rewritten = &events[0];
    assert_eq!(rewritten.id, first.id);
    assert_ne!(event_hash(rewritten), first_hash);
    assert_eq!(
        event_hash(rewritten),
        crate::compute_payload_hash(&rewritten.payload["body"])
            .unwrap()
            .as_str()
    );
    let serialized = serde_json::to_string(rewritten).unwrap();
    assert!(serialized.contains("second generation"));
    assert!(!serialized.contains("first generation"));
}

#[test]
fn exact_released_id_hash_is_migrated_without_changing_the_event_id() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-released", None);
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let adapter_context = context(&source_path);
    import_crush_nativepath(
        &source_path,
        &mut store,
        adapter_context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Crush, "session-released")
        .unwrap()
        .unwrap();
    let source_id = session.capture_source_id.unwrap();

    let native_id = "released-failure-id";
    let native_row = CrushMessageRow {
        rowid: 1,
        id: native_id.to_owned(),
        session_id: "session-released".to_owned(),
        role: "tool".to_owned(),
        parts: json!([{
            "type": "tool_result",
            "data": {
                "call_id": "released-call",
                "success": false,
                "content": "RELEASED-PRIVATE-FAILURE"
            }
        }])
        .to_string(),
        created_at: Some(1001),
        updated_at: Some(1001),
        provider: Some("test".to_owned()),
        model: Some("model".to_owned()),
        is_summary_message: false,
    };
    let legacy_index = super::projection::legacy_event_index(&native_row);
    let legacy_id = provider_source_event_uuid(source_id, legacy_index);
    let legacy_dedupe = Store::provider_source_event_dedupe_key(source_id, legacy_index, native_id);
    store
        .upsert_event(&Event {
            id: legacy_id,
            seq: provider_source_event_seq(source_id, legacy_index),
            history_record_id: None,
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::ToolOutput,
            role: Some(EventRole::Tool),
            occurred_at: adapter_context.imported_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::Crush.as_str(),
                "provider_session_id": "session-released",
                "provider_event_index": legacy_index,
                "provider_event_hash": native_id,
                "cursor": "released",
                "artifacts": [],
                "body": {
                    "result_outcome": "failure",
                    "call_id": "released-call",
                    "output_preview": "RELEASED-PRIVATE-FAILURE"
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some(legacy_dedupe),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": "session-released",
                    "provider_event_index": legacy_index,
                    "provider_event_hash": native_id,
                    "provider_event_hash_authority": "provider_supplied",
                    "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                    (VERIFIED_CONTENT_LOCATORS_METADATA_KEY): {
                        "released_result_locator": true
                    },
                }),
            ),
        })
        .unwrap();

    let conn = Connection::open(&source_path).unwrap();
    insert_message(
        &conn,
        native_id,
        "session-released",
        "tool",
        &native_row.parts,
        1001,
    );
    drop(conn);
    import_crush_nativepath(
        &source_path,
        &mut store,
        adapter_context,
        ProviderImportOptions::default(),
    )
    .unwrap();

    let events = session_events(&store, "session-released");
    assert_eq!(events.len(), 1);
    let migrated = &events[0];
    assert_eq!(migrated.id, legacy_id);
    assert_eq!(
        migrated.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_eq!(migrated.event_type, EventType::ToolOutput);
    assert_eq!(migrated.sync.metadata["native_record_id"], json!(native_id));
    assert_eq!(
        event_hash(migrated),
        crate::compute_payload_hash(&migrated.payload["body"])
            .unwrap()
            .as_str()
    );
    assert!(migrated
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(event_hash(migrated)));
    let retained = serde_json::to_string(migrated).unwrap();
    assert!(!retained.contains("RELEASED-PRIVATE-FAILURE"));
    assert!(!retained.contains("output_preview"));
    assert!(!retained.contains(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
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
fn null_session_rejections_survive_restart_and_noop_with_bounded_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            created_at integer,
            updated_at integer
        );
        create table messages (
            id text primary key,
            session_id text,
            role text,
            parts text,
            created_at integer,
            updated_at integer
        );
        insert into sessions values ('valid-session', 1000, 1000);",
    )
    .unwrap();
    for ordinal in 0..70 {
        conn.execute(
            "insert into messages (
                id, session_id, role, parts, created_at, updated_at
             ) values (?1, null, 'assistant', '[]', ?2, ?2)",
            (format!("null-session-{ordinal}"), 2000 + ordinal),
        )
        .unwrap();
    }
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
    assert_eq!(first.failed, 70);
    assert_eq!(
        first.failures.len(),
        crate::summaries::MAX_RETAINED_PROVIDER_FAILURES
    );
    assert!(first
        .failures
        .iter()
        .all(|failure| failure.error.contains("could not be decoded")));
    let evidence = first.failures.clone();
    drop(store);

    let mut restarted = Store::open(&store_path).unwrap();
    let noop = import_crush_nativepath(
        &source_path,
        &mut restarted,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(noop.failed, 70);
    assert_eq!(noop.failures, evidence);
    assert!(session_events(&restarted, "valid-session").is_empty());
}

#[test]
fn message_locator_reconstructs_normalized_hash_and_fails_closed_after_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let original_body = format!(
        "original complete body\n{}",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let conn = Connection::open(&source_path).unwrap();
    create_crush_tables(&conn);
    insert_session(&conn, "session-complete", None);
    insert_message(
        &conn,
        "complete-message-id",
        "session-complete",
        "assistant",
        &json!([{"type": "text", "data": {"text": original_body}}]).to_string(),
        1001,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import_crush_nativepath(
        &source_path,
        &mut store,
        context(&source_path),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let event = session_events(&store, "session-complete").pop().unwrap();
    assert_eq!(
        event.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators
        .locator(VerifiedContentRole::MessageBody)
        .unwrap()
        .clone();
    assert_eq!(locator.native_record_id(), "complete-message-id");

    let resolved = SqliteCompleteContentResolver::new()
        .resolve(&[complete_message_request(&source_path, &event, &locator)])
        .unwrap();
    assert_eq!(resolved[0].text, original_body);

    let conn = Connection::open(&source_path).unwrap();
    conn.execute(
        "update messages set created_at = 2001 where id = 'complete-message-id'",
        [],
    )
    .unwrap();
    let rowid = conn
        .query_row(
            "select rowid from messages where id = 'complete-message-id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mutated_values = super::load_crush_message_values(&conn, rowid).unwrap();
    let (_, _, mutated_hash, mutated_text) =
        super::crush_complete_message(&mutated_values).unwrap();
    assert_eq!(mutated_text, original_body);
    assert_ne!(mutated_hash, event_hash(&event));
    let mutated_record_digest = super::capture::message_record_digest(&mutated_values).unwrap();
    drop(conn);
    let mut mutated_request = complete_message_request(&source_path, &event, &locator);
    mutated_request.expected_record_digest = Some(mutated_record_digest);
    assert!(SqliteCompleteContentResolver::new()
        .resolve(&[mutated_request])
        .is_err());
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
