use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus,
    SyncCursor,
};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    complete_content::sqlite::SqliteCompleteContentResolver,
    complete_content::{
        verified_content_profile, AuthorizedSourceRoute, CompleteContentHashAuthority,
        CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
        SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    import_lingma_sqlite,
    native_source::NativePosition,
    provider::importer::{
        provider_path_identity, provider_scoped_source_uuid,
        provider_source_cursor_stream_for_path, provider_source_event_seq,
        provider_source_event_uuid, provider_source_identity, provider_source_session_uuid,
        provider_sync_metadata, timestamps, BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    CaptureWorkLimit, ImportProfile, LingmaSqliteImportOptions, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportWorkResult, LINGMA_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS,
};

const MACHINE: &str = "lingma-nativepath-test-machine";

fn create_db(path: &std::path::Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (
                session_id text not null,
                request_id text,
                chat_prompt text,
                summary text,
                error_result text,
                gmt_create integer,
                extra text
             );",
        )
        .unwrap();
    connection
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    connection: &Connection,
    session_id: &str,
    request_id: &str,
    prompt: &str,
    summary: Option<&str>,
    error: Option<&str>,
    timestamp: i64,
    extra: Option<&str>,
) {
    connection
        .execute(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![session_id, request_id, prompt, summary, error, timestamp, extra],
        )
        .unwrap();
}

fn options(profile: ImportProfile) -> LingmaSqliteImportOptions {
    LingmaSqliteImportOptions {
        machine_id: MACHINE.to_owned(),
        import_profile: profile,
        ..LingmaSqliteImportOptions::default()
    }
}

fn lingma_events(store: &Store) -> Vec<ctx_history_core::Event> {
    let mut events = Vec::new();
    for session in store
        .list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::Lingma)
    {
        events.extend(store.events_for_session(session.id).unwrap());
    }
    events.sort_by_key(|event| event.seq);
    events
}

fn complete_request(store: &Store, event: &Event) -> CompleteMessageRequest {
    let persisted = event
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .and_then(VerifiedContentLocatorsV1::from_metadata_value)
        .and_then(|locators| locators.locator(VerifiedContentRole::MessageBody).cloned())
        .unwrap();
    let route = store.authorized_source_route_for_event(event.id).unwrap();
    let source = store.get_capture_source(route.capture_source_id()).unwrap();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: route.capture_source_id(),
                provider: route.provider(),
                source_format: route.source_format().to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: route.path().to_path_buf(),
                source_root: source.descriptor.source_root.map(PathBuf::from),
                source_identity: Some(route.canonical_source_identity().to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event.id,
        )
        .unwrap();
    let indexed_text = event
        .payload
        .pointer("/body/text")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_owned();
    let provider_session_id = event
        .session_id
        .and_then(|session_id| store.get_session(session_id).ok())
        .and_then(|session| session.external_session_id);
    CompleteMessageRequest {
        event_id: event.id,
        provider: route.provider(),
        source_format: route.source_format().to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator(),
        provider_session_id,
        source_record_ordinal: event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: u32::try_from(
            event.sync.metadata["source_record_subrecord_index"]
                .as_u64()
                .unwrap(),
        )
        .unwrap(),
        expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(persisted.native_record_id().to_owned()),
        expected_record_digest: Some(persisted.record_sha256().clone()),
        expected_content_ref: Some(persisted.content_ref().clone()),
        indexed_text,
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
}

fn hydrate(store: &Store, event: &Event) -> String {
    SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&complete_request(store, event)))
        .unwrap()[0]
        .text
        .clone()
}

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for component in &from[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

#[test]
fn nativepath_lifecycle_is_idempotent_and_excludes_unclassified_extra_from_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session-a",
        "request-a",
        "first prompt",
        Some("first assistant summary"),
        None,
        1_700_000_000,
        Some("CTX_LINGMA_PRIVATE_OUTPUT_BODY"),
    );
    drop(connection);
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_events, 2);
    let original = lingma_events(&store);
    assert_eq!(original.len(), 2);
    assert!(original.iter().all(|event| {
        !serde_json::to_string(&(event.payload.clone(), event.sync.metadata.clone()))
            .unwrap()
            .contains("CTX_LINGMA_PRIVATE_OUTPUT_BODY")
    }));
    let routed_event = original[0].id;
    store
        .authorized_source_route_for_event(routed_event)
        .unwrap();

    let noop = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );

    let connection = Connection::open(&db).unwrap();
    insert_row(
        &connection,
        "session-a",
        "request-b",
        "appended prompt",
        Some("appended summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    let append = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(lingma_events(&store).len(), 4);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "update chat_record set chat_prompt = 'rewritten prompt' where rowid = 1",
            [],
        )
        .unwrap();
    drop(connection);
    let rewrite = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert!(serde_json::to_string(&lingma_events(&store))
        .unwrap()
        .contains("rewritten prompt"));

    let connection = Connection::open(&db).unwrap();
    connection
        .execute("delete from chat_record where rowid = 2", [])
        .unwrap();
    drop(connection);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = temp.path().join("replacement.db");
    let replacement_connection = create_db(&replacement);
    insert_row(
        &replacement_connection,
        "session-replacement",
        "request-replacement",
        "replacement prompt",
        Some("replacement summary"),
        None,
        1_700_000_100,
        None,
    );
    drop(replacement_connection);
    std::fs::rename(&replacement, &db).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(serde_json::to_string(&lingma_events(&store))
        .unwrap()
        .contains("replacement prompt"));

    std::fs::remove_file(&db).unwrap();
    let retired = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn store_published_long_prompt_hydrates_with_normalized_payload_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let prompt = format!(
        "published Lingma prompt {}",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let connection = create_db(&db);
    insert_row(
        &connection,
        "published-session",
        "published-request",
        &prompt,
        Some("summary only"),
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    let events = lingma_events(&store);
    let user = events
        .iter()
        .find(|event| event.role == Some(ctx_history_core::EventRole::User))
        .unwrap();
    let assistant = events
        .iter()
        .find(|event| event.role == Some(ctx_history_core::EventRole::Assistant))
        .unwrap();
    let expected_hash = compute_payload_hash(&user.payload["body"]).unwrap();
    assert_eq!(
        user.sync.metadata["provider_event_hash"],
        serde_json::json!(expected_hash)
    );
    assert_eq!(
        user.sync.metadata["provider_event_hash_authority"],
        serde_json::json!("normalized_payload_fallback")
    );
    assert!(user
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_some());
    assert!(assistant
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
    assert_eq!(hydrate(&store, user), prompt);
}

#[test]
fn released_positional_event_upgrades_in_place_and_old_locator_still_resolves() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let prompt = format!(
        "released Lingma prompt {}",
        "u".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let connection = create_db(&db);
    insert_row(
        &connection,
        "released-session",
        "  released-request  ",
        &prompt,
        None,
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let canonical_path = std::fs::canonicalize(&db).unwrap();
    let raw_source_path = canonical_path.display().to_string();
    let imported_at = DateTime::<Utc>::UNIX_EPOCH;
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let values = super::lingma_complete_values(
        &Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap(),
        1,
    )
    .unwrap()
    .unwrap();
    let (complete_event, complete_text) = super::lingma_complete_user_message(&values).unwrap();
    let released_hash = complete_event.released_provider_event_hash.clone();
    assert_eq!(
        released_hash,
        "released-session:released-request:user".to_owned()
    );
    let locator = super::native_path::lingma_locator(1).unwrap();
    let profile = verified_content_profile(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .unwrap();
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        ContentRef::from_bytes(prompt.as_bytes()).unwrap(),
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        released_hash.clone(),
        super::native_path::lingma_logical_record_digest(&values).unwrap(),
    )
    .unwrap();
    let released_locators = VerifiedContentLocatorsV1::singleton(persisted.clone()).unwrap();
    let canonical_source_identity = provider_source_identity(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        Some(&raw_source_path),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .unwrap();
    let released_source_id = provider_scoped_source_uuid(
        CaptureProvider::Lingma,
        "released-session",
        LINGMA_SQLITE_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    store
        .upsert_capture_source(&CaptureSource {
            id: released_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Lingma,
                machine_id: MACHINE.to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(raw_source_path.clone()),
                source_format: Some(LINGMA_SQLITE_SOURCE_FORMAT.to_owned()),
                source_root: Some(raw_source_path.clone()),
                source_identity: Some(canonical_source_identity.clone()),
                external_session_id: None,
            },
            started_at: imported_at,
            ended_at: None,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "source_identity": canonical_source_identity,
                    "source_root": raw_source_path,
                }),
            ),
        })
        .unwrap();
    let released_session_id =
        provider_source_session_uuid(&canonical_source_identity, "released-session");
    store
        .upsert_session(&Session {
            id: released_session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(released_source_id),
            provider: CaptureProvider::Lingma,
            external_session_id: Some("released-session".to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: imported_at,
            ended_at: Some(imported_at),
            timestamps: timestamps(imported_at),
            sync: provider_sync_metadata(Fidelity::Partial, json!({})),
        })
        .unwrap();
    let released_event_id = provider_source_event_uuid(released_source_id, 0);
    let released_seq = provider_source_event_seq(released_source_id, 0);
    let released_dedupe =
        Store::provider_source_event_dedupe_key(released_source_id, 0, &released_hash);
    store
        .upsert_event(&Event {
            id: released_event_id,
            seq: released_seq,
            history_record_id: None,
            session_id: Some(released_session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: imported_at,
            capture_source_id: Some(released_source_id),
            payload: json!({
                "provider": CaptureProvider::Lingma.as_str(),
                "provider_session_id": "released-session",
                "provider_event_index": 0,
                "provider_event_hash": released_hash,
                "cursor": complete_event.cursor,
                "artifacts": [],
                "body": complete_event.payload,
            }),
            payload_blob_id: None,
            dedupe_key: Some(released_dedupe),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": "released-session",
                    "provider_event_index": 0,
                    "provider_event_hash": released_hash,
                    "provider_event_hash_authority": "provider_supplied",
                    "cursor": complete_event.cursor,
                    "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "fixture_line": 1,
                    "imported_at": imported_at,
                    "source_record_ordinal": 1,
                    "source_record_subrecord_index": 0,
                    VERIFIED_CONTENT_LOCATORS_METADATA_KEY: released_locators.to_metadata_value(),
                    "metadata": {},
                }),
            ),
        })
        .unwrap();
    let locator_identity = provider_path_identity(&canonical_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let released_cursor = CertifiedProviderCursor::new(
        "released-lingma-source",
        5,
        6,
        NativePosition::new("lingma-chat-record-rowid-v5", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: MACHINE.to_owned(),
            stream: cursor_stream,
            cursor: released_cursor,
            last_synced_at: None,
            timestamps: timestamps(imported_at),
        })
        .unwrap();

    let upgraded = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(upgraded.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(upgraded.imported_events, 0);
    let migrated = store.get_event(released_event_id).unwrap();
    assert_eq!(migrated.id, released_event_id);
    assert_eq!(migrated.seq, released_seq);
    let normalized_hash = compute_payload_hash(&migrated.payload["body"]).unwrap();
    assert_ne!(normalized_hash, released_hash);
    assert!(migrated
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(&normalized_hash));
    assert_eq!(
        migrated.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_eq!(hydrate(&store, &migrated), complete_text);

    let mut old_request = complete_request(&store, &migrated);
    old_request.source_locator = persisted.source_locator();
    old_request.expected_provider_event_hash = released_hash.clone();
    old_request.expected_hash_authority = CompleteContentHashAuthority::ProviderSupplied;
    old_request.expected_native_record_id = Some(released_hash);
    old_request.expected_record_digest = Some(persisted.record_sha256().clone());
    old_request.expected_content_ref = Some(persisted.content_ref().clone());
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(std::slice::from_ref(&old_request))
            .unwrap()[0]
            .text,
        prompt
    );

    let connection = Connection::open(&db).unwrap();
    insert_row(
        &connection,
        "released-session",
        "append-request",
        "appended after released upgrade",
        None,
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(
        store.get_event(released_event_id).unwrap().id,
        released_event_id
    );
    assert_eq!(lingma_events(&store).len(), 2);
}

#[test]
fn relative_database_path_persists_absolute_authority_and_hydrates_after_reopen() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("relative-local.db");
    let prompt = format!(
        "relative Lingma prompt {}",
        "r".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let connection = create_db(&db);
    insert_row(
        &connection,
        "relative-session",
        "relative-request",
        &prompt,
        None,
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let relative_db = relative_path(&std::env::current_dir().unwrap(), &db);
    assert!(relative_db.is_relative());
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    import_lingma_sqlite(&relative_db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    let event_id = lingma_events(&store)[0].id;
    let route = store.authorized_source_route_for_event(event_id).unwrap();
    assert!(route.path().is_absolute());
    assert_eq!(route.path(), std::fs::canonicalize(&db).unwrap());
    let source = store.get_capture_source(route.capture_source_id()).unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(route.path().to_str().unwrap())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(route.path().to_str().unwrap())
    );
    assert_eq!(
        source.sync.metadata["display_source_path"],
        serde_json::json!(relative_db.display().to_string())
    );

    drop(store);
    let store = Store::open(&store_path).unwrap();
    let event = store.get_event(event_id).unwrap();
    assert_eq!(hydrate(&store, &event), prompt);
}

#[test]
fn byte_identical_restore_reactivates_route_and_complete_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let missing = temp.path().join("local.db.missing");
    let prompt = format!(
        "restored Lingma prompt {}",
        "z".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    );
    let connection = create_db(&db);
    insert_row(
        &connection,
        "restore-session",
        "restore-request",
        &prompt,
        None,
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    let event_id = lingma_events(&store)[0].id;

    std::fs::rename(&db, &missing).unwrap();
    let retired = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(event_id).is_err());

    std::fs::rename(&missing, &db).unwrap();
    let restored = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(restored.work_result(), ProviderImportWorkResult::Changed);
    let event = store.get_event(event_id).unwrap();
    assert!(store.authorized_source_route_for_event(event_id).is_ok());
    assert_eq!(hydrate(&store, &event), prompt);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn one_safe_group_resumes_without_replaying_the_committed_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    for index in 0..70 {
        insert_row(
            &connection,
            "session",
            &format!("request-{index}"),
            &format!("prompt-{index}"),
            Some(&format!("summary-{index}")),
            None,
            1_700_000_000 + index,
            None,
        );
    }
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut first_options = options(ImportProfile::CoreOnly);
    first_options.capture_work_limit = CaptureWorkLimit::OneSafeGroup;
    let first = import_lingma_sqlite(&db, &mut store, first_options).unwrap();
    assert!(first.work_remaining);
    assert_eq!(lingma_events(&store).len(), 128);

    let replay = Arc::new(RecordingSink::default());
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert!(replay.pages.lock().unwrap().is_empty());

    let second = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert!(!second.work_remaining);
    assert_eq!(lingma_events(&store).len(), 140);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[derive(Default)]
struct RecordingSink {
    fail: bool,
    pages: Mutex<Vec<ProOutputMaterializationPage>>,
    progress: Mutex<Option<ProOutputProgress>>,
}

impl RecordingSink {
    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "test-materializer-v1"
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
        if self.fail {
            return Err(ProOutputSinkError::new("injected", "injected Pro failure"));
        }
        assert!(page.observations.is_empty());
        assert!(page.terminal);
        let result = ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor.clone(),
            accepted_outputs: 0,
            materialized_facts: 0,
            replayed: false,
        };
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(page.next_safe_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        self.pages.lock().unwrap().push(page);
        Ok(result)
    }
}

#[test]
fn pro_failure_never_blocks_core_and_later_activation_replays_independently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session",
        "request",
        "prompt",
        Some("summary"),
        None,
        1_700_000_000,
        Some("private unclassified result"),
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let mut empty_store = Store::open(temp.path().join("empty.sqlite")).unwrap();
    let empty_replay = Arc::new(RecordingSink::default());
    let empty_summary = import_lingma_sqlite(
        &db,
        &mut empty_store,
        options(ImportProfile::ProReplayOnly(empty_replay.clone())),
    )
    .unwrap();
    assert_eq!(empty_summary.work_result(), ProviderImportWorkResult::NoOp);
    assert!(empty_store.list_sessions().unwrap().is_empty());
    assert!(empty_replay.pages.lock().unwrap().is_empty());

    let failing = Arc::new(RecordingSink::failing());
    let summary =
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreAndPro(failing))).unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(lingma_events(&store).len(), 2);

    let replay = Arc::new(RecordingSink::default());
    let summary = import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    assert_eq!(lingma_events(&store).len(), 2);

    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
}

#[test]
fn pro_replay_waits_for_lingma_append_rewrite_and_replacement_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session",
        "initial",
        "initial prompt",
        Some("initial summary"),
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("core.sqlite")).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );
    let replay = Arc::new(RecordingSink::default());

    let connection = Connection::open(&db).unwrap();
    insert_row(
        &connection,
        "session",
        "append",
        "append prompt",
        Some("append summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert!(replay.pages.lock().unwrap().is_empty());
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "update chat_record set chat_prompt = 'rewrite prompt' where rowid = 1",
            [],
        )
        .unwrap();
    drop(connection);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 2);

    let replacement = temp.path().join("replacement.db");
    let replacement_connection = create_db(&replacement);
    insert_row(
        &replacement_connection,
        "replacement",
        "replacement",
        "replacement prompt",
        Some("replacement summary"),
        None,
        1_700_000_100,
        None,
    );
    drop(replacement_connection);
    std::fs::remove_file(&db).unwrap();
    std::fs::rename(&replacement, &db).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 2);
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 3);
}

#[test]
fn malformed_text_is_row_local_and_valid_siblings_commit() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    connection
        .execute_batch(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary, gmt_create
             ) values ('bad-session', 'bad-request', cast(x'80' as text), null, 1700000000);",
        )
        .unwrap();
    insert_row(
        &connection,
        "good-session",
        "good-request",
        "good prompt",
        Some("good summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(lingma_events(&store).len(), 2);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}
