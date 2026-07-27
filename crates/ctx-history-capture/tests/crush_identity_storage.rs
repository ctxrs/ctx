use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_capture::{
    complete_content::{
        sqlite::SqliteCompleteContentResolver, AuthorizedSourceRoute, CompleteContentHashAuthority,
        CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
        SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    import_crush_sqlite, CaptureWorkLimit, CrushSqliteImportOptions, ProviderImportWorkResult,
};
use ctx_history_core::{CaptureProvider, Event};
use ctx_history_store::Store;
use rusqlite::Connection;

const CRUSH_SOURCE_FORMAT: &str = "crush_sqlite";

fn options(source_path: &Path, capture_work_limit: CaptureWorkLimit) -> CrushSqliteImportOptions {
    CrushSqliteImportOptions {
        machine_id: "crush-identity-storage-tests".to_owned(),
        source_path: Some(source_path.to_path_buf()),
        imported_at: "2026-07-26T12:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        capture_work_limit,
        ..CrushSqliteImportOptions::default()
    }
}

fn session_events(store: &Store, external_id: &str) -> Vec<Event> {
    let session = store
        .session_by_external_session(CaptureProvider::Crush, external_id)
        .unwrap()
        .unwrap();
    store.events_for_session(session.id).unwrap()
}

fn complete_message_request(
    source_path: &Path,
    event: &Event,
    locator: &ctx_history_capture::complete_content::VerifiedContentLocatorV1,
) -> CompleteMessageRequest {
    let source_id = event.capture_source_id.unwrap();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id,
                provider: CaptureProvider::Crush,
                source_format: CRUSH_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: source_path.to_path_buf(),
                source_root: source_path.parent().map(Path::to_path_buf),
                source_identity: Some("crush-identity-storage-test".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event.id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Crush,
        source_format: CRUSH_SOURCE_FORMAT.to_owned(),
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
        expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
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
        indexed_limit_chars: 16_000,
    }
}

#[test]
fn integer_text_identity_collisions_reject_locally_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("crush.db");
    let conn = Connection::open(&source_path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id primary key,
            parent_session_id,
            title,
            prompt_tokens,
            completion_tokens,
            cost,
            created_at,
            updated_at,
            summary_message_id
        );
        create table messages (
            id primary key,
            session_id,
            role,
            parts,
            created_at,
            updated_at,
            provider,
            model,
            is_summary_message
        );
        create table files (
            session_id,
            path,
            version,
            created_at,
            updated_at
        );
        create table read_files (
            session_id,
            path,
            read_at
        );
        insert into sessions values (
            1, null, 'invalid integer identity', 1, 1, 0.0, 1000, 1000, null
        );
        insert into sessions values (
            '1', null, 'valid text identity', 1, 1, 0.0, 1001, 1001, null
        );
        insert into messages values (
            1, '1', 'assistant',
            '[{\"type\":\"text\",\"data\":{\"text\":\"invalid integer message identity\"}}]',
            2000, 2000, 'test', 'model', 0
        );
        insert into messages values (
            'bad-relation', 1, 'assistant',
            '[{\"type\":\"text\",\"data\":{\"text\":\"invalid integer relation\"}}]',
            2001, 2001, 'test', 'model', 0
        );
        insert into files values (1, 'invalid-file.txt', 'v1', 3000, 3000);
        insert into files values ('1', 'valid-file.txt', 'v1', 3001, 3001);
        insert into read_files values (1, 'invalid-read.txt', 4000);
        insert into read_files values ('1', 'valid-read.txt', 4001);",
    )
    .unwrap();
    let valid_body = format!("valid text sibling\n{}", "x".repeat(16_064));
    conn.execute(
        "insert into messages values (
            ?1, ?2, 'assistant', ?3, ?4, ?4, 'test', 'model', 0
         )",
        (
            "1",
            "1",
            serde_json::json!([{"type": "text", "data": {"text": valid_body}}]).to_string(),
            2002_i64,
        ),
    )
    .unwrap();
    drop(conn);

    let store_path = temp.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let first = import_crush_sqlite(
        &source_path,
        &mut store,
        options(&source_path, CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.failures.len(), 1);
    assert!(first.work_remaining);
    assert!(store
        .session_by_external_session(CaptureProvider::Crush, "1")
        .unwrap()
        .is_none());
    drop(store);

    let mut restarted = Store::open(&store_path).unwrap();
    let resumed = import_crush_sqlite(
        &source_path,
        &mut restarted,
        options(&source_path, CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(resumed.failed, 5);
    assert_eq!(resumed.failures.len(), 5);
    assert!(!resumed.work_remaining);
    assert!(resumed
        .failures
        .iter()
        .all(|failure| failure.error.contains("could not be decoded")));

    let events = session_events(&restarted, "1");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    let event_id = event.id;
    assert_eq!(event.sync.metadata["native_record_id"], "1");
    assert!(serde_json::to_string(event)
        .unwrap()
        .contains("valid text sibling"));

    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    let resolved = SqliteCompleteContentResolver::new()
        .resolve(&[complete_message_request(&source_path, event, locator)])
        .unwrap();
    assert_eq!(resolved[0].text, valid_body);

    let source_store = Connection::open(&store_path).unwrap();
    let touched_paths = source_store
        .prepare("select path from files_touched order by path")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        touched_paths,
        vec!["valid-file.txt".to_owned(), "valid-read.txt".to_owned()]
    );
    drop(source_store);

    let evidence = resumed.failures;
    drop(restarted);
    let mut reopened = Store::open(&store_path).unwrap();
    let noop = import_crush_sqlite(
        &source_path,
        &mut reopened,
        options(&source_path, CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(noop.failed, 5);
    assert_eq!(noop.failures, evidence);
    assert_eq!(session_events(&reopened, "1")[0].id, event_id);
}
