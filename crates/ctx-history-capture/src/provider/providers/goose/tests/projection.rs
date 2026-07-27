use crate::provider::normalization::text_id_index;
use crate::{NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary};
use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::super::{
    goose_timestamp, import_goose_sessions_sqlite_batched, stream::goose_oversize_limit,
};
use super::{create_goose_tables, insert_message, insert_session};

#[test]
fn goose_production_import_rejects_bad_message_rows_and_persists_valid_siblings() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "valid-session");
    insert_session(&source, "empty-session");
    insert_session(&source, "rejected-parent-session");
    source
        .execute(
            "update sessions set name = zeroblob(?1) where id = 'rejected-parent-session'",
            [i64::try_from(goose_oversize_limit().unwrap() + 1).unwrap()],
        )
        .unwrap();
    insert_message(&source, 1, "valid-session", "valid sibling message");
    insert_message(&source, 2, "missing-session", "orphan message");
    insert_message(&source, 3, "valid-session", "malformed sibling message");
    insert_message(
        &source,
        4,
        "rejected-parent-session",
        "orphaned child with touch",
    );
    source
        .execute(
            "update messages set role = 'assistant', content_json = ?1 where id = 4",
            [json!([{
                "type": "toolRequest",
                "toolCall": {
                    "name": "write_file",
                    "arguments": {"path": "src/orphaned-goose.rs"},
                },
            }])
            .to_string()],
        )
        .unwrap();
    source
        .execute(
            "update messages set content_json = '{not-json' where id = 3",
            [],
        )
        .unwrap();
    drop(source);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "goose-production-rejections".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(temp.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let summary = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 4, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), 4);
    assert!(summary.failures.iter().any(|failure| {
        failure.error == "Goose message message-2 references missing session missing-session"
            && failure.line == 7_308_627_157_275
    }));
    assert!(summary.failures.iter().any(|failure| {
        failure.error
            == "invalid JSON in Goose message message-3 content_json: key must be a string at line 1 column 2"
            && failure.line == 7_308_627_165_032
    }));
    assert_eq!(
        summary
            .failures
            .iter()
            .filter(|failure| failure
                .error
                .contains("not already persisted for its exact source"))
            .count(),
        1,
        "the orphaned child must produce exactly one deterministic session rejection"
    );
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 1);

    let valid = store
        .session_by_external_session(CaptureProvider::Goose, "valid-session")
        .unwrap()
        .unwrap();
    assert_eq!(valid.external_agent_id.as_deref(), Some("test-provider"));
    assert_eq!(valid.role_hint.as_deref(), Some("chat"));
    assert_eq!(
        valid.started_at,
        goose_timestamp(Some("2026-07-18 00:00:00"), context.imported_at)
    );
    let valid_source = store
        .capture_source_by_external_session(CaptureProvider::Goose, "valid-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        valid_source.descriptor.cwd.as_deref(),
        Some("/workspace/goose")
    );
    let valid_events = store.events_for_session(valid.id).unwrap();
    assert_eq!(valid_events.len(), 1);
    assert!(valid_events[0]
        .payload
        .to_string()
        .contains("valid sibling message"));
    let empty = store
        .session_by_external_session(CaptureProvider::Goose, "empty-session")
        .unwrap()
        .unwrap();
    assert!(store.events_for_session(empty.id).unwrap().is_empty());
    assert!(store
        .session_by_external_session(CaptureProvider::Goose, "missing-session")
        .unwrap()
        .is_none());
    assert!(store
        .session_by_external_session(CaptureProvider::Goose, "rejected-parent-session")
        .unwrap()
        .is_none());
    assert!(store.export_archive().unwrap().files_touched.is_empty());

    let replay = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        replay,
        ProviderImportSummary {
            failed: 4,
            ..ProviderImportSummary::default()
        }
    );
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(store.events_for_session(valid.id).unwrap().len(), 1);
}

#[test]
fn goose_streams_exact_ordered_touches_after_capture_with_distinct_source_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "streaming-touch-session");
    insert_message(&source, 1, "streaming-touch-session", "placeholder");
    let content = Value::Array(
        ["src/first.rs", "src/second.rs", "src/third.rs"]
            .into_iter()
            .map(|path| {
                json!({
                   "type": "toolRequest",
                   "toolCall": {
                       "name": "write_file",
                       "arguments": {"path": path},
                   },
                })
            })
            .collect(),
    );
    source
        .execute(
            "update messages set role = 'assistant', content_json = ?1 where id = 1",
            [content.to_string()],
        )
        .unwrap();
    drop(source);

    let context = ProviderAdapterContext {
        machine_id: "goose-streaming-touches".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(temp.path().join("configured-source-root")),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let configured_source_root = context.source_root_display().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    let event_index = 1_784_332_801_u64
        .saturating_mul(4_096)
        .saturating_add(text_id_index("message-1", 0) % 4_096);
    let session = store
        .session_by_external_session(CaptureProvider::Goose, "streaming-touch-session")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 3);
    for (index, path) in ["src/first.rs", "src/second.rs", "src/third.rs"]
        .into_iter()
        .enumerate()
    {
        let touch = archive
            .files_touched
            .iter()
            .find(|touch| touch.path == path)
            .unwrap();
        assert_eq!(touch.event_id, Some(events[0].id));
        assert_eq!(
            touch.sync.metadata["provider_touch_index"],
            (event_index << 16) | u64::try_from(index).unwrap()
        );
        assert_eq!(touch.sync.metadata["provider_event_index"], event_index);
        assert_eq!(
            touch.sync.metadata["raw_source_path"],
            source_path.display().to_string()
        );
        assert_eq!(touch.sync.metadata["source_root"], configured_source_root);
        assert_eq!(
            touch.sync.metadata["metadata"],
            json!({
                "source": "structured_provider_payload",
                "path_key": "path",
            })
        );
    }

    let replay = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay, ProviderImportSummary::default());
    assert_eq!(store.export_archive().unwrap().files_touched.len(), 3);
}
