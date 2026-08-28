use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};

use crate::{provider_sources::provider_source_for_path, test_support_paths};
use ctx_history_capture_composition::{
    refresh_source_backed_generation, register_goose_source_backed_route,
    register_landed_source_backed_route_with_data_root, register_warp_source_backed_route,
    SourceBackedCoordinatorError, SourceBackedProviderRegistry, SourceBackedRefreshReceipt,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection,
};

#[test]
fn goose_v15_active_wal_append_preserves_identities_and_replays_exactly() {
    let temp = test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
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
    assert_active_wal(&database);

    let mut registry = SourceBackedProviderRegistry::new();
    let source = provider_source_for_path(CaptureProvider::Goose, database.clone());
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
    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&cold);

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

    connection
        .execute(
            "insert into messages
                 (message_id, session_id, role, content_json, created_timestamp)
             values (?1, ?2, ?3, ?4, ?5)",
            params![
                "goose-wal-append",
                "child-session",
                "user",
                r#"[{"type":"text","text":"goose lifecycle wal append"}]"#,
                4,
            ],
        )
        .unwrap();
    assert_active_wal(&database);

    let changed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&changed);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    let changed_record =
        only_indexed_record(&index, CaptureProvider::Goose, "goose lifecycle wal append");

    let replay = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&replay);
    assert_eq!(replay.commit.generation_id, changed.commit.generation_id);
    assert_eq!(replay.sources, changed.sources);
    assert_eq!(
        only_indexed_record(&index, CaptureProvider::Goose, "goose lifecycle wal append",),
        changed_record
    );
}

#[test]
fn warp_active_wal_update_publishes_and_then_replays_exactly() {
    const INITIAL_PROMPT: &str = "warp sqlite oracle prompt";
    const UPDATED_PROMPT: &str = "warp wal lifecycle update";

    let temp = test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    fs::create_dir(&provider).unwrap();
    let database = provider.join("warp.sqlite");
    fs::copy(
        test_support_paths::capture_repo_root()
            .join("tests/fixtures/provider-history/warp/v1/warp.sqlite"),
        &database,
    )
    .unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .execute(
            "update agent_tasks set last_modified_at = ?1 where task_id = ?2",
            params!["2026-06-24 12:00:06", "warp-task-root"],
        )
        .unwrap();
    assert_active_wal(&database);

    let mut registry = SourceBackedProviderRegistry::new();
    let source = provider_source_for_path(CaptureProvider::Warp, database.clone());
    register_warp_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        &temp.path().join("data-root"),
        "linux:stable:gui",
        None,
    )
    .unwrap();
    let index = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&cold);
    only_indexed_record(&index, CaptureProvider::Warp, INITIAL_PROMPT);

    let (task_id, mut task) = connection
        .query_row(
            "select task_id, task from agent_tasks where task_id = ?1",
            ["warp-task-root"],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .unwrap();
    replace_once(
        &mut task,
        INITIAL_PROMPT.as_bytes(),
        UPDATED_PROMPT.as_bytes(),
    );
    connection
        .execute(
            "update agent_tasks set task = ?1, last_modified_at = ?2 where task_id = ?3",
            params![task, "2026-06-24 12:00:07", task_id],
        )
        .unwrap();
    assert_active_wal(&database);

    let changed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&changed);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(
        changed.commit.indexed_documents,
        cold.commit.indexed_documents
    );
    let changed_record = only_indexed_record(&index, CaptureProvider::Warp, UPDATED_PROMPT);

    let replay = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_clean_refresh(&replay);
    assert_eq!(replay.commit.generation_id, changed.commit.generation_id);
    assert_eq!(replay.sources, changed.sources);
    assert_eq!(
        only_indexed_record(&index, CaptureProvider::Warp, UPDATED_PROMPT),
        changed_record
    );
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

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn assert_clean_refresh(receipt: &SourceBackedRefreshReceipt) {
    assert!(
        receipt.failed_routes.is_empty(),
        "unexpected route failures: {:?}",
        receipt.failed_routes
    );
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.successful_route_ids.len(), 1);
}

fn assert_active_wal(database: &Path) {
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    assert!(Path::new(&wal).is_file(), "active WAL sidecar is missing");
}

fn replace_once(bytes: &mut [u8], original: &[u8], replacement: &[u8]) {
    assert_eq!(original.len(), replacement.len());
    let offset = {
        let mut matches = bytes
            .windows(original.len())
            .enumerate()
            .filter(|(_, candidate)| *candidate == original)
            .map(|(offset, _)| offset);
        let offset = matches.next().expect("fixture prompt is missing");
        assert!(matches.next().is_none(), "fixture prompt is ambiguous");
        offset
    };
    bytes[offset..offset + replacement.len()].copy_from_slice(replacement);
}

fn only_indexed_record(index_root: &Path, provider: CaptureProvider, marker: &str) -> CoreRecord {
    let index = VerifiedIndex::open(index_root).unwrap();
    let records = super::lexical_test_support::search_event_candidates(&index, marker, 16)
        .into_iter()
        .filter_map(|candidate| {
            index
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
        })
        .filter(|record| {
            record.source.provider() == provider.as_str()
                && record.content.meaningful_text().contains(marker)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "expected one indexed {provider} record containing {marker:?}"
    );
    records.into_iter().next().unwrap()
}
