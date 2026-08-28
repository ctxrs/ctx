use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use ctx_history_capture::{
    provider_source_for_path, register_landed_source_backed_route_with_data_root,
    CaptureDocumentSpool, SourceBackedCoordinatorError, SourceBackedProviderRegistry,
    SourceBackedRefreshExecutor, SourceBackedRefreshScope, SourceBackedRoute,
    SourceBackedRouteControlExpectation, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use ctx_history_capture_composition::IndexCaptureLifecycle;
use ctx_history_capture_runtime::{
    replacement_document_tree_driver, CapturePublicationDisposition, DocumentInventoryAuthority,
    ReplacementDocumentTree,
};
use ctx_history_core::{CaptureProvider, SourceAnchorScope};
use ctx_history_index::{
    CompiledSearchFilter, LexicalExecution, LexicalMode, VerifiedIndex, WriterOptions,
};
use ctx_history_providers_sqlite_logical::{
    logical_sqlite_route_plan_scoped, LogicalSqliteRoutePlan, LogicalSqliteRuntimeBinding,
};
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};
use rusqlite::{params, Connection};
use serde_json::json;

struct ScopedReplayBinding;

impl LogicalSqliteRuntimeBinding for ScopedReplayBinding {
    type Lifecycle = IndexCaptureLifecycle;
    type Spool = CaptureDocumentSpool;
    type RouteControl = SourceBackedRouteControlExpectation;
}

#[test]
fn deepagents_durable_replay_rebinds_identical_content_to_the_current_root_scope() {
    assert_scoped_replay_lifecycle(
        CaptureProvider::DeepAgents,
        "deepagents scope replay marker",
        create_deepagents_database,
    );
}

#[test]
fn zed_durable_replay_rebinds_identical_content_to_the_current_root_scope() {
    assert_scoped_replay_lifecycle(
        CaptureProvider::Zed,
        "zed scope replay marker",
        create_zed_database,
    );
}

fn assert_scoped_replay_lifecycle(
    provider: CaptureProvider,
    marker: &str,
    create_database: fn(&Path, &str),
) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("source/provider.sqlite");
    create_database(&database, marker);
    let index_root = temp.path().join("index");
    let scope_a = SourceAnchorScope::Lineage([0xa1; 32]);
    let scope_b = SourceAnchorScope::Lineage([0xb2; 32]);

    let executor_a = SourceBackedRefreshExecutor::new(
        scoped_registry(provider, &database, data_root.path(), scope_a),
        WriterOptions::default(),
    );
    let mut cold = executor_a
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| Ok(b"logical-sqlite-scoped-replay-v1".to_vec()),
        )
        .unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert!(cold.successful_route_outcomes[0].changed);
    let source_a = cold.sources[0].observation().source().clone();
    let replay_fingerprint_a = cold.sources[0].frontier().unwrap().checkpoint().clone();
    let cold_generation = cold.commit.generation_id.clone();
    let (cold_disposition, cold_pin) = cold.take_verified_publication().unwrap();
    assert_eq!(cold_disposition, CapturePublicationDisposition::Published);
    let cold_index = cold_pin.into_inner().into_verified_index();
    let event_a = only_matching_event(&cold_index, marker);
    assert!(event_a.source.exact_descriptor_eq(&source_a));

    let executor_b = SourceBackedRefreshExecutor::new(
        scoped_registry(provider, &database, data_root.path(), scope_b),
        WriterOptions::default(),
    );
    let mut rebound = executor_b
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| Ok(b"logical-sqlite-scoped-replay-v1".to_vec()),
        )
        .unwrap();
    assert_eq!(rebound.sources.len(), 1);
    assert!(rebound.successful_route_outcomes[0].changed);
    assert_ne!(rebound.commit.generation_id, cold_generation);
    let source_b = rebound.sources[0].observation().source().clone();
    assert!(!source_b.exact_descriptor_eq(&source_a));
    assert_eq!(
        rebound.sources[0].frontier().unwrap().checkpoint(),
        &replay_fingerprint_a
    );
    let rebound_generation = rebound.commit.generation_id.clone();
    let (rebound_disposition, rebound_pin) = rebound.take_verified_publication().unwrap();
    assert_eq!(
        rebound_disposition,
        CapturePublicationDisposition::Published
    );
    let rebound_index = rebound_pin.into_inner().into_verified_index();
    let event_b = only_matching_event(&rebound_index, marker);
    assert!(event_b.source.exact_descriptor_eq(&source_b));
    assert_ne!(event_b.session_id, event_a.session_id);
    assert_ne!(event_b.event_id, event_a.event_id);
    assert!(rebound_index
        .event_by_id(event_a.event_id.as_uuid())
        .unwrap()
        .is_none());

    let mut replay = executor_b
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| Ok(b"unexpected-same-scope-publication".to_vec()),
        )
        .unwrap();
    assert_eq!(replay.commit.generation_id, rebound_generation);
    assert!(!replay.successful_route_outcomes[0].changed);
    assert!(replay.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&source_b));
    assert_eq!(
        replay.take_verified_publication().unwrap().0,
        CapturePublicationDisposition::Reused
    );
}

fn scoped_registry(
    provider: CaptureProvider,
    database: &Path,
    data_root: &Path,
    scope: SourceAnchorScope,
) -> SourceBackedProviderRegistry {
    let source = provider_source_for_path(provider, database.to_path_buf());
    let plan = logical_sqlite_route_plan_scoped::<ScopedReplayBinding>(
        source,
        SourceBackedRouteSelection::ExplicitManual,
        data_root,
        scope,
    )
    .unwrap();
    let authority = plan.selector_authority();
    let route = match plan {
        LogicalSqliteRoutePlan::DeepAgents { source, adapter } => {
            scoped_route(source, authority, adapter)
        }
        LogicalSqliteRoutePlan::Zed { source, adapter } => scoped_route(source, authority, adapter),
        _ => panic!("unexpected logical SQLite replay provider"),
    };
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    registry
}

fn scoped_route<A>(
    source: ctx_history_capture::ProviderSource,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
) -> SourceBackedRoute
where
    A: ReplacementDocumentTree<
        Lifecycle = IndexCaptureLifecycle,
        Spool = CaptureDocumentSpool,
        RouteControl = SourceBackedRouteControlExpectation,
    >,
{
    let inventory_authority =
        DocumentInventoryAuthority::new(source.provider.as_str().to_owned(), [0x71; 32]);
    let driver = replacement_document_tree_driver(inventory_authority, adapter);
    SourceBackedRoute::explicit_manual(source, selector_authority, driver).unwrap()
}

fn only_matching_event(index: &VerifiedIndex, marker: &str) -> ctx_history_index::EventRecord {
    let mut matches = matching_events(index, marker);
    assert_eq!(matches.len(), 1);
    matches.remove(0)
}

fn matching_events(index: &VerifiedIndex, marker: &str) -> Vec<ctx_history_index::EventRecord> {
    let filter = CompiledSearchFilter::compile(Default::default()).unwrap();
    let queries = [marker];
    let batch = index
        .execute_lexical(LexicalExecution::new(
            LexicalMode::Search(&queries),
            &filter,
            2,
        ))
        .unwrap()
        .batch;
    assert!(
        batch.complete,
        "lexical execution must complete: {:?}",
        batch.exhaustion
    );
    batch
        .candidates
        .into_iter()
        .map(|candidate| {
            let winner = ctx_history_index::EventSearchCandidate::from(candidate);
            index
                .event_by_id(winner.event.event_id)
                .unwrap()
                .expect("selected lexical winner must hydrate")
        })
        .collect()
}

fn create_deepagents_database(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table checkpoints (
                 thread_id text not null,
                 checkpoint_ns text not null,
                 checkpoint_id text not null,
                 checkpoint blob not null,
                 metadata blob
             );
             create table writes (
                 thread_id text not null,
                 checkpoint_ns text not null,
                 checkpoint_id text not null,
                 task_id text not null,
                 idx integer not null,
                 channel text not null,
                 type text,
                 value blob not null
             );",
        )
        .unwrap();
    let metadata = serde_json::to_vec(&json!({
        "updated_at": "2026-08-24T12:00:00Z",
        "cwd": "/workspace/deepagents"
    }))
    .unwrap();
    connection
        .execute(
            "insert into checkpoints (
                 thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata
             ) values ('thread-1', '', 'checkpoint-1', x'80', ?1)",
            [&metadata],
        )
        .unwrap();
    let message = MsgpackValue::Map(vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String("human".into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String(text.into()),
        ),
        (
            MsgpackValue::String("id".into()),
            MsgpackValue::String("message-1".into()),
        ),
    ]);
    let mut payload = Vec::new();
    write_msgpack_value(&mut payload, &message).unwrap();
    connection
        .execute(
            "insert into writes (
                 thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value
             ) values ('thread-1', '', 'checkpoint-1', 'task-1', 0, 'messages', 'msgpack', ?1)",
            [&payload],
        )
        .unwrap();
}

fn create_zed_database(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 3;
             create table threads (
                 id text primary key,
                 summary text not null,
                 updated_at text not null,
                 data_type text not null,
                 data blob not null,
                 parent_id text,
                 folder_paths text,
                 folder_paths_order text,
                 created_at text
             );",
        )
        .unwrap();
    let payload = serde_json::to_vec(&json!({
        "version": "0.3.0",
        "title": "Scoped replay Zed thread",
        "updated_at": "2026-08-24T12:00:10Z",
        "messages": [{
            "User": {
                "id": "message-1",
                "content": [{"Text": text}]
            }
        }]
    }))
    .unwrap();
    connection
        .execute(
            "insert into threads (
                 id, summary, updated_at, data_type, data, parent_id,
                 folder_paths, folder_paths_order, created_at
             ) values (
                 'thread-1', 'scoped replay fixture', '2026-08-24T12:00:10Z',
                 'json', ?1, null, '/workspace/zed', '0', '2026-08-24T12:00:00Z'
             )",
            params![payload],
        )
        .unwrap();
}

#[test]
fn opencode_family_changed_wal_capture_then_exact_replay_finishes_progress() {
    for provider in [
        CaptureProvider::OpenCode,
        CaptureProvider::Kilo,
        CaptureProvider::MiMoCode,
    ] {
        assert_opencode_family_changed_wal_capture_then_exact_replay(provider);
    }
}

fn assert_opencode_family_changed_wal_capture_then_exact_replay(provider: CaptureProvider) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp
        .path()
        .join("source")
        .join(format!("{}.sqlite", provider.as_str()));
    let writer = create_opencode_wal_database(&database, "logical SQLite facade cold capture");
    assert_active_wal(&database);
    let index_root = temp.path().join("index");
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        provider_source_for_path(provider, database.clone()),
        SourceBackedRouteSelection::ExplicitManual,
        data_root.path(),
    )
    .unwrap();
    let executor = SourceBackedRefreshExecutor::new(registry, WriterOptions::default());
    let metadata_calls = AtomicUsize::new(0);

    let mut cold = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| {
                metadata_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"logical-sqlite-replay-v1".to_vec())
            },
        )
        .unwrap();
    let cold_generation = cold.commit.generation_id.clone();
    assert_eq!(cold.successful_route_outcomes.len(), 1, "{provider:?}");
    assert!(cold.successful_route_outcomes[0].changed, "{provider:?}");
    assert_eq!(
        cold.take_verified_publication().unwrap().0,
        CapturePublicationDisposition::Published,
        "{provider:?}"
    );

    let changed_marker = format!("{} logical SQLite changed WAL capture", provider.as_str());
    append_opencode_message(&writer, &changed_marker);
    assert_active_wal(&database);
    let mut changed = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| {
                metadata_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"logical-sqlite-replay-v2".to_vec())
            },
        )
        .unwrap();
    assert_eq!(changed.successful_route_outcomes.len(), 1, "{provider:?}");
    assert!(changed.successful_route_outcomes[0].changed, "{provider:?}");
    assert_ne!(
        changed.commit.generation_id, cold_generation,
        "{provider:?}"
    );
    assert_eq!(changed.sources.len(), 1, "{provider:?}");
    assert_eq!(
        changed.sources[0].observation().source().provider(),
        provider.as_str(),
        "{provider:?}"
    );
    let changed_generation = changed.commit.generation_id.clone();
    let changed_opstamp = changed.commit.opstamp;
    let (changed_disposition, changed_pin) = changed.take_verified_publication().unwrap();
    assert_eq!(
        changed_disposition,
        CapturePublicationDisposition::Published,
        "{provider:?}"
    );
    let changed_index = changed_pin.into_inner().into_verified_index();
    assert!(
        !matching_events(&changed_index, &changed_marker).is_empty(),
        "{provider:?}"
    );
    let mut updates = Vec::new();
    let mut replay = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                updates.push(update);
                Ok(())
            },
            |_| {
                metadata_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"unexpected-replay-metadata".to_vec())
            },
        )
        .unwrap();
    assert_eq!(
        replay.commit.generation_id, changed_generation,
        "{provider:?}"
    );
    assert_eq!(replay.commit.opstamp, changed_opstamp, "{provider:?}");
    assert_eq!(metadata_calls.load(Ordering::SeqCst), 2, "{provider:?}");
    assert_eq!(replay.successful_route_outcomes.len(), 1, "{provider:?}");
    assert!(!replay.successful_route_outcomes[0].changed, "{provider:?}");
    let (replay_disposition, replay_pin) = replay.take_verified_publication().unwrap();
    assert_eq!(
        replay_disposition,
        CapturePublicationDisposition::Reused,
        "{provider:?}"
    );
    let replay_index = replay_pin.into_inner().into_verified_index();
    assert!(
        !matching_events(&replay_index, &changed_marker).is_empty(),
        "{provider:?}"
    );
    let terminal = updates.last().expect("terminal replay progress");
    assert_eq!(terminal.progress.phase, "committed", "{provider:?}");
    assert!(terminal.current_source_progress.is_none(), "{provider:?}");
    assert!(terminal.progress.current_source.is_none(), "{provider:?}");
    assert!(
        terminal.progress.completed_records.is_none(),
        "{provider:?}"
    );
    assert!(terminal.progress.completed_bytes.is_none(), "{provider:?}");

    if provider != CaptureProvider::OpenCode {
        return;
    }

    let error = executor
        .refresh_scope_with_detailed_progress(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                if update.current_source_progress.is_some() {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::ResourceUnavailable,
                        "injected logical SQLite replay progress failure",
                    ));
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError {
            kind: SourceBackedRouteErrorKind::ResourceUnavailable,
            detail,
        }) if detail == "injected logical SQLite replay progress failure"
    ));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        changed_generation
    );
}

fn create_opencode_wal_database(path: &Path, text: &str) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    let journal_mode = connection
        .query_row("pragma journal_mode = wal", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(journal_mode, "wal");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 parent_id text,
                 directory text,
                 branch text,
                 agent text,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table message (
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
             create index message_session_time_created_id_idx
                 on message(session_id, time_created, id);
             create index part_message_id_id_idx on part(message_id, id);
             insert into session values (
                 'session-1', null, '/tmp/project', 'main', 'build', 1, 1
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                'message-1', 'session-1', 1, 1, ?1
            )",
            params![json!({
                "role": "user",
                "time": {"created": 1}
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                'part-1', 'message-1', 'session-1', 1, 1, ?1
            )",
            params![json!({"type": "text", "text": text}).to_string()],
        )
        .unwrap();
    connection
}

fn append_opencode_message(connection: &Connection, text: &str) {
    connection
        .execute(
            "insert into message values (
                'message-2', 'session-1', 2, 2, ?1
            )",
            params![json!({
                "role": "assistant",
                "time": {"created": 2}
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                'part-2', 'message-2', 'session-1', 2, 2, ?1
            )",
            params![json!({"type": "text", "text": text}).to_string()],
        )
        .unwrap();
}

fn assert_active_wal(database: &Path) {
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    assert!(fs::metadata(Path::new(&wal)).unwrap().len() > 0);
}
