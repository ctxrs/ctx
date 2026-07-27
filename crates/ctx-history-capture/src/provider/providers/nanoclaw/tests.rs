use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{TimeZone, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::{decode_native_path_committed_cursor, Store};
use rusqlite::Connection;
use tempfile::TempDir;

use super::*;
use crate::provider::importer::{provider_path_identity, provider_source_cursor_stream_for_path};
use crate::{
    CaptureWorkLimit, ImportProfile, ProviderImportOptions, ProviderImportWorkResult,
    NANOCLAW_SOURCE_FORMAT,
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

#[test]
fn nativepath_import_is_cursor_committed_and_idempotent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "native", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in-1", 1, 1_000, "native-inbound-marker");
    insert_outbound(&outbound, "out-1", 2, 2_000, "native-outbound-marker");
    let stream = cursor_stream(&root);
    let mut store = Store::open(temp.path().join("store.sqlite")).unwrap();

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
