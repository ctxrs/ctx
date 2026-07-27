use crate::complete_content::sqlite::SqliteCompleteContentResolver;
use crate::complete_content::{
    AuthorizedSourceRoute, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
    SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::providers::shelley::{
    shelley_event_index, shelley_value_text, ShelleyMessageRow,
};
use crate::tests::native_sqlite::shelley_fixtures::write_shelley_adversarial_db;
use crate::tests::support::fixtures::sqlite::write_shelley_smoke_db;
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_shelley_sqlite, native_source::NativePosition, ProviderImportWorkResult,
    ShelleySqliteImportOptions, PROVIDER_MAX_TEXT_CHARS, SHELLEY_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EntityTimestamps, EventType, SyncCursor};
use ctx_history_store::{decode_native_path_committed_cursor, ProviderEventHashAuthority, Store};
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use uuid::Uuid;

use crate::provider::importer::{
    provider_path_identity, provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
    provider_source_event_seq, provider_source_event_uuid, provider_source_identity,
    provider_source_session_uuid, BoundedParserCheckpoint, CertifiedProviderCursor,
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
    assert_eq!(summary.imported_events, 3);
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
    assert_eq!(events.len(), 2);
    let agent_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent"))
        .expect("Shelley agent event imported");
    assert_eq!(agent_event.event_type, EventType::ToolCall);
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("shelley search oracle"));
    assert!(rendered.contains("thinking through the search"));
    assert!(rendered.contains("tool call: bash"));
    assert!(!rendered.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(!rendered.contains("https://github.com/ctxrs/ctx/pull/123"));
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
    let committed = decode_native_path_committed_cursor(&cursor.cursor).unwrap();
    let authority: serde_json::Value = serde_json::from_str(committed.provider_cursor()).unwrap();
    assert_eq!(authority["version"].as_u64(), Some(2));
    assert_eq!(authority["provider"].as_str(), Some("shelley"));
    assert_eq!(
        authority["path_identity"].as_str(),
        Some(cursor_path.as_str())
    );
    assert_eq!(authority["phase"].as_str(), Some("complete"));
    assert_eq!(authority["terminal"].as_bool(), Some(true));
    assert_eq!(authority["route_retired"].as_bool(), Some(false));
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
    assert_eq!(first.imported_events, 3);

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
fn native_shelley_path_alias_append_keeps_source_session_and_event_identity() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let alias_parent = temp.path().join("alias");
    fs::create_dir(&alias_parent).unwrap();
    let alias = alias_parent.join("..").join("shelley.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(
        &alias,
        &mut store,
        shelley_options("shelley-alias-machine", alias.clone()),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{first:?}");
    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let original_event_ids = event_ids_by_message(&store, session);

    Connection::open(&fixture)
        .unwrap()
        .execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, created_at
             ) values (
                'msg-alias-append', 'shelley-root', 9, 'user', ?1,
                '2026-06-24 12:00:09'
             )",
            [json!({"Content": [{"Type": 2, "Text": "alias append"}]}).to_string()],
        )
        .unwrap();
    let appended = import_shelley_sqlite(
        &fixture,
        &mut store,
        shelley_options("shelley-alias-machine", fixture.clone()),
    )
    .unwrap();
    assert_eq!(appended.failed, 0, "{appended:?}");
    assert_eq!(appended.imported_events, 1);

    let stable_source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(stable_source.id, source.id);
    assert_eq!(
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root"),
        session
    );
    let appended_ids = event_ids_by_message(&store, session);
    for (message_id, event_id) in original_event_ids {
        assert_eq!(appended_ids.get(&message_id), Some(&event_id));
    }
    assert!(appended_ids.contains_key("msg-alias-append"));
}

#[test]
fn native_shelley_rewrite_and_truncate_update_in_place_without_generation_ids() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let options = shelley_options("shelley-rewrite-machine", fixture.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();

    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let before = event_ids_by_message(&store, session);
    let rewritten = json!({
        "Content": [{"Type": 2, "Text": "rewritten Shelley prompt"}]
    })
    .to_string();
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update messages set user_data = ?1 where message_id = 'msg-user'",
        [rewritten],
    )
    .unwrap();
    drop(conn);

    let rewrite = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(rewrite.failed, 0, "{rewrite:?}");
    let after_rewrite = event_ids_by_message(&store, session);
    assert_eq!(after_rewrite, before);
    let user = store.get_event(before["msg-user"]).unwrap();
    assert!(user
        .payload
        .to_string()
        .contains("rewritten Shelley prompt"));

    Connection::open(&fixture)
        .unwrap()
        .execute("delete from messages where message_id = 'msg-agent'", [])
        .unwrap();
    let truncate = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(truncate.failed, 0, "{truncate:?}");
    assert_eq!(
        store
            .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
            .unwrap()
            .unwrap()
            .id,
        source.id
    );
    assert_eq!(
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root"),
        session
    );
    assert_eq!(event_ids_by_message(&store, session), before);
}

#[test]
fn native_shelley_known_compact_index_collision_uses_stable_full_tuple_alternate() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    replace_with_collision_fixture(&fixture);
    let options = shelley_options("shelley-collision-machine", fixture.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_events, 2);
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "conv");
    let events = store.events_for_session(session).unwrap();
    assert_eq!(events.len(), 2);
    let ids = event_ids_by_message(&store, session);
    assert_ne!(ids["msg-32719"], ids["msg-150040"]);
    let indexes = events
        .iter()
        .map(|event| event.payload["provider_event_index"].as_u64().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(indexes.len(), 2);

    let first_message = collision_message("msg-32719");
    let second_message = collision_message("msg-150040");
    assert_eq!(
        shelley_event_index(&first_message),
        shelley_event_index(&second_message)
    );
    assert!(indexes.contains(&shelley_event_index(&first_message)));

    Connection::open(&fixture)
        .unwrap()
        .execute(
            "update messages set user_data = ?1 where message_id = 'msg-150040'",
            [json!({"Content": [{"Type": 2, "Text": "collision rewrite"}]}).to_string()],
        )
        .unwrap();
    let rewritten = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(rewritten.failed, 0, "{rewritten:?}");
    assert_eq!(event_ids_by_message(&store, session), ids);
    assert_eq!(store.events_for_session(session).unwrap().len(), 2);
}

#[test]
fn native_shelley_invalid_utf8_rejects_only_the_addressed_row() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    replace_with_invalid_utf8_fixture(&fixture);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        shelley_options("shelley-utf8-machine", fixture.clone()),
    )
    .unwrap();
    assert_eq!(summary.failed, 1, "{summary:?}");
    assert_eq!(summary.imported_events, 2);
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "utf8-conv");
    let ids = event_ids_by_message(&store, session);
    assert_eq!(
        ids.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["msg-before".to_owned(), "msg-after".to_owned()])
    );
}

#[test]
fn native_shelley_direct_delete_and_restore_reauthorizes_stable_events() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let backup = temp.path().join("shelley.backup");
    let options = shelley_options("shelley-route-machine", fixture.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let event_ids = event_ids_by_message(&store, session);
    let routed = event_ids["msg-user"];
    assert!(store.authorized_source_route_for_event(routed).is_ok());

    fs::rename(&fixture, &backup).unwrap();
    let deleted = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(deleted.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(routed).is_err());

    fs::rename(&backup, &fixture).unwrap();
    let restored = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(restored.failed, 0, "{restored:?}");
    assert_eq!(event_ids_by_message(&store, session), event_ids);
    assert!(store.authorized_source_route_for_event(routed).is_ok());
}

#[test]
fn native_shelley_released_cursor_migrates_once_then_is_terminal_noop() {
    let temp = tempdir();
    let database = temp.path().join("work.sqlite");
    let fixture = write_shelley_smoke_db(&temp);
    let machine_id = "shelley-released-cursor-machine";
    let imported_at = "2026-06-24T12:20:00Z".parse().unwrap();
    let options = ShelleySqliteImportOptions {
        machine_id: machine_id.to_owned(),
        source_path: Some(fixture.clone()),
        imported_at,
        ..ShelleySqliteImportOptions::default()
    };
    let mut store = Store::open(&database).unwrap();
    let canonical_path = fs::canonicalize(&fixture).unwrap();
    let path_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let released = CertifiedProviderCursor::new(
        "released-shelley-source",
        9,
        5,
        NativePosition::new("shelley-native-message-keyset-v9", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: machine_id.to_owned(),
            stream: stream.clone(),
            cursor: released,
            last_synced_at: Some(imported_at),
            timestamps: EntityTimestamps {
                created_at: imported_at,
                updated_at: imported_at,
            },
        })
        .unwrap();

    let migrated = import_shelley_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(migrated.failed, 0, "{migrated:?}");
    assert_eq!(migrated.imported_events, 3);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
    let migrated_cursor = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .expect("Shelley NativePath cursor exists after migration");
    let committed = decode_native_path_committed_cursor(&migrated_cursor.cursor).unwrap();
    let authority: serde_json::Value = serde_json::from_str(committed.provider_cursor()).unwrap();
    assert_eq!(authority["provider"].as_str(), Some("shelley"));
    assert_eq!(authority["terminal"].as_bool(), Some(true));

    let terminal = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(terminal.failed, 0, "{terminal:?}");
    assert_eq!(terminal.imported_events, 0);
    assert_eq!(terminal.skipped_events, 0);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
    assert_eq!(
        store
            .get_sync_cursor(None, machine_id, &stream)
            .unwrap()
            .unwrap()
            .cursor,
        migrated_cursor.cursor
    );
}

#[test]
fn native_shelley_v1_alias_restart_reuses_released_source_identity() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let alias_parent = temp.path().join("alias");
    fs::create_dir(&alias_parent).unwrap();
    let alias = alias_parent.join("..").join("shelley.db");
    let machine_id = "shelley-v1-alias-machine";
    let options = shelley_options(machine_id, fixture.clone());

    let mut reference = Store::open(temp.path().join("reference.sqlite")).unwrap();
    import_shelley_sqlite(&fixture, &mut reference, options.clone()).unwrap();
    let reference_source = reference
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    let reference_session =
        stored_provider_session_id(&reference, CaptureProvider::Shelley, "shelley-root");
    let reference_event = reference
        .events_for_session(reference_session)
        .unwrap()
        .into_iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .unwrap();

    let legacy_path = alias.display().to_string();
    let legacy_source_identity = provider_source_identity(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        Some(&legacy_path),
        Some(&legacy_path),
        None,
        &serde_json::Value::Null,
    )
    .unwrap();
    let legacy_source_id = provider_scoped_source_uuid(
        CaptureProvider::Shelley,
        "shelley-root",
        SHELLEY_SQLITE_SOURCE_FORMAT,
        Some(&legacy_path),
    );
    let legacy_session_id = provider_source_session_uuid(&legacy_source_identity, "shelley-root");
    let released_index = reference_event.payload["provider_event_index"]
        .as_u64()
        .unwrap();

    let mut legacy_source = reference_source;
    legacy_source.id = legacy_source_id;
    legacy_source.descriptor.raw_source_path = Some(legacy_path.clone());
    legacy_source.descriptor.source_root = Some(legacy_path);
    legacy_source.descriptor.source_identity = Some(legacy_source_identity.clone());
    let mut legacy_session = reference.get_session(reference_session).unwrap();
    legacy_session.id = legacy_session_id;
    legacy_session.capture_source_id = Some(legacy_source_id);
    let mut legacy_event = reference_event;
    legacy_event.id = provider_source_event_uuid(legacy_source_id, released_index);
    legacy_event.seq = provider_source_event_seq(legacy_source_id, released_index);
    legacy_event.session_id = Some(legacy_session_id);
    legacy_event.capture_source_id = Some(legacy_source_id);
    legacy_event.run_id = None;
    legacy_event.payload["provider_event_hash"] = json!("msg-user");
    legacy_event.sync.metadata["provider_event_hash"] = json!("msg-user");
    legacy_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::ProviderSupplied.as_str());
    legacy_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        legacy_source_id,
        released_index,
        "msg-user",
    ));

    let canonical_path = fs::canonicalize(&fixture).unwrap();
    let path_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let mut released_cursor = reference
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let mut envelope: serde_json::Value = serde_json::from_str(&released_cursor.cursor).unwrap();
    let mut provider_cursor: serde_json::Value =
        serde_json::from_str(envelope["provider_cursor"].as_str().unwrap()).unwrap();
    provider_cursor["version"] = json!(1);
    provider_cursor["canonical_source_identity"] = json!(legacy_source_identity);
    provider_cursor["route_retired"] = json!(true);
    envelope["provider_cursor"] = json!(serde_json::to_string(&provider_cursor).unwrap());
    released_cursor.cursor = serde_json::to_string(&envelope).unwrap();

    let mut store = Store::open(temp.path().join("upgrade.sqlite")).unwrap();
    store.upsert_capture_source(&legacy_source).unwrap();
    store.upsert_session(&legacy_session).unwrap();
    assert!(store.insert_event_if_absent(&legacy_event).unwrap());
    store.upsert_sync_cursor(&released_cursor).unwrap();

    let migrated = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(migrated.failed, 0, "{migrated:?}");
    assert_eq!(
        store
            .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
            .unwrap()
            .unwrap()
            .id,
        legacy_source_id
    );
    assert_eq!(
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root"),
        legacy_session_id
    );
    assert!(store.get_event(legacy_event.id).is_ok());
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

#[test]
fn native_shelley_message_locator_round_trips_and_fails_after_row_mutation() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let complete_text = format!(
        "Shelley complete body\n{}",
        "locator-body-".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    Connection::open(&fixture)
        .unwrap()
        .execute(
            "update messages set user_data = ?1 where message_id = 'msg-user'",
            [json!({"Content": [{"Type": 2, "Text": complete_text.clone()}]}).to_string()],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_shelley_sqlite(
        &fixture,
        &mut store,
        shelley_options("shelley-content-machine", fixture.clone()),
    )
    .unwrap();
    let session = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let event = store
        .events_for_session(session)
        .unwrap()
        .into_iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .unwrap();
    let request = shelley_complete_request(&store, &event);
    let resolved = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(resolved[0].text, complete_text);

    Connection::open(&fixture)
        .unwrap()
        .execute(
            "update messages set user_data = ?1 where message_id = 'msg-user'",
            [json!({"Content": [{"Type": 2, "Text": "mutated source row"}]}).to_string()],
        )
        .unwrap();
    let mutated_request = shelley_complete_request(&store, &event);
    let error = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&mutated_request))
        .unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn native_shelley_v025_raw_path_source_and_positional_hash_migrate_in_place() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let machine_id = "shelley-v025-machine";
    let options = shelley_options(machine_id, fixture.clone());

    let mut reference = Store::open(temp.path().join("reference.sqlite")).unwrap();
    import_shelley_sqlite(&fixture, &mut reference, options.clone()).unwrap();
    let reference_source = reference
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    let reference_session =
        stored_provider_session_id(&reference, CaptureProvider::Shelley, "shelley-root");
    let reference_event = reference
        .events_for_session(reference_session)
        .unwrap()
        .into_iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .unwrap();

    let raw_path = fixture.display().to_string();
    let legacy_source_id = provider_scoped_source_uuid(
        CaptureProvider::Shelley,
        "shelley-root",
        SHELLEY_SQLITE_SOURCE_FORMAT,
        Some(&raw_path),
    );
    let legacy_source_identity = provider_source_identity(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        Some(&raw_path),
        Some(&raw_path),
        None,
        &serde_json::Value::Null,
    )
    .unwrap();
    let legacy_session_id = provider_source_session_uuid(&legacy_source_identity, "shelley-root");
    let released_index = reference_event.payload["provider_event_index"]
        .as_u64()
        .unwrap();
    let legacy_event_id = provider_source_event_uuid(legacy_source_id, released_index);

    let mut legacy_source = reference_source;
    legacy_source.id = legacy_source_id;
    legacy_source.descriptor.raw_source_path = Some(raw_path.clone());
    legacy_source.descriptor.source_root = Some(raw_path.clone());
    legacy_source.descriptor.source_identity = Some(legacy_source_identity);
    let mut legacy_session = reference.get_session(reference_session).unwrap();
    legacy_session.id = legacy_session_id;
    legacy_session.capture_source_id = Some(legacy_source_id);
    let mut legacy_event = reference_event;
    legacy_event.id = legacy_event_id;
    legacy_event.seq = provider_source_event_seq(legacy_source_id, released_index);
    legacy_event.session_id = Some(legacy_session_id);
    legacy_event.capture_source_id = Some(legacy_source_id);
    legacy_event.run_id = None;
    legacy_event.payload["provider_event_hash"] = json!("msg-user");
    legacy_event.sync.metadata["provider_event_hash"] = json!("msg-user");
    legacy_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::ProviderSupplied.as_str());
    legacy_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        legacy_source_id,
        released_index,
        "msg-user",
    ));

    let mut store = Store::open(temp.path().join("upgrade.sqlite")).unwrap();
    store.upsert_capture_source(&legacy_source).unwrap();
    store.upsert_session(&legacy_session).unwrap();
    assert!(store.insert_event_if_absent(&legacy_event).unwrap());

    let migrated = import_shelley_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(migrated.failed, 0, "{migrated:?}");
    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(source.id, legacy_source_id);
    assert_ne!(
        source.descriptor.source_identity,
        legacy_source.descriptor.source_identity
    );
    assert_eq!(
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root"),
        legacy_session_id
    );
    let event = store.get_event(legacy_event_id).unwrap();
    assert_eq!(event.id, legacy_event_id);
    assert_eq!(
        event.sync.metadata["provider_event_hash_authority"].as_str(),
        Some(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str())
    );
    assert_eq!(
        store
            .events_for_session(legacy_session_id)
            .unwrap()
            .iter()
            .filter(|event| {
                event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user")
            })
            .count(),
        1
    );
}

fn shelley_options(machine_id: &str, path: std::path::PathBuf) -> ShelleySqliteImportOptions {
    ShelleySqliteImportOptions {
        machine_id: machine_id.to_owned(),
        source_path: Some(path),
        imported_at: "2026-07-26T12:00:00Z".parse().unwrap(),
        ..ShelleySqliteImportOptions::default()
    }
}

fn event_ids_by_message(store: &Store, session_id: Uuid) -> BTreeMap<String, Uuid> {
    store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .map(|event| {
            (
                event.sync.metadata["metadata"]["message_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                event.id,
            )
        })
        .collect()
}

fn replace_with_collision_fixture(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "delete from messages;
         delete from conversations;
         insert into conversations (conversation_id, slug) values ('conv', 'collision');
         insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (
            'msg-32719', 'conv', 7, 'user',
            '{\"Content\":[{\"Type\":2,\"Text\":\"collision first\"}]}',
            '2026-07-26 12:00:00'
         );
         insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (
            'msg-150040', 'conv', 7, 'user',
            '{\"Content\":[{\"Type\":2,\"Text\":\"collision second\"}]}',
            '2026-07-26 12:00:01'
         );",
    )
    .unwrap();
}

fn collision_message(message_id: &str) -> ShelleyMessageRow {
    ShelleyMessageRow {
        rowid: 1,
        message_id: message_id.to_owned(),
        conversation_id: "conv".to_owned(),
        sequence_id: 7,
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
    }
}

fn replace_with_invalid_utf8_fixture(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "delete from messages;
         delete from conversations;
         insert into conversations (conversation_id, slug)
            values ('utf8-conv', 'invalid utf8');
         insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (
            'msg-before', 'utf8-conv', 1, 'user',
            '{\"Content\":[{\"Type\":2,\"Text\":\"before invalid\"}]}',
            '2026-07-26 12:00:00'
         );
         insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (
            'msg-invalid', 'utf8-conv', 2, 'user', cast(x'80' as text),
            '2026-07-26 12:00:01'
         );
         insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
         ) values (
            'msg-after', 'utf8-conv', 3, 'user',
            '{\"Content\":[{\"Type\":2,\"Text\":\"after invalid\"}]}',
            '2026-07-26 12:00:02'
         );",
    )
    .unwrap();
}

fn shelley_complete_request(
    store: &Store,
    event: &ctx_history_core::Event,
) -> CompleteMessageRequest {
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    let source_locator = locator.source_locator().unwrap();
    let route = store.authorized_source_route_for_event(event.id).unwrap();
    let source_access = SourceAccessBroker::new()
        .admit_for_source_locators(
            AuthorizedSourceRoute {
                source_id: route.capture_source_id(),
                provider: route.provider(),
                source_format: route.source_format().to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: route.path().to_path_buf(),
                source_root: route.path().parent().map(std::path::Path::to_path_buf),
                source_identity: Some(route.canonical_source_identity().to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            std::slice::from_ref(&source_locator),
            event.id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Shelley,
        source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: locator.content_profile().to_owned(),
        source_locator: Some(source_locator),
        provider_session_id: event.sync.metadata["provider_session_id"]
            .as_str()
            .map(str::to_owned),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: event.payload["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(locator.native_record_id().to_owned()),
        expected_record_digest: Some(locator.record_sha256().clone()),
        expected_content_ref: Some(locator.content_ref().clone()),
        indexed_text: event.payload["body"]["text"].as_str().unwrap().to_owned(),
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
}
