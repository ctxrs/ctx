use std::{fs, path::Path};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};

use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_progress,
        register_warp_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, WARP_SQLITE_SOURCE_FORMAT,
};

fn field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3 | 2);
    value.extend(varint(payload.len() as u64));
    value.extend_from_slice(payload);
    value
}

fn integer_field(number: u32, integer: u64) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3);
    value.extend(varint(integer));
    value
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return output;
        }
    }
}

fn text_task(task_id: &str, message_id: &str, body: &str) -> Vec<u8> {
    let mut timestamp = integer_field(1, 1_782_259_200);
    timestamp.extend(integer_field(2, 0));
    let text = field(2, &field(1, body.as_bytes()));
    let mut message = field(1, message_id.as_bytes());
    message.extend(text);
    message.extend(field(11, task_id.as_bytes()));
    message.extend(field(13, b"request-1"));
    message.extend(field(14, &timestamp));
    let mut task = field(1, task_id.as_bytes());
    task.extend(field(2, b"Task"));
    task.extend(field(5, &message));
    task
}

fn create_source(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 1;
             create table agent_conversations (
                 id integer primary key,
                 conversation_id text not null unique,
                 conversation_data text not null,
                 last_modified_at text not null
             );
             create table agent_tasks (
                 id integer primary key,
                 conversation_id text not null,
                 task_id text not null unique,
                 task blob not null,
                 last_modified_at text not null
             );
             create table ai_queries (
                 id integer primary key,
                 exchange_id text not null unique,
                 conversation_id text not null,
                 start_ts text not null,
                 input text not null,
                 working_directory text,
                 output_status text not null,
                 model_id text not null,
                 planning_model_id text not null default '',
                 coding_model_id text not null default ''
             );
             insert into agent_conversations
                 (conversation_id, conversation_data, last_modified_at)
             values ('conversation', '{\"agent_name\":\"Warp\"}', '2026-07-24 12:00:00');",
        )
        .unwrap();
    connection
}

fn insert_task(connection: &Connection, task_id: &str, message_id: &str, body: &str) {
    connection
        .execute(
            "insert into agent_tasks
             (conversation_id, task_id, task, last_modified_at)
             values ('conversation', ?1, ?2, '2026-07-24 12:00:01')",
            params![task_id, text_task(task_id, message_id, body)],
        )
        .unwrap();
}

fn registry(path: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Warp,
        path: path.to_owned(),
        exists: true,
        source_format: WARP_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_warp_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        crate::test_provider_sqlite_data_root(),
        "linux:stable:gui",
    )
    .unwrap();
    registry
}

fn options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}{suffix}", path.display()))
}

#[test]
fn warp_active_wal_noop_replace_delete_and_batch_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("warp.sqlite");
    let index = temp.path().join("index");
    let writer = create_source(&database);
    writer
        .query_row("pragma journal_mode = wal", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    insert_task(&writer, "task-1", "message-1", "first logical body");
    insert_task(&writer, "task-2", "message-2", "second logical body");
    assert!(sidecar(&database, "-wal").exists());

    let registry = registry(&database);
    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    assert_eq!(cold.sources.len(), 1);
    assert!(cold.sources[0].frontier().is_none());

    writer
        .execute_batch("pragma wal_checkpoint(truncate);")
        .unwrap();
    let noop = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);

    let verified = VerifiedIndex::open(&index).unwrap();
    let source = verified.manifest().sources[0]
        .observation()
        .source()
        .clone();
    let events = verified.source_event_page(&source, None, 10).unwrap().items;
    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests).unwrap();
    let resolver = registry.resolver_registry();
    let mut bodies = resolver
        .hydrate_batch(&batch)
        .unwrap()
        .into_records()
        .into_iter()
        .map(|record| String::from_utf8(record.provider_bytes).unwrap())
        .collect::<Vec<_>>();
    bodies.sort();
    assert_eq!(bodies, ["first logical body", "second logical body"]);

    writer
        .execute(
            "update agent_tasks set task = ?1 where task_id = 'task-1'",
            [text_task("task-1", "message-1", "replacement body")],
        )
        .unwrap();
    let replacement = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_ne!(replacement.sources, noop.sources);
    assert_eq!(replacement.commit.indexed_documents, 2);
    assert_eq!(
        resolver.hydrate_batch(&batch).unwrap_err().kind,
        HydrationFailureKind::StaleRecordEvidence
    );

    drop(writer);
    for path in [
        sidecar(&database, "-wal"),
        sidecar(&database, "-shm"),
        database.clone(),
    ] {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    let deleted = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
}

#[test]
fn warp_unavailable_and_commit_mutation_do_not_publish() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("warp.sqlite");
    let index = temp.path().join("index");
    let writer = create_source(&database);
    insert_task(&writer, "task", "message", "baseline");
    let registry = registry(&database);
    let baseline = refresh_source_backed_generation(&index, &registry, options()).unwrap();

    writer.execute_batch("begin immediate").unwrap();
    writer
        .execute(
            "update agent_tasks set last_modified_at = '2026-07-24 12:01:00'
             where task_id = 'task'",
            [],
        )
        .unwrap();
    assert!(sidecar(&database, "-journal").exists());
    assert!(refresh_source_backed_generation(&index, &registry, options()).is_err());
    writer.execute_batch("rollback").unwrap();
    assert_eq!(
        VerifiedIndex::open(&index).unwrap().generation_id(),
        baseline.commit.generation_id
    );

    writer
        .query_row("pragma journal_mode = wal", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    let mut mutated = false;
    let result =
        refresh_source_backed_generation_with_progress(&index, &registry, options(), |progress| {
            if progress.phase == "verifying" {
                writer
                    .execute(
                        "update agent_tasks
                         set last_modified_at = '2026-07-24 12:02:00'
                         where task_id = 'task'",
                        [],
                    )
                    .unwrap();
                mutated = true;
            }
            Ok(())
        });
    assert!(mutated);
    assert!(result.is_err());
    assert_eq!(
        VerifiedIndex::open(&index).unwrap().generation_id(),
        baseline.commit.generation_id
    );
}
