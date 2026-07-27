use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::{NormalizedProviderImportOptions, ProviderAdapterContext};

use super::super::import_goose_sessions_sqlite_batched;
use super::{create_goose_tables, insert_message, insert_session};

#[test]
fn goose_rowid_traversal_preserves_semantic_event_order_in_store() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("sessions.db");
    let source = Connection::open(&source_path).unwrap();
    create_goose_tables(&source);
    insert_session(&source, "semantic-order");
    insert_message(
        &source,
        1,
        "semantic-order",
        "physically first, semantically late",
    );
    insert_message(
        &source,
        2,
        "semantic-order",
        "physically second, semantically early",
    );
    source
        .execute(
            "update messages set created_timestamp = case id when 1 then 200 else 100 end",
            [],
        )
        .unwrap();
    drop(source);
    let context = ProviderAdapterContext {
        machine_id: "goose-semantic-order".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(temp.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_goose_sessions_sqlite_batched(
        &source_path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.imported_events, 2);
    let session = store
        .session_by_external_session(CaptureProvider::Goose, "semantic-order")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].payload.to_string().contains("semantically early"));
    assert!(events[1].payload.to_string().contains("semantically late"));
}
