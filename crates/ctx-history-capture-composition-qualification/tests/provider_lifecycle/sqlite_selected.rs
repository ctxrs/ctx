use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use crate::{provider_sources::provider_source_for_path, test_support_paths};
use ctx_history_capture_composition::{
    refresh_source_backed_generation, register_goose_source_backed_route,
    register_landed_source_backed_route_with_data_root, SourceBackedCoordinatorError,
    SourceBackedProviderRegistry, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
};

#[test]
fn goose_v15_literal_parent_and_native_identities_publish_consistently() {
    let temp = test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "create table schema_version (version integer not null);
             insert into schema_version values (15);
             create table sessions (
                 id text primary key,
                 working_dir text,
                 parent_session_id text
             );
             create table messages (
                 id integer primary key,
                 message_id text,
                 session_id text not null,
                 role text not null,
                 content_json text not null,
                 created_timestamp integer
             );
             insert into sessions values
                 ('parent-session', '/tmp/goose', null),
                 ('child-session', '/tmp/goose', 'parent-session');
             insert into messages values
                 (1, 'copied-message', 'parent-session', 'user',
                  '[{\"type\":\"text\",\"text\":\"parent copy\"}]', 1),
                 (2, 'copied-message', 'child-session', 'user',
                  '[{\"type\":\"text\",\"text\":\"child copy\"}]', 2),
                 (3, 'tool-message', 'child-session', 'assistant',
                  '[{\"type\":\"toolRequest\",\"toolCall\":{\"id\":\"tool-call-exact\",\"name\":\"read_file\"}}]', 3);",
        )
        .unwrap();
    drop(connection);

    let mut registry = SourceBackedProviderRegistry::new();
    let source = provider_source_for_path(CaptureProvider::Goose, database);
    register_goose_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        &temp.path().join("data-root"),
        temp.path(),
        Vec::new(),
        None,
    )
    .unwrap();
    let index = temp.path().join("index");
    refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    let verified = VerifiedIndex::open(&index).unwrap();
    let source = verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == CaptureProvider::Goose.as_str())
        .unwrap()
        .observation()
        .source()
        .clone();
    let records = verified
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    let parent = records
        .iter()
        .find(|record| record.content.meaningful_text() == "parent copy")
        .unwrap();
    let child = records
        .iter()
        .find(|record| record.content.meaningful_text() == "child copy")
        .unwrap();
    let tool = records
        .iter()
        .find(|record| record.native_event_id == Some(TypedKey::utf8("tool-message").unwrap()))
        .unwrap();

    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, None);
    assert!(super::has_literal_fact(
        parent,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/goose"
    ));
    assert!(super::has_literal_fact(
        child,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/goose"
    ));
    assert_eq!(
        parent.native_event_id,
        Some(TypedKey::utf8("copied-message").unwrap())
    );
    assert_eq!(
        child.native_event_id,
        Some(TypedKey::utf8("copied-message").unwrap())
    );
    assert_ne!(parent.event_id, child.event_id);
    assert!(tool
        .content
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/provider_native_tool_call_ids/0"))
        .and_then(serde_json::Value::as_str)
        .is_none());
}

#[test]
fn kiro_schema_failure_reports_cleanup_failure_without_staging_leftovers() {
    let temp = test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("kiro.sqlite");
    let data_root = temp.path().join("ctx-data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider).unwrap();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute_batch(
            "CREATE TABLE unsupported(value TEXT); INSERT INTO unsupported VALUES ('present')",
        )
        .unwrap();
    let source = provider_source_for_path(CaptureProvider::KiroCli, database);
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        &data_root,
    )
    .unwrap();
    ctx_history_providers_sqlite_selected::fail_next_opened_snapshot_cleanup_for_test();

    let error = refresh_source_backed_generation(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    let SourceBackedCoordinatorError::RouteScan { source, .. } = error else {
        panic!("unexpected Kiro refresh error: {error:?}");
    };
    assert_eq!(source.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert!(source.detail.contains("cleanup_status=failed"));
    let staging = data_root.join("tmp/provider-sqlite");
    assert!(staging.is_dir());
    assert_directory_empty(&staging);
}

fn assert_directory_empty(path: &Path) {
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}
