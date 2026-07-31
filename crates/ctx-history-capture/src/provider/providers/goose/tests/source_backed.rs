use std::{collections::HashSet, fs, path::Path};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use super::{create_goose_tables, insert_message, insert_session};
use crate::provider::providers::goose::source_backed::{
    goose_source_backed_work, reset_goose_source_backed_work, GooseSourceBackedWork,
};
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

fn goose_logical_identity_fixture(session_order: [&str; 2], rewrite: bool) -> String {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    for session in session_order {
        insert_session(&writer, session);
    }
    insert_message(&writer, 20, "session-b", "body b");
    insert_message(&writer, 10, "session-a", "body a");
    if rewrite {
        writer
            .execute(
                "update messages
                 set content_json = '[{\"type\":\"text\",\"text\":\"rewritten\"}]'
                 where id = 20",
                [],
            )
            .unwrap();
    }
    let receipt =
        refresh_source_backed_generation(&index, &registry(&database), options()).unwrap();
    receipt.sources[0]
        .content_digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn goose_streaming_fingerprint_preserves_legacy_logical_identity_fixture() {
    let legacy = [
        "fbd025ba7816e8cf76fca2c21140aa65cbcf88eb33d15aa74e78fbf3ed149093",
        "fbd025ba7816e8cf76fca2c21140aa65cbcf88eb33d15aa74e78fbf3ed149093",
        "7e821cfa3a9c0e3976f09074ae7b75125beec50c81b325df1c96d96716656c00",
    ];
    let streaming = [
        goose_logical_identity_fixture(["session-b", "session-a"], false),
        goose_logical_identity_fixture(["session-a", "session-b"], false),
        goose_logical_identity_fixture(["session-b", "session-a"], true),
    ];
    assert_eq!(
        streaming,
        [
            "770bce5685ad900daa2b376bbf2abda257b2b484020c19c0660eb985310d3d28",
            "770bce5685ad900daa2b376bbf2abda257b2b484020c19c0660eb985310d3d28",
            "371a68cc909e48bf130e6102e15408bd209d808a773c0c64249358047325a8d2",
        ]
    );
    for left in 0..legacy.len() {
        for right in 0..legacy.len() {
            assert_eq!(
                legacy[left] == legacy[right],
                streaming[left] == streaming[right],
                "streaming order changed the legacy logical identity partition"
            );
        }
    }
}

#[test]
fn goose_canonical_native_key_order_is_complete_and_index_backed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let writer = create_database(&database);
    for session in ["session-z", "session-a", "session-m"] {
        insert_session(&writer, session);
    }
    for native_id in [9, -4, 3] {
        insert_message(&writer, native_id, "session-a", "body");
    }

    for sql in [
        "explain query plan select id from sessions order by id limit 64",
        "explain query plan select id from messages order by id limit 64",
    ] {
        let mut statement = writer.prepare(sql).unwrap();
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
            "canonical key selector introduced a global SQL sort: {details:?}"
        );
    }

    let session_ids = writer
        .prepare("select id from sessions order by id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        session_ids,
        ["session-a", "session-m", "session-z"],
        "sessions.id must completely order every session row"
    );
    let message_ids = writer
        .prepare("select id from messages order by id")
        .unwrap()
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        message_ids,
        [-4, 3, 9],
        "messages.id must completely order every message row"
    );
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
    reset_goose_source_backed_work();
    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    assert_eq!(cold.sources.len(), 1);
    assert!(cold.sources[0].frontier().is_some());
    let cold_work = goose_source_backed_work();
    assert!(cold_work.source_bytes_copied > 0);
    assert_goose_snapshot_work(cold_work, 5, 3, 1, 5);

    writer
        .execute_batch("pragma wal_checkpoint(truncate);")
        .unwrap();
    reset_goose_source_backed_work();
    let noop = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_goose_snapshot_work(goose_source_backed_work(), 5, 3, 0, 0);

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
    reset_goose_source_backed_work();
    let mut bodies = resolver
        .hydrate_batch(&batch)
        .unwrap()
        .into_records()
        .into_iter()
        .map(|record| String::from_utf8(record.provider_bytes).unwrap())
        .collect::<Vec<_>>();
    bodies.sort();
    assert_eq!(bodies, ["first logical body", "second logical body"]);
    let hydration_work = goose_source_backed_work();
    assert_eq!(hydration_work.snapshot_opens, 1);
    assert!(hydration_work.source_bytes_copied > 0);
    assert_eq!(hydration_work.hydration_queries, 1);
    assert_eq!(hydration_work.provider_projections, 0);
    assert_eq!(hydration_work.terminal_fences, 1);
    assert!(hydration_work.terminal_revalidations >= 2);
    assert_eq!(hydration_work.active_snapshots, 0);
    assert_eq!(hydration_work.max_active_snapshots, 1);
    assert_eq!(hydration_work.logical_observations, 0);
    assert_eq!(hydration_work.logical_rows_hashed, 0);
    assert_eq!(hydration_work.peak_retained_digest_rows, 0);

    writer
        .execute(
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"replacement body\"}]'
             where id = 1",
            [],
        )
        .unwrap();
    reset_goose_source_backed_work();
    let replacement = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_ne!(replacement.sources, noop.sources);
    assert_eq!(replacement.commit.indexed_documents, 2);
    assert_goose_snapshot_work(goose_source_backed_work(), 5, 3, 1, 5);
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
    reset_goose_source_backed_work();
    let deleted = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
    assert_eq!(goose_source_backed_work(), GooseSourceBackedWork::default());
}

fn assert_goose_snapshot_work(
    work: GooseSourceBackedWork,
    logical_observation_queries: u64,
    logical_rows_hashed: u64,
    provider_projections: u64,
    projection_queries: u64,
) {
    assert_eq!(work.snapshot_opens, 1);
    assert_eq!(work.terminal_fences, 1);
    assert!(work.terminal_revalidations >= 2);
    assert_eq!(work.active_snapshots, 0);
    assert_eq!(work.max_active_snapshots, 1);
    assert_eq!(work.logical_observations, 1);
    assert_eq!(
        work.logical_observation_queries,
        logical_observation_queries
    );
    assert_eq!(work.logical_rows_hashed, logical_rows_hashed);
    assert!(work.peak_logical_page_rows <= 64);
    assert_eq!(
        work.peak_retained_digest_rows,
        u64::from(logical_rows_hashed > 0)
    );
    assert_eq!(work.provider_projections, provider_projections);
    assert_eq!(work.projection_queries, projection_queries);
    assert_eq!(work.hydration_queries, 0);
}

#[test]
fn goose_set_queries_preserve_duplicate_identity_order_and_attachments() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    insert_session(&writer, "session");
    for native_id in 1..=130 {
        insert_message(&writer, native_id, "session", &format!("body {native_id}"));
    }
    writer
        .execute("update messages set message_id = 'duplicate-id'", [])
        .unwrap();
    writer
        .execute(
            "update messages
             set content_json =
                 '[{\"type\":\"toolRequest\",\"path\":\"src/reference.rs\",\
                    \"action\":\"modify\"}]'
             where id = 130",
            [],
        )
        .unwrap();

    let registry = registry(&database);
    reset_goose_source_backed_work();
    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 130);
    assert_goose_snapshot_work(goose_source_backed_work(), 7, 131, 1, 7);

    let verified = VerifiedIndex::open(&index).unwrap();
    let source = verified.manifest().sources[0]
        .observation()
        .source()
        .clone();
    let events = verified
        .source_event_page(&source, None, 200)
        .unwrap()
        .items;
    assert_eq!(events.len(), 130);
    let mut event_sequences = events
        .iter()
        .map(|event| event.event_sequence)
        .collect::<Vec<_>>();
    event_sequences.sort_unstable();
    assert_eq!(event_sequences, (0..130).collect::<Vec<_>>());
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id)
            .collect::<HashSet<_>>()
            .len(),
        130
    );
    let attachment = events
        .iter()
        .find(|event| event.event_sequence == 129)
        .unwrap();
    assert_eq!(attachment.event_type, "tool_call");
    assert_eq!(attachment.touched_files, ["src/reference.rs"]);

    reset_goose_source_backed_work();
    let noop = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    let work = goose_source_backed_work();
    assert_eq!(work.logical_observations, 1);
    assert_eq!(work.logical_observation_queries, 7);
    assert_eq!(work.logical_rows_hashed, 131);
    assert_eq!(work.peak_logical_page_rows, 64);
    assert_eq!(work.peak_retained_digest_rows, 1);
    assert_eq!(work.provider_projections, 0);
    assert_eq!(work.projection_queries, 0);
    assert_eq!(work.snapshot_opens, 1);
    assert_eq!(work.terminal_fences, 1);
    assert!(work.terminal_revalidations >= 2);
    assert_eq!(work.active_snapshots, 0);
    assert_eq!(work.max_active_snapshots, 1);
}

#[test]
fn goose_large_noop_and_hydration_keep_fingerprint_work_page_bounded() {
    const MESSAGE_COUNT: i64 = 4_097;
    const HYDRATION_COUNT: usize = 513;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    insert_session(&writer, "large-session");
    writer.execute_batch("begin immediate").unwrap();
    for native_id in 1..=MESSAGE_COUNT {
        insert_message(
            &writer,
            native_id,
            "large-session",
            &format!("large body {native_id}"),
        );
    }
    writer.execute_batch("commit").unwrap();

    let registry = registry(&database);
    reset_goose_source_backed_work();
    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(
        cold.commit.indexed_documents,
        u64::try_from(MESSAGE_COUNT).unwrap()
    );
    assert_goose_snapshot_work(
        goose_source_backed_work(),
        69,
        u64::try_from(MESSAGE_COUNT + 1).unwrap(),
        1,
        69,
    );

    reset_goose_source_backed_work();
    let noop = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_goose_snapshot_work(
        goose_source_backed_work(),
        69,
        u64::try_from(MESSAGE_COUNT + 1).unwrap(),
        0,
        0,
    );

    let verified = VerifiedIndex::open(&index).unwrap();
    let source = verified.manifest().sources[0]
        .observation()
        .source()
        .clone();
    let events = verified
        .source_event_page(&source, None, HYDRATION_COUNT)
        .unwrap()
        .items;
    assert_eq!(events.len(), HYDRATION_COUNT);
    let requests = events
        .into_iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator).unwrap())
        .collect::<Vec<_>>();
    reset_goose_source_backed_work();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap();
    assert_eq!(hydrated.records().len(), HYDRATION_COUNT);
    let hydration_work = goose_source_backed_work();
    assert_eq!(hydration_work.snapshot_opens, 1);
    assert_eq!(hydration_work.hydration_queries, 3);
    assert_eq!(hydration_work.terminal_fences, 1);
    assert!(hydration_work.terminal_revalidations >= 2);
    assert_eq!(hydration_work.active_snapshots, 0);
    assert_eq!(hydration_work.max_active_snapshots, 1);
    assert_eq!(hydration_work.logical_observations, 0);
    assert_eq!(hydration_work.logical_rows_hashed, 0);
    assert_eq!(hydration_work.provider_projections, 0);
}

#[test]
fn goose_fails_without_complete_canonical_native_primary_keys() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = Connection::open(&database).unwrap();
    writer
        .execute_batch(
            "pragma user_version = 14;
             create table schema_version (version integer not null);
             insert into schema_version values (14);
             create table sessions (
                 id text,
                 shard text,
                 primary key (shard, id)
             );
             create table messages (
                 id integer primary key,
                 message_id text,
                 session_id text not null,
                 role text not null,
                 content_json text not null
             );
             insert into sessions values ('session', 'shard');",
        )
        .unwrap();

    let error =
        refresh_source_backed_generation(&index, &registry(&database), options()).unwrap_err();
    assert!(error
        .to_string()
        .contains("sole native primary keys messages.id and sessions.id"));
}

#[test]
fn goose_fails_when_a_session_row_has_no_bounded_canonical_key() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let index = temp.path().join("index");
    let writer = create_database(&database);
    writer
        .execute("insert into sessions (id) values (null)", [])
        .unwrap();

    let error =
        refresh_source_backed_generation(&index, &registry(&database), options()).unwrap_err();
    assert!(error
        .to_string()
        .contains("no bounded canonical native key"));
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
