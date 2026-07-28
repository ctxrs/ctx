use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{TimeZone, Utc};
use ctx_history_core::{
    CaptureProvider, Event, LocatorRevisionPolicy, NativeRecordCoordinate, SyncCursor, TypedKey,
};
use ctx_history_store::{decode_native_path_committed_cursor, Store};
use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::native_path::source_backed::{
    hydrate_nanoclaw_source_backed_exact, scan_nanoclaw_source_backed,
};
use super::position::{nanoclaw_message_locator, NanoClawMessageSource};
use super::rows::NANOCLAW_NATIVE_MAX_RECORD_BYTES;
use super::*;
use crate::complete_content::{
    source_access::set_nanoclaw_before_source_set_revalidation,
    sqlite::CompleteContentSqliteQueryBudget, AuthorizedSourceRoute, CompleteContentErrorKind,
    CompleteContentSourceFamily, CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::native_source::NativePosition;
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, timestamps,
    BoundedParserCheckpoint, CertifiedProviderCursor,
};
use crate::{
    CaptureWorkLimit, ImportProfile, ProviderImportOptions, ProviderImportWorkResult,
    NANOCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "machine-nanoclaw-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 12, 0, 0)
            .single()
            .unwrap(),
    }
}

fn import_options(work_limit: CaptureWorkLimit) -> ProviderImportOptions {
    ProviderImportOptions {
        capture_work_limit: work_limit,
        import_profile: ImportProfile::CoreOnly,
        ..ProviderImportOptions::default()
    }
}

fn create_project(temp: &TempDir, name: &str, sessions: usize) -> PathBuf {
    let root = temp.path().join(name);
    let data = root.join("data");
    fs::create_dir_all(data.join("v2-sessions")).unwrap();
    let central = Connection::open(data.join("v2.db")).unwrap();
    central
        .execute_batch(
            "create table agent_groups (
                id text primary key, name text, folder text, agent_provider text
            );
            create table messaging_groups (
                id text primary key, channel_type text, platform_id text,
                instance text, name text
            );
            create table sessions (
                id text primary key, agent_group_id text not null,
                messaging_group_id text, thread_id text, agent_provider text,
                status text, container_status text, last_active integer,
                created_at integer
            );
            insert into agent_groups values (
                'ag-1', 'Personal', '/workspace/nanoclaw', 'codex'
            );
            insert into messaging_groups values (
                'mg-1', 'telegram', 'chat-1', 'default', 'DM'
            );",
        )
        .unwrap();
    for index in 0..sessions {
        central
            .execute(
                "insert into sessions values (
                    ?1, 'ag-1', 'mg-1', ?2, 'codex', 'active', 'running',
                    ?3, ?4
                )",
                rusqlite::params![
                    format!("session-{index:04}"),
                    format!("thread-{index:04}"),
                    1_782_259_202_000_i64 + index as i64,
                    1_782_259_200_000_i64 + index as i64,
                ],
            )
            .unwrap();
    }
    root
}

fn create_message_stores(root: &Path, session_id: &str) -> (PathBuf, PathBuf) {
    let session_dir = root
        .join("data")
        .join("v2-sessions")
        .join("ag-1")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let inbound_path = session_dir.join("inbound.db");
    let inbound = Connection::open(&inbound_path).unwrap();
    inbound
        .execute_batch(
            "create table messages_in (
                id text primary key, seq integer, kind text, timestamp integer,
                status text, trigger text, platform_id text, channel_type text,
                thread_id text, content text, source_session_id text, on_wake integer
            );",
        )
        .unwrap();
    let outbound_path = session_dir.join("outbound.db");
    let outbound = Connection::open(&outbound_path).unwrap();
    outbound
        .execute_batch(
            "create table messages_out (
                id text primary key, seq integer, in_reply_to text, timestamp integer,
                kind text, platform_id text, channel_type text, thread_id text,
                content text
            );",
        )
        .unwrap();
    (inbound_path, outbound_path)
}

fn insert_inbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_in values (
                ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                'thread', ?4, null, 0
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

fn insert_outbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_out values (
                ?1, ?2, null, ?3, 'chat', 'chat-1', 'telegram', 'thread', ?4
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

fn cursor_stream(root: &Path) -> String {
    let canonical_root = fs::canonicalize(root).unwrap();
    let identity = provider_path_identity(&canonical_root).unwrap();
    provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &identity,
    )
}

fn nanoclaw_event_count(store: &Store) -> usize {
    let archive = store.export_archive().unwrap();
    let source_ids = archive
        .capture_sources
        .iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::NanoClaw)
        .map(|source| source.id)
        .collect::<std::collections::BTreeSet<_>>();
    archive
        .events
        .iter()
        .filter(|event| {
            event
                .capture_source_id
                .is_some_and(|source_id| source_ids.contains(&source_id))
        })
        .count()
}

fn nanoclaw_events(store: &Store) -> Vec<Event> {
    let archive = store.export_archive().unwrap();
    let source_ids = archive
        .capture_sources
        .iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::NanoClaw)
        .map(|source| source.id)
        .collect::<std::collections::BTreeSet<_>>();
    archive
        .events
        .into_iter()
        .filter(|event| {
            event
                .capture_source_id
                .is_some_and(|source_id| source_ids.contains(&source_id))
        })
        .collect()
}

fn active_nanoclaw_events(store: &Store) -> Vec<Event> {
    nanoclaw_events(store)
        .into_iter()
        .filter(|event| event.sync.deleted_at.is_none())
        .collect()
}

fn event_containing(store: &Store, marker: &str) -> Event {
    nanoclaw_events(store)
        .into_iter()
        .find(|event| serde_json::to_string(event).unwrap().contains(marker))
        .unwrap()
}

fn provider_cursor(store: &Store, root: &Path) -> String {
    let stored = store
        .get_sync_cursor(None, "machine-nanoclaw-test", &cursor_stream(root))
        .unwrap()
        .unwrap();
    decode_native_path_committed_cursor(&stored.cursor)
        .unwrap()
        .provider_cursor()
        .to_owned()
}

fn install_released_cursor(store: &Store, root: &Path) {
    let cursor = CertifiedProviderCursor::new(
        "released-nanoclaw-source-revision",
        NANOCLAW_CAPTURE_REVISION,
        NANOCLAW_POLICY_REVISION,
        NativePosition::new("nanoclaw-project-keyset-v1", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "machine-nanoclaw-test".to_owned(),
            stream: cursor_stream(root),
            cursor,
            last_synced_at: None,
            timestamps: timestamps(chrono::DateTime::<Utc>::UNIX_EPOCH),
        })
        .unwrap();
}

#[test]
fn nativepath_import_is_cursor_committed_and_idempotent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "native", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "native-inbound-marker");
    insert_outbound(&outbound, "out-1", 2, 2_000, "native-outbound-marker");
    let stream = cursor_stream(&root);
    let store_path = temp.path().join("store.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let first = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(nanoclaw_event_count(&store), 2);
    let stored = store
        .get_sync_cursor(None, "machine-nanoclaw-test", &stream)
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    assert!(committed
        .provider_cursor()
        .starts_with("nanoclaw-nativepath-v1:"));
    assert!(committed.journal_checkpoint().is_some());
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let replay = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(nanoclaw_event_count(&store), 2);
}

#[test]
fn nativepath_append_resumes_and_one_group_is_bounded() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "append", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    for index in 0..150 {
        insert_inbound(
            &inbound,
            &format!("in-{index:03}"),
            index,
            1_000 + index,
            &format!("bounded-marker-{index:03}"),
        );
    }
    let mut store = Store::open(temp.path().join("bounded.sqlite")).unwrap();
    let first = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert!(first.work_remaining);
    assert!(nanoclaw_event_count(&store) < 150);

    let drained = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert!(!drained.work_remaining);
    assert_eq!(nanoclaw_event_count(&store), 150);

    insert_inbound(&inbound, "in-tail", 151, 2_000, "append-tail-marker");
    let appended = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(nanoclaw_event_count(&store), 151);
}

#[test]
fn nativepath_source_disappearance_retires_exact_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "retire", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "retirement-marker");
    let mut store = Store::open(temp.path().join("retire.sqlite")).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let held = temp.path().join("retire-held");
    fs::rename(&root, &held).unwrap();

    let retired = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(nanoclaw_event_count(&store), 1);

    let repeated = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);

    fs::rename(&held, &root).unwrap();
    let reactivated = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(reactivated.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(nanoclaw_event_count(&store), 1);
}

#[test]
fn same_id_rewrite_updates_in_place_and_truncation_retires_the_omitted_tail() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "rewrite", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    let old_body = format!(
        "rewriteoldmarker {}",
        "o".repeat(PROVIDER_MAX_TEXT_CHARS + 32)
    );
    insert_inbound(&inbound, "stable-id", 1, 1_000, &old_body);
    insert_inbound(&inbound, "tail-id", 2, 2_000, "truncated-tail-marker");
    let mut store = Store::open(temp.path().join("rewrite.sqlite")).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let old = store
        .get_event(event_containing(&store, "rewriteoldmarker").id)
        .unwrap();
    let tail = event_containing(&store, "truncated-tail-marker");
    assert_eq!(
        old.payload.pointer("/body/text_retention/truncated"),
        Some(&json!(true)),
        "{}",
        old.payload
    );
    let old_locator = old.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].clone();

    let connection = Connection::open(&inbound).unwrap();
    connection
        .execute(
            "update messages_in set content = ?1 where id = 'stable-id'",
            [json!({
                "text": format!(
                    "rewritenewmarker {} tailonlyalpha",
                    "n".repeat(PROVIDER_MAX_TEXT_CHARS + 32)
                )
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute("delete from messages_in where id = 'tail-id'", [])
        .unwrap();
    drop(connection);

    let rewritten = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(rewritten.failed, 0, "{:?}", rewritten.failures);
    let current = store
        .get_event(event_containing(&store, "rewritenewmarker").id)
        .unwrap();
    assert_eq!(current.id, old.id);
    assert_ne!(
        current.sync.metadata["provider_event_hash"],
        old.sync.metadata["provider_event_hash"]
    );
    assert_ne!(
        current.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY], old_locator,
        "old metadata: {}; current metadata: {}",
        old.sync.metadata, current.sync.metadata,
    );
    assert_eq!(
        current.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert!(store
        .search_event_hits("rewritenewmarker", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.event_id == current.id));
    assert!(store
        .search_event_hits("rewriteoldmarker", 10)
        .unwrap()
        .is_empty());

    let current_locator = current.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].clone();
    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = ?1 where id = 'stable-id'",
            [json!({
                "text": format!(
                    "rewritenewmarker {} tailonlybravo",
                    "n".repeat(PROVIDER_MAX_TEXT_CHARS + 32)
                )
            })
            .to_string()],
        )
        .unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let tail_rewritten = store.get_event(current.id).unwrap();
    assert_eq!(tail_rewritten.id, current.id);
    assert_eq!(tail_rewritten.payload["body"], current.payload["body"]);
    assert_ne!(
        tail_rewritten.sync.metadata["provider_event_hash"],
        current.sync.metadata["provider_event_hash"]
    );
    assert_ne!(
        tail_rewritten.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        current_locator
    );
    assert!(store
        .search_event_hits("tailonlyalpha", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("tailonlybravo", 10)
        .unwrap()
        .is_empty());
    assert_eq!(active_nanoclaw_events(&store).len(), 1);
    assert!(store.get_event(tail.id).unwrap().sync.deleted_at.is_some());
}

#[test]
fn session_deletion_retires_only_the_omitted_session_and_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "session-delete", 2);
    let (first, _) = create_message_stores(&root, "session-0000");
    let (second, _) = create_message_stores(&root, "session-0001");
    insert_inbound(&first, "first", 1, 1_000, "retained-session-marker");
    insert_inbound(&second, "second", 1, 1_000, "deleted-session-marker");
    let mut store = Store::open(temp.path().join("session-delete.sqlite")).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let deleted_event = event_containing(&store, "deleted-session-marker");
    let deleted_session_id = deleted_event.session_id.unwrap();

    Connection::open(root.join("data").join("v2.db"))
        .unwrap()
        .execute("delete from sessions where id = 'session-0001'", [])
        .unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();

    assert_eq!(active_nanoclaw_events(&store).len(), 1);
    assert!(serde_json::to_string(&active_nanoclaw_events(&store))
        .unwrap()
        .contains("retained-session-marker"));
    assert!(store
        .get_event(deleted_event.id)
        .unwrap()
        .sync
        .deleted_at
        .is_some());
    assert!(store
        .get_session(deleted_session_id)
        .unwrap()
        .sync
        .deleted_at
        .is_some());
}

#[test]
fn bounded_retirement_resumes_after_store_reopen() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "retirement-reopen", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    for index in 0..140 {
        insert_inbound(
            &inbound,
            &format!("row-{index:03}"),
            index,
            1_000 + index,
            &format!("restart-retirement-{index:03}"),
        );
    }
    let store_path = temp.path().join("retirement-reopen.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    Connection::open(&inbound)
        .unwrap()
        .execute("delete from messages_in where id <> 'row-000'", [])
        .unwrap();

    for _ in 0..12 {
        if provider_cursor(&store, &root).contains("\"retirement\"") {
            break;
        }
        let partial = import_nanoclaw_nativepath(
            &root,
            &mut store,
            context(&root),
            import_options(CaptureWorkLimit::OneSafeGroup),
        )
        .unwrap();
        assert!(partial.work_remaining);
    }
    assert!(provider_cursor(&store, &root).contains("\"retirement\""));
    let partial_retirement = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert!(partial_retirement.work_remaining);
    assert!(provider_cursor(&store, &root).contains("\"retirement\""));
    drop(store);

    let mut reopened = Store::open(&store_path).unwrap();
    let completed = import_nanoclaw_nativepath(
        &root,
        &mut reopened,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert!(!completed.work_remaining);
    assert_eq!(active_nanoclaw_events(&reopened).len(), 1);
    let cursor = provider_cursor(&reopened, &root);
    assert!(cursor.contains("\"terminal\":true"));
    assert!(!cursor.contains("\"retirement\""));
}

#[test]
fn malformed_rows_are_bounded_rejections_and_advance_the_frontier() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "mixed-rejections", 2);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    Connection::open(root.join("data").join("v2.db"))
        .unwrap()
        .execute(
            "update sessions set status = CAST(x'80ff' AS TEXT) where id = 'session-0001'",
            [],
        )
        .unwrap();
    insert_inbound(&inbound, "valid-1", 1, 1_000, "valid-before-rejection");
    Connection::open(&inbound)
        .unwrap()
        .execute(
            "insert into messages_in values (
                'invalid', 'not-an-integer', 'chat', 1500, 'done', 'message',
                'chat-1', 'telegram', 'thread', x'80ff', null, 0
            )",
            [],
        )
        .unwrap();
    Connection::open(&inbound)
        .unwrap()
        .execute(
            "insert into messages_in values (
                'invalid-utf8', 2, 'chat', 1600, 'done', 'message',
                'chat-1', 'telegram', 'thread', CAST(x'80ff' AS TEXT), null, 0
            )",
            [],
        )
        .unwrap();
    insert_inbound(&inbound, "invalid-validation", -1, 1_700, "negative-seq");
    insert_inbound(&inbound, "valid-2", 3, 2_000, "valid-after-rejection");
    let store_path = temp.path().join("mixed-rejections.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let mixed = import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(mixed.failed, 4, "{:?}", mixed.failures);
    assert_eq!(active_nanoclaw_events(&store).len(), 2);
    assert!(provider_cursor(&store, &root).contains("\"terminal\":true"));
    drop(store);

    let mut reopened = Store::open(&store_path).unwrap();
    let replay = import_nanoclaw_nativepath(
        &root,
        &mut reopened,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(active_nanoclaw_events(&reopened).len(), 2);

    let all_invalid_root = create_project(&temp, "all-invalid", 1);
    let (all_invalid, _) = create_message_stores(&all_invalid_root, "session-0000");
    let connection = Connection::open(&all_invalid).unwrap();
    connection
        .execute(
            "insert into messages_in values (
                'bad-type', 'bad-seq', 'chat', 1000, 'done', 'message',
                'chat-1', 'telegram', 'thread', x'80ff', null, 0
            )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "insert into messages_in values (
                'too-large', 2, 'chat', 2000, 'done', 'message',
                'chat-1', 'telegram', 'thread', ?1, null, 0
            )",
            ["x".repeat(NANOCLAW_NATIVE_MAX_RECORD_BYTES as usize + 1)],
        )
        .unwrap();
    drop(connection);
    let mut invalid_store = Store::open(temp.path().join("all-invalid.sqlite")).unwrap();
    let invalid = import_nanoclaw_nativepath(
        &all_invalid_root,
        &mut invalid_store,
        context(&all_invalid_root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert_eq!(invalid.failed, 2, "{:?}", invalid.failures);
    assert!(active_nanoclaw_events(&invalid_store).is_empty());
    assert!(provider_cursor(&invalid_store, &all_invalid_root).contains("\"terminal\":true"));
}

#[test]
fn exact_released_source_id_hash_migrates_in_place() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "legacy-hash", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "legacy-id", 1, 1_000, "legacy-migration-marker");
    let store_path = temp.path().join("legacy-hash.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let mut released = event_containing(&store, "legacy-migration-marker");
    let released_id = released.id;
    let legacy_hash = "inbound:legacy-id";
    released.dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        released.dedupe_key.as_deref().unwrap(),
        legacy_hash,
    );
    released.payload["provider_event_hash"] = json!(legacy_hash);
    released.sync.metadata["provider_event_hash"] = json!(legacy_hash);
    released.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
    drop(store);
    let raw = Connection::open(&store_path).unwrap();
    raw.create_scalar_function(
        "ctx_projection_writer_authorized_v1",
        0,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(1_i64),
    )
    .unwrap();
    raw.execute(
        "update events set payload_json = ?1, dedupe_key = ?2, metadata_json = ?3 where id = ?4",
        rusqlite::params![
            serde_json::to_string(&released.payload).unwrap(),
            released.dedupe_key,
            serde_json::to_string(&released.sync.metadata).unwrap(),
            released.id.to_string(),
        ],
    )
    .unwrap();
    drop(raw);

    let mut store = Store::open(&store_path).unwrap();
    install_released_cursor(&store, &root);
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    let migrated = store.get_event(released_id).unwrap();
    assert_eq!(migrated.id, released_id);
    assert_eq!(
        migrated.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert!(!migrated
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(legacy_hash));
    assert_eq!(active_nanoclaw_events(&store).len(), 1);
}

#[test]
fn compound_locator_recovers_exact_inbound_and_outbound_content_without_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "locator", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "exact-inbound-content");
    insert_outbound(&outbound, "outbound", 2, 2_000, "exact-outbound-content");
    let inbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let outbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Outbound, 1).unwrap();
    let project = NanoClawCompleteProject::open(
        &root,
        &[inbound_locator.clone(), outbound_locator.clone()],
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    assert_eq!(
        project.resolve(&inbound_locator).unwrap().unwrap().text,
        "exact-inbound-content"
    );
    assert_eq!(
        project.resolve(&outbound_locator).unwrap().unwrap().text,
        "exact-outbound-content"
    );

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'mutated-content' where id = 'inbound'",
            [],
        )
        .unwrap();
    assert!(project.resolve(&inbound_locator).is_err());
}

#[test]
fn core_projection_never_persists_or_indexes_the_complete_tail() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "privacy", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    let secret = "NANOCLAW_COMPLETE_TAIL_MUST_NOT_PERSIST";
    let mut content = "p".repeat(PROVIDER_MAX_TEXT_CHARS + 256);
    content.push_str(secret);
    insert_inbound(&inbound, "private-tail", 1, 1_000, &content);
    let mut store = Store::open(temp.path().join("privacy.sqlite")).unwrap();
    import_nanoclaw_nativepath(
        &root,
        &mut store,
        context(&root),
        import_options(CaptureWorkLimit::Drain),
    )
    .unwrap();
    assert!(!serde_json::to_string(&store.export_archive().unwrap())
        .unwrap()
        .contains(secret));
    assert!(store.search_event_hits(secret, 10).unwrap().is_empty());

    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let project = NanoClawCompleteProject::open(
        &root,
        std::slice::from_ref(&locator),
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    assert!(project
        .resolve(&locator)
        .unwrap()
        .unwrap()
        .text
        .ends_with(secret));
}

#[test]
fn source_backed_cold_scan_has_stable_ids_compound_evidence_and_exact_locators() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "source-backed", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "source-backed-inbound");
    insert_outbound(&outbound, "outbound", 2, 2_000, "source-backed-outbound");
    let lineage = [0x4a; 32];

    let mut documents = Vec::new();
    let receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        assert!(!page.documents.is_empty());
        assert!(page.documents.len() <= 64);
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(documents.len(), 2);
    assert_eq!(receipt.emitted_pages, 1);
    assert_eq!(
        receipt.source.counts(),
        ctx_history_core::ScannedSourceCounts {
            complete_records: 3,
            retained_records: 2,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 2,
            certified_bytes: receipt.source.counts().certified_bytes,
        }
    );
    assert!(receipt.source.counts().certified_bytes > 0);
    let evidence: serde_json::Value =
        serde_json::from_slice(receipt.source.observation().revision()).unwrap();
    assert_eq!(evidence["version"], json!(1));
    assert_eq!(evidence["sessions"], json!(1));
    assert_eq!(evidence["component_databases"], json!(2));
    assert!(evidence["central_sha256"].as_str().unwrap().len() == 64);
    assert!(evidence["session_inventory_sha256"].as_str().unwrap().len() == 64);

    let canonical_root = fs::canonicalize(&root).unwrap().display().to_string();
    for document in &documents {
        assert_eq!(document.parent_session_id, None);
        assert_eq!(document.root_session_id, document.session_id);
        assert_eq!(document.provider_session_id.as_deref(), Some("thread-0000"));
        assert_eq!(document.branch, None);
        assert_eq!(
            document.source_path.as_deref(),
            Some(canonical_root.as_str())
        );
        assert_eq!(document.agent_type, "codex");
        assert!(document.is_primary);
        assert_eq!(
            document.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(document
            .locator
            .certified_source_revision_digest()
            .is_some());
        let NativeRecordCoordinate::ProviderNative {
            namespace,
            coordinate,
        } = document.locator.coordinate()
        else {
            panic!("NanoClaw source-backed locator was not provider-native");
        };
        assert_eq!(namespace, NANOCLAW_MESSAGE_LOCATOR_KIND);
        assert!(matches!(coordinate, TypedKey::Bytes(value) if value.len() == 17));
    }

    let exact = documents
        .iter()
        .map(|document| {
            hydrate_nanoclaw_source_backed_exact(&root, lineage, &document.locator)
                .unwrap()
                .text
        })
        .collect::<Vec<_>>();
    assert_eq!(
        exact,
        vec!["source-backed-inbound", "source-backed-outbound"]
    );

    let mut repeated_documents = Vec::new();
    let repeated = scan_nanoclaw_source_backed(&root, lineage, |page| {
        repeated_documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(receipt.source, repeated.source);
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>(),
        repeated_documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>()
    );

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'changed' where id = 'inbound'",
            [],
        )
        .unwrap();
    assert!(hydrate_nanoclaw_source_backed_exact(&root, lineage, &documents[0].locator).is_err());
}

#[test]
fn source_backed_partial_authority_and_unsupported_roots_fail_before_emitting() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "partial-authority", 1);
    let (_, outbound) = create_message_stores(&root, "session-0000");
    let lineage = [0x73; 32];
    let mut emitted = 0;

    let unsupported = root.join("data").join("v2-sessions");
    assert!(scan_nanoclaw_source_backed(&unsupported, lineage, |_| {
        emitted += 1;
        Ok(())
    })
    .is_err());
    assert_eq!(emitted, 0);

    Connection::open(outbound)
        .unwrap()
        .execute_batch("drop table messages_out; create table unrelated (id text);")
        .unwrap();
    let error = scan_nanoclaw_source_backed(&root, lineage, |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("messages_out"));
    assert_eq!(emitted, 0);
}

fn nanoclaw_broker_route(root: &Path) -> AuthorizedSourceRoute {
    AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: root.to_path_buf(),
        source_root: root.parent().map(Path::to_path_buf),
        source_identity: Some("nanoclaw-root-safety".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    }
}

fn complete_content_locator(
    locator: &crate::native_source::NativeLocator,
) -> CompleteContentSourceLocator {
    CompleteContentSourceLocator::new(locator.kind(), locator.value().to_vec()).unwrap()
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_snapshot_stays_exact_after_live_leaf_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-exact", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-snapshot");
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_broker_route(&root),
            std::slice::from_ref(&source_locator),
            event_id,
        )
        .unwrap();

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();

    let project = access
        .open_nanoclaw_project(
            std::slice::from_ref(&locator),
            CompleteContentSqliteQueryBudget::new(),
            event_id,
        )
        .unwrap();
    let resolved = project.resolve(&locator).unwrap().unwrap();
    assert_eq!(resolved.text, "inside-nanoclaw-snapshot");
    assert!(!resolved.text.contains("OUTSIDE_NANOCLAW"));
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_root_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-root", 1);
    let moved = temp.path().join("moved-broker-root");
    let replacement = create_project(&temp, "replacement-broker-root", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-root");
    let (replacement_inbound, _) = create_message_stores(&replacement, "session-0000");
    insert_inbound(
        &replacement_inbound,
        "inbound",
        1,
        1_000,
        "OUTSIDE_NANOCLAW_ROOT_MUST_NOT_ESCAPE",
    );
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let root = root.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&root, moved).unwrap();
                fs::rename(replacement, root).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_leaf_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-leaf", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-leaf");
    let moved = inbound.with_extension("moved");
    let replacement = inbound.with_extension("replacement");
    fs::copy(&inbound, &replacement).unwrap();
    Connection::open(&replacement)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_LEAF_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let inbound = inbound.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&inbound, moved).unwrap();
                fs::rename(replacement, inbound).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}
