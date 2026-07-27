use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use chrono::{DateTime, TimeZone, Utc};
use ctx_history_store::{ProviderEventHashAuthority, Store};
use serde_json::json;
use uuid::Uuid;

use super::{
    decode_current_cursor, import_opencode_nativepath, CertifiedProviderCursor,
    ProviderAdapterContext, ProviderImportWorkResult, SyncCursor,
};
use crate::native_source::NativePosition;
use crate::provider::importer::{
    provider_event_import_identity_with_exact_legacy_source, provider_path_identity,
    provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
};
use crate::provider::providers::opencode::native_path::tests::{
    create_family_database, insert_part_event, insert_row_event, insert_session,
};
use crate::provider::providers::opencode::native_path::{
    OpenCodeNativePageLimits, OpenCodeNativePathReader, OpenCodeNativeSchemaFamily,
    OpenCodeNativeSourceSelection,
};
use crate::provider::providers::opencode::{
    KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT, OPENCODE_SQLITE_DIALECT,
};
use crate::{
    CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderImportOptions,
};

fn context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "opencode-nativepath-test".to_owned(),
        source_path: None,
        source_root: None,
        imported_at: Utc.timestamp_millis_opt(1_785_024_000_000).unwrap(),
    }
}

#[test]
fn opencode_nativepath_vertical_core_is_restart_safe_and_appends() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1_785_024_000_000);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "user",
        1,
        1_785_024_000_001,
        r#"{"role":"user","text":"first"}"#,
    );
    drop(conn);

    let store_path = temp.path().join("store.db");
    let mut store = Store::open(&store_path).unwrap();
    let first = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let events = store.events_for_session(sessions[0].id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["text"], "first");

    let second = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(second.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 1);
    drop(store);

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-b",
        "session-a",
        "assistant",
        2,
        1_785_024_000_002,
        r#"{"role":"assistant","text":"second"}"#,
    );
    drop(conn);
    let mut store = Store::open(&store_path).unwrap();
    let appended = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 2);

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    conn.execute(
        "update session_message
             set data = '{\"role\":\"user\",\"text\":\"rewritten\"}'
             where id = 'message-a'",
        [],
    )
    .unwrap();
    drop(conn);
    let rewritten = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(rewritten.imported_events, 0);
    assert_eq!(rewritten.skipped_events, 2);
    assert_eq!(rewritten.accepted_content_records, 2);
    let events = store.events_for_session(sessions[0].id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .any(|event| event.payload["text"] == "rewritten"));
    assert!(!events.iter().any(|event| event.payload["text"] == "first"));

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-c",
        "session-a",
        "assistant",
        3,
        1_785_024_000_003,
        r#"{"role":"assistant","text":"after rewrite"}"#,
    );
    drop(conn);
    let after_rewrite = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(
        after_rewrite.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(after_rewrite.imported_events, 1);
    assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 3);
}

#[test]
fn opencode_nativepath_new_session_does_not_renumber_existing_event_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 20);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "user",
        1,
        21,
        r#"{"role":"user","text":"stable"}"#,
    );
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let original_session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("session-a"))
        .unwrap();
    let original = store
        .events_for_session(original_session.id)
        .unwrap()
        .pop()
        .unwrap();
    assert_ne!(
        original.sync.metadata["source_record_ordinal"],
        original.sync.metadata["metadata"]["legacy_provider_event_index"]
    );

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    insert_session(&conn, "session-new", None, 30);
    drop(conn);
    let appended = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(appended.imported_sessions, 1);
    let retained = store
        .events_for_session(original_session.id)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(retained.id, original.id);
    assert_eq!(retained.seq, original.seq);
    assert_eq!(retained.dedupe_key, original.dedupe_key);
    assert_eq!(
        retained.sync.metadata["source_record_ordinal"],
        original.sync.metadata["source_record_ordinal"]
    );
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}

#[test]
fn opencode_nativepath_same_native_id_rewrite_uses_stable_index_and_normalized_hash() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "user",
        1,
        2,
        r#"{"role":"user","text":"before"}"#,
    );
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let first = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .find(|event| event.payload["text"] == "before")
        .unwrap();
    let stable_index = first.sync.metadata["metadata"]["stable_provider_event_index"]
        .as_u64()
        .unwrap();

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    conn.execute(
        "update session_message
             set data = '{\"role\":\"user\",\"text\":\"after\"}', time_updated = 5
             where id = 'message-a'",
        [],
    )
    .unwrap();
    drop(conn);
    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let rewritten = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .find(|event| event.payload["text"] == "after")
        .unwrap();
    assert_eq!(
        rewritten.sync.metadata["metadata"]["stable_provider_event_index"],
        json!(stable_index)
    );
    assert_eq!(
        rewritten.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_ne!(
        rewritten.sync.metadata["provider_event_hash"],
        json!(rewritten.sync.metadata["native_record_id"])
    );
}

#[test]
fn opencode_nativepath_core_result_diagnostics_never_store_body_or_preview() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::MessagePart);
    insert_session(&conn, "session-a", None, 1);
    for (index, (status, output)) in [
        ("completed", "success-secret"),
        ("failed", "failure-secret"),
        ("timeout", "timeout-secret"),
        ("future", "unknown-secret"),
    ]
    .into_iter()
    .enumerate()
    {
        insert_part_event(
            &conn,
            &format!("message-{index}"),
            &format!("part-{index}"),
            "session-a",
            "assistant",
            "tool_result",
            2 + index as i64,
            &json!({
                "type": "tool_result",
                "state": {"status": status, "output": output}
            })
            .to_string(),
        );
    }
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let events = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    for event in events {
        assert!(matches!(
            event.event_type,
            ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
        ));
        assert!(event.payload.get("body").is_none());
        assert!(event.payload.get("output_preview").is_none());
        let encoded = event.payload.to_string();
        assert!(!encoded.contains("failure-secret"));
        assert!(!encoded.contains("timeout-secret"));
    }
}

#[test]
fn opencode_nativepath_vertical_retires_a_deleted_exact_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1_785_024_000_000);
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    let options = ProviderImportOptions {
        inventory_observation_token: Some("exact-root-scan-1".to_owned()),
        ..ProviderImportOptions::default()
    };
    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        options.clone(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    std::fs::remove_file(&source_path).unwrap();

    let retired = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        options.clone(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    let repeated = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        options,
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn opencode_nativepath_one_safe_group_restart_classifies_mutation_against_pending_state() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    for index in 0..65 {
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            &format!("message-{index:03}"),
            "session-a",
            "user",
            index,
            10 + index,
            &json!({"role": "user", "text": format!("before-{index}")}).to_string(),
        );
    }
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    let first = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..ProviderImportOptions::default()
        },
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert!(first.work_remaining);
    let path_identity =
        provider_path_identity(&std::fs::canonicalize(&source_path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        OPENCODE_SQLITE_DIALECT.provider,
        OPENCODE_SQLITE_DIALECT.source_format,
        &path_identity,
    );
    let pending = store
        .get_sync_cursor(None, &context().machine_id, &stream)
        .unwrap()
        .unwrap();
    let pending = decode_current_cursor(&pending.cursor).unwrap();
    assert!(pending.completed_state.is_none());

    let conn = rusqlite::Connection::open(&source_path).unwrap();
    conn.execute(
        "update session_message
             set data = '{\"role\":\"user\",\"text\":\"mutated-during-restart\"}',
                 time_updated = 999
             where id = 'message-000'",
        [],
    )
    .unwrap();
    drop(conn);
    let resumed = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert!(!resumed.work_remaining);
    let completed = store
        .get_sync_cursor(None, &context().machine_id, &stream)
        .unwrap()
        .unwrap();
    let completed = decode_current_cursor(&completed.cursor).unwrap();
    assert!(completed.completed_state.is_some());
    assert_ne!(completed.locator_identity, pending.locator_identity);
    assert!(store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .any(|event| event.payload["text"] == "mutated-during-restart"));
}

#[test]
fn opencode_nativepath_empty_database_publishes_terminal_core_and_zero_observation_pro_group() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("empty.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    drop(conn);
    let store_path = temp.path().join("store.db");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path));
    let summary = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(!summary.work_remaining);
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    assert!(sink
        .progress
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|progress| progress.terminal));
}

#[test]
fn mimocode_nativepath_proof_diagnostics_use_the_mimo_family_dialect() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("mimo.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "user",
        1,
        2,
        r#"{"role":"user","text":"bad time","time":{"created":"not-millis"}}"#,
    );
    drop(conn);
    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    let summary = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &MIMOCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("MiMo Code event time.created"));
    assert!(!summary.failures[0]
        .error
        .contains("OpenCode event time.created"));
}

#[test]
fn opencode_family_nativepath_keeps_provider_and_source_format_identities_separate() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("shared-family-schema.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "shared-session", None, 1);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "shared-message",
        "shared-session",
        "user",
        1,
        2,
        r#"{"role":"user","text":"one native schema; three providers"}"#,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    for dialect in [
        &OPENCODE_SQLITE_DIALECT,
        &KILO_SQLITE_DIALECT,
        &MIMOCODE_SQLITE_DIALECT,
    ] {
        let summary = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            dialect,
        )
        .unwrap();
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 1);
    }

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 3);
    for dialect in [
        &OPENCODE_SQLITE_DIALECT,
        &KILO_SQLITE_DIALECT,
        &MIMOCODE_SQLITE_DIALECT,
    ] {
        let session = sessions
            .iter()
            .find(|session| session.provider == dialect.provider)
            .unwrap();
        let event = store.events_for_session(session.id).unwrap().pop().unwrap();
        assert_eq!(event.sync.metadata["source_format"], dialect.source_format);
        assert_eq!(event.sync.metadata["provider_session_id"], "shared-session");
    }
}

#[test]
fn opencode_nativepath_exactly_migrates_released_event_only_ordinal_and_hash() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "user",
        1,
        2,
        r#"{"role":"user","text":"released"}"#,
    );
    drop(conn);

    let staging_path = temp.path().join("staging.db");
    let mut staging = Store::open(&staging_path).unwrap();
    import_opencode_nativepath(
        &source_path,
        &mut staging,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let session = staging.list_sessions().unwrap().pop().unwrap();
    let source_id = session.capture_source_id.unwrap();
    let source = staging.get_capture_source(source_id).unwrap();
    let mut released = staging
        .events_for_session(session.id)
        .unwrap()
        .pop()
        .unwrap();
    let legacy_index = released.sync.metadata["metadata"]["legacy_provider_event_index"]
        .as_u64()
        .unwrap();
    assert_eq!(legacy_index, 0);
    let legacy_native_record_id = released.sync.metadata["native_record_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&source_path))
            .unwrap();
    let mut scanner = reader.scanner(OpenCodeNativePageLimits::default()).unwrap();
    let legacy_provider_event_hash = loop {
        let page = scanner.next_page().unwrap().unwrap();
        if let Some(event) = page.events.first() {
            break event.content_digest.clone();
        }
    };

    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    store.upsert_capture_source(&source).unwrap();
    store.upsert_session(&session).unwrap();
    let legacy_identity = provider_event_import_identity_with_exact_legacy_source(
        &store,
        OPENCODE_SQLITE_DIALECT.provider,
        "session-a",
        source_id,
        legacy_index,
        legacy_index,
        &legacy_native_record_id,
        None,
        Some(legacy_index),
        true,
    )
    .unwrap();
    released.id = legacy_identity.id;
    released.seq = legacy_identity.seq;
    released.dedupe_key = Some(
        Store::provider_event_dedupe_key_with_payload_hash(
            &legacy_identity.dedupe_key,
            &legacy_provider_event_hash,
        )
        .unwrap(),
    );
    released.sync.metadata["provider_event_index"] = json!(legacy_index);
    released.sync.metadata["provider_event_hash"] = json!(legacy_provider_event_hash);
    released.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::ProviderSupplied.as_str());
    store.upsert_event(&released).unwrap();
    let released_id = released.id;

    let path_identity =
        provider_path_identity(&std::fs::canonicalize(&source_path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        OPENCODE_SQLITE_DIALECT.provider,
        OPENCODE_SQLITE_DIALECT.source_format,
        &path_identity,
    );
    let released_cursor = CertifiedProviderCursor::new(
        "released-opencode-source-revision",
        1,
        1,
        NativePosition::new("released-opencode-position-v1", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context().machine_id,
            stream,
            cursor: released_cursor,
            last_synced_at: None,
            timestamps: crate::provider::importer::timestamps(DateTime::<Utc>::UNIX_EPOCH),
        })
        .unwrap();

    import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        ProviderImportOptions::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let migrated = store.get_event(released_id).unwrap();
    assert_eq!(migrated.id, released_id);
    assert_eq!(
        migrated.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_ne!(
        migrated.sync.metadata["provider_event_hash"],
        json!(legacy_provider_event_hash)
    );
    assert!(migrated.dedupe_key.as_deref().unwrap().ends_with(
        migrated.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
    ));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn opencode_nativepath_vertical_commits_core_before_independent_output_replay() {
    const OUTPUT_SENTINEL: &str = "OPENCODE_NATIVEPATH_OUTPUT_SENTINEL";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let store_path = temp.path().join("store.db");
    let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::MessagePart);
    insert_session(&conn, "session-a", None, 1_785_024_000_000);
    insert_part_event(
        &conn,
        "message-a",
        "part-a",
        "session-a",
        "assistant",
        "text",
        1_785_024_000_001,
        r#"{"type":"text","text":"safe core text"}"#,
    );
    insert_part_event(
        &conn,
        "message-output",
        "part-output",
        "session-a",
        "assistant",
        "tool_result",
        1_785_024_000_002,
        &serde_json::json!({
            "type": "tool_result",
            "state": {"status": "completed", "output": OUTPUT_SENTINEL}
        })
        .to_string(),
    );
    drop(conn);

    let mut store = Store::open(&store_path).unwrap();
    let failing = Arc::new(FailingSink::default());
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(failing.clone()),
        ..ProviderImportOptions::default()
    };
    let summary = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        options,
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(failing.behind.load(Ordering::SeqCst));
    let session = store.list_sessions().unwrap().pop().unwrap();
    let core_debug = format!("{:?}", store.events_for_session(session.id).unwrap());
    assert!(!core_debug.contains(OUTPUT_SENTINEL));

    let sink = Arc::new(RecordingSink::new(store_path));
    let replay_options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(sink.clone()),
        ..ProviderImportOptions::default()
    };
    let replay = import_opencode_nativepath(
        &source_path,
        &mut store,
        context(),
        replay_options,
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [OUTPUT_SENTINEL.as_bytes()]
    );
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

struct RecordingSink {
    store_path: std::path::PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: std::path::PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            contents: Mutex::new(Vec::new()),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_committed_core: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "opencode-nativepath-test-materializer-v1"
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
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        self.contents.lock().unwrap().extend(
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
}

#[derive(Default)]
struct FailingSink {
    behind: AtomicBool,
}

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "opencode-nativepath-failing-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Err(ProOutputSinkError::new("test_failure", "expected failure"))
    }

    fn materialize_page(
        &self,
        _page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        unreachable!("observe_source fails before materialization")
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}
