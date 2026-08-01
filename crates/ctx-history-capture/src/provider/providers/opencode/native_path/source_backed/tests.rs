use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, EventRole, EventType, TypedKey};
use ctx_history_index::{CoreSourceEventPage, VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use super::register_source_backed_route;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("projection.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let body = if searchable.is_empty()"));
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
fn metadata_only_session_message_yields_message_part_events_through_production_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_metadata_and_message_part_fixture(&database);

    let page = project_fixture(&database, temp.path());

    assert!(page.terminal);
    assert_eq!(
        page.source.schema_variant(),
        "opencode-family-message_part-v1"
    );
    assert_eq!(page.items.len(), 2);
    for item in &page.items {
        assert_eq!(
            item.event.event_type.parse::<EventType>().unwrap(),
            EventType::Message
        );
        assert_eq!(item.event.role, item.core_record.role);
    }
    let mut projected = page
        .items
        .iter()
        .map(|item| {
            (
                item.core_record.content.normalized_body.clone().unwrap(),
                item.event.role.clone().unwrap(),
                item.event.native_event_id.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    projected.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        projected,
        vec![
            (
                "assistant conversation from part".to_owned(),
                EventRole::Assistant.as_str().to_owned(),
                TypedKey::Utf8("part-assistant".to_owned()),
            ),
            (
                "user conversation from part".to_owned(),
                EventRole::User.as_str().to_owned(),
                TypedKey::Utf8("part-user".to_owned()),
            ),
        ]
    );
}

#[test]
fn agent_switched_current_event_has_canonical_unknown_role_through_production_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_agent_switched_fixture(&database);

    let page = project_fixture(&database, temp.path());

    assert!(page.terminal);
    assert_eq!(
        page.source.schema_variant(),
        "opencode-family-session_message_seq-v1"
    );
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(
        item.event.event_type.parse::<EventType>().unwrap(),
        EventType::Notice
    );
    assert_eq!(
        item.event
            .role
            .as_deref()
            .unwrap()
            .parse::<EventRole>()
            .unwrap(),
        EventRole::Unknown
    );
    assert_eq!(
        item.core_record.role.as_deref(),
        Some(EventRole::Unknown.as_str())
    );
    assert_eq!(
        item.core_record.content.normalized_body.as_deref(),
        Some("agent switched from build to plan")
    );
    assert_eq!(
        item.event.native_event_id,
        Some(TypedKey::Utf8("metadata-agent".to_owned()))
    );
}

fn project_fixture(database: &Path, root: &Path) -> CoreSourceEventPage {
    let data_root = root.join("data-root");
    let index_root = root.join("index");
    let source = provider_source_for_path(CaptureProvider::OpenCode, database.to_path_buf());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &data_root,
    )
    .unwrap();
    let refresh = refresh_source_backed_generation(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(refresh.sources.len(), 1);
    let source = refresh.sources[0].observation().source().clone();
    VerifiedIndex::open(index_root)
        .unwrap()
        .core_source_event_page(&source, None, 16)
        .unwrap()
}

fn create_metadata_and_message_part_fixture(path: &Path) {
    let connection = create_current_fixture(path);
    connection
        .execute_batch(
            r#"create table message (
                 id text primary key,
                 session_id text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create table part (
                 id text primary key,
                 message_id text not null,
                 session_id text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             insert into session_message values (
                 'metadata-agent', 'session-1', 'agent-switched', 1, 1, 1,
                 '{"text":"metadata agent notice must not be emitted"}'
             );
             insert into session_message values (
                 'metadata-model', 'session-1', 'model-switched', 2, 2, 2,
                 '{"text":"metadata model notice must not be emitted"}'
             );
             insert into message values (
                 'message-user', 'session-1', 10, 10, '{"role":"user"}'
             );
             insert into part values (
                 'part-user', 'message-user', 'session-1', 11, 11,
                 '{"type":"text","text":"user conversation from part"}'
             );
             insert into message values (
                 'message-assistant', 'session-1', 20, 20, '{"role":"assistant"}'
             );
             insert into part values (
                 'part-assistant', 'message-assistant', 'session-1', 21, 21,
                 '{"type":"text","text":"assistant conversation from part"}'
             );"#,
        )
        .unwrap();
}

fn create_agent_switched_fixture(path: &Path) {
    let connection = create_current_fixture(path);
    connection
        .execute_batch(
            r#"insert into session_message values (
                 'metadata-agent', 'session-1', 'agent-switched', 1, 2, 2,
                 '{"agent":"plan","text":"agent switched from build to plan"}'
             );"#,
        )
        .unwrap();
}

fn create_current_fixture(path: &Path) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             insert into session values ('session-1', 1, 1);",
        )
        .unwrap();
    connection
}
