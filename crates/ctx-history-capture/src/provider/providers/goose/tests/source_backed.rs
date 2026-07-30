use std::{fs, path::Path};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use super::{create_goose_tables, insert_message, insert_session};
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_progress,
        register_goose_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

fn create_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    create_goose_tables(&connection);
    connection.pragma_update(None, "user_version", 14).unwrap();
    connection
}

fn registry(path: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Goose,
        path: path.to_owned(),
        exists: true,
        source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_goose_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        crate::test_provider_sqlite_data_root(),
        path.parent().unwrap(),
        Vec::new(),
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

#[test]
fn goose_active_wal_noop_replace_delete_and_batch_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    writer
        .query_row("pragma journal_mode = wal", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    insert_session(&writer, "session");
    insert_message(&writer, 1, "session", "first logical body");
    insert_message(&writer, 2, "session", "second logical body");
    assert!(database.with_extension("db-wal").exists());

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
    assert!(events.iter().all(|event| event.agent_type == "primary"));
    let mut event_sequences = events
        .iter()
        .map(|event| event.event_sequence)
        .collect::<Vec<_>>();
    event_sequences.sort_unstable();
    assert_eq!(event_sequences, [0, 1]);
    assert!(events
        .iter()
        .all(|event| event.event_sequence <= i64::MAX as u64));
    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
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
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"replacement body\"}]'
             where id = 1",
            [],
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
        database.with_extension("db-wal"),
        database.with_extension("db-shm"),
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
fn goose_unavailable_and_commit_mutation_do_not_publish() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    insert_session(&writer, "session");
    insert_message(&writer, 1, "session", "baseline");
    let registry = registry(&database);
    let baseline = refresh_source_backed_generation(&index, &registry, options()).unwrap();

    writer.execute_batch("begin immediate").unwrap();
    writer
        .execute("update messages set role = 'assistant' where id = 1", [])
        .unwrap();
    assert!(database.with_extension("db-journal").exists());
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
                    .execute("update messages set role = 'assistant' where id = 1", [])
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

#[test]
fn goose_ignores_unrelated_sibling_creation_during_commit() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    insert_session(&writer, "session");
    insert_message(&writer, 1, "session", "stable logical body");
    drop(writer);
    let registry = registry(&database);

    let mut created_sibling = false;
    let published =
        refresh_source_backed_generation_with_progress(&index, &registry, options(), |progress| {
            if progress.phase == "verifying" && !created_sibling {
                fs::write(temp.path().join("unrelated-sibling"), b"unrelated").unwrap();
                created_sibling = true;
            }
            Ok(())
        })
        .unwrap();

    assert!(created_sibling);
    assert_eq!(published.commit.indexed_documents, 1);
}
