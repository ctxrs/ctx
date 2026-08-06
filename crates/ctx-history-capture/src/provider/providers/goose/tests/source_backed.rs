#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("../source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("record.native_event_id = native_event_id"));
    assert!(production.contains("GOOSE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.searchable_text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn v15_delegated_parent_and_native_identities_publish_consistently() {
    use ctx_history_core::{CaptureProvider, EventOrigin, SessionRelationshipKind, TypedKey};
    use ctx_history_index::{VerifiedIndex, WriterOptions};
    use rusqlite::Connection;

    use crate::provider::source_backed::{
        refresh_source_backed_generation, register_goose_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    };
    use crate::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
    };

    let temp = crate::test_support_paths::tempdir().unwrap();
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
    register_goose_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Goose,
            path: database,
            exists: true,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
        &temp.path().join("data-root"),
        temp.path(),
        Vec::new(),
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

    let source = super::super::source_backed::goose_source_key().unwrap();
    let records = VerifiedIndex::open(&index)
        .unwrap()
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

    assert_eq!(parent.session_relationship, SessionRelationshipKind::Root);
    assert_eq!(parent.event_origin, EventOrigin::Unknown);
    assert_eq!(
        child.session_relationship,
        SessionRelationshipKind::Delegated
    );
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, parent.session_id);
    assert!(!child.is_primary);
    assert_eq!(child.event_origin, EventOrigin::Unknown);
    assert_eq!(
        parent.native_event_id,
        Some(TypedKey::utf8("copied-message").unwrap())
    );
    assert_eq!(
        child.native_event_id,
        Some(TypedKey::utf8("copied-message").unwrap())
    );
    assert_ne!(parent.event_id, child.event_id);
    assert_eq!(
        tool.content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/provider_native_tool_call_ids/0"))
            .and_then(serde_json::Value::as_str),
        Some("tool-call-exact")
    );
    assert_eq!(tool.event_origin, EventOrigin::Unknown);
}
