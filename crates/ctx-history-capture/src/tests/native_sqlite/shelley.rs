use crate::complete_content::{
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::providers::shelley::{
    shelley_event_index, shelley_value_text, ShelleyMessageRow,
};
use crate::tests::native_sqlite::shelley_fixtures::write_shelley_adversarial_db;
use crate::tests::support::fixtures::sqlite::write_shelley_smoke_db;
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::{
    assert_provider_policy_cursor_restored, delete_event_and_downgrade_provider_policy_cursor,
    only_provider_cursor_stream, stored_provider_session_id,
};
use crate::{
    import_shelley_sqlite, ShelleySqliteImportOptions, PROVIDER_MAX_TEXT_CHARS,
    SHELLEY_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;

use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, CertifiedProviderCursor,
};

#[test]
fn native_shelley_imports_sessions_messages_metadata_and_citations() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 3);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    assert!(store
        .get_session(parent_id)
        .unwrap()
        .sync
        .metadata
        .to_string()
        .contains("queued oracle"));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        fixture.to_str()
    );
    assert_eq!(source.descriptor.provider, CaptureProvider::Shelley);

    let events = store.events_for_session(parent_id).unwrap();
    assert_eq!(events.len(), 3);
    let agent_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent"))
        .expect("Shelley agent event imported");
    let tool_result_event = events
        .iter()
        .find(|event| {
            event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-tool-result")
        })
        .expect("Shelley tool-result event imported");
    assert_eq!(agent_event.event_type, EventType::ToolCall);
    assert_eq!(tool_result_event.event_type, EventType::ToolOutput);
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &tool_result_event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .expect("Shelley tool result has a bounded verified-content locator");
    let result_locator = locators
        .locator(VerifiedContentRole::ResultBody)
        .expect("Shelley tool result has a result-body route");
    assert_eq!(result_locator.native_record_id(), "msg-tool-result");
    assert_eq!(result_locator.source_locator().unwrap().value()[0], 2);
    let locator_json = serde_json::to_string(result_locator).unwrap();
    assert!(!locator_json.contains(fixture.to_string_lossy().as_ref()));
    assert!(!locator_json.contains("0123456789abcdef0123456789abcdef01234567"));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("shelley search oracle"));
    assert!(rendered.contains("thinking through the search"));
    assert!(rendered.contains("tool call: bash"));
    assert!(rendered.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(rendered.contains("https://github.com/ctxrs/ctx/pull/123"));
    assert!(rendered.contains("toolu_1"));
    assert!(rendered.contains("claude-opus-4-7"));
    assert!(rendered.contains("https://api.anthropic.com/v1/messages"));
    let user_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .expect("Shelley user event imported");
    assert!(user_event
        .sync
        .metadata
        .to_string()
        .contains("conversation:shelley-root:sequence:1:message:msg-user"));

    let cursor_path = provider_path_identity(&fs::canonicalize(&fixture).unwrap()).unwrap();
    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_source_cursor_stream_for_path(
                CaptureProvider::Shelley,
                SHELLEY_SQLITE_SOURCE_FORMAT,
                &cursor_path,
            ),
        )
        .unwrap()
        .unwrap();
    assert!(CertifiedProviderCursor::decode_if_certified(&cursor.cursor)
        .unwrap()
        .is_some());
}

#[test]
fn native_shelley_reimport_is_idempotent() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 4);

    let second = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 0);
    assert_eq!(second.skipped_events, 0);
    assert_eq!(second.skipped_edges, 0);
}

#[test]
fn native_shelley_policy_upgrade_repairs_once_then_is_terminal_noop() {
    let temp = tempdir();
    let database = temp.path().join("work.sqlite");
    let fixture = write_shelley_smoke_db(&temp);
    let machine_id = "shelley-policy-upgrade-machine";
    let options = ShelleySqliteImportOptions {
        machine_id: machine_id.to_owned(),
        source_path: Some(fixture.clone()),
        imported_at: "2026-06-24T12:20:00Z".parse().unwrap(),
        ..ShelleySqliteImportOptions::default()
    };
    let mut store = Store::open(&database).unwrap();

    let first = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_events, 4);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let output = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("Shelley result exists before simulated legacy upgrade");
    let stream = only_provider_cursor_stream(&database, machine_id);
    let policy_revision = delete_event_and_downgrade_provider_policy_cursor(
        &database, &store, machine_id, &stream, output.id,
    );

    let repaired = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(repaired.failed, 0, "{repaired:?}");
    assert_eq!(repaired.imported_events, 1);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 3);
    assert_provider_policy_cursor_restored(&store, machine_id, &stream, policy_revision);

    let terminal = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(terminal.failed, 0, "{terminal:?}");
    assert_eq!(terminal.imported_events, 0);
    assert_eq!(terminal.skipped_events, 0);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 3);
}

#[test]
fn native_shelley_handles_duplicate_sequences_and_nonchat_rows() {
    let temp = tempdir();
    let fixture = write_shelley_adversarial_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-adversarial");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("duplicate sequence first"));
    assert!(rendered.contains("duplicate sequence second"));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::VcsChange));
    assert!(!rendered.contains("commit abc touched shelley.rs"));
    assert!(events
        .iter()
        .any(|event| event.sync.metadata["metadata"]["message_type"].as_str() == Some("warning")));

    let large = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-large"))
        .expect("large Shelley event imported");
    assert_eq!(
        large.payload["body"]["text_retention"]["truncated"].as_bool(),
        Some(true)
    );
    assert_eq!(
        large.payload["body"]["text_retention"]["limit_chars"].as_u64(),
        Some(PROVIDER_MAX_TEXT_CHARS as u64)
    );
    assert!(
        large.payload["body"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= PROVIDER_MAX_TEXT_CHARS
    );
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &large.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .expect("truncated Shelley message has a verified-content locator");
    assert!(locators.locator(VerifiedContentRole::MessageBody).is_some());
}

#[test]
fn native_shelley_text_extraction_is_not_duplicate_or_unbounded() {
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": "once"}
        ]
    }))
    .unwrap();
    assert_eq!(text, "once");

    let huge = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200);
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": huge},
            {"Type": 2, "Text": "after cap"}
        ]
    }))
    .unwrap();
    assert_eq!(text.chars().count(), PROVIDER_MAX_TEXT_CHARS + 1);
    assert!(!text.contains("after cap"));
}

#[test]
fn native_shelley_event_index_uses_stable_message_identity() {
    let message = ShelleyMessageRow {
        rowid: 1,
        message_id: "msg-stable".to_owned(),
        conversation_id: "conv-stable".to_owned(),
        sequence_id: 42,
        entry_type: "user".to_owned(),
        llm_data: None,
        user_data: None,
        usage_data: None,
        created_at: None,
        display_data: None,
        excluded_from_context: false,
        generation: None,
        llm_api_url: None,
        model_name: None,
        forked_from_message_id: None,
    };
    let mut moved_row = message.clone();
    moved_row.rowid = 999;
    let mut duplicate_sequence = message.clone();
    duplicate_sequence.message_id = "msg-stable-other".to_owned();

    assert_eq!(
        shelley_event_index(&message),
        shelley_event_index(&moved_row)
    );
    assert_ne!(
        shelley_event_index(&message),
        shelley_event_index(&duplicate_sequence)
    );
}
