use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, SourceAnchor};
use ctx_history_index::WriterOptions;
use rusqlite::Connection;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_progress,
        SourceBackedProviderRegistry,
    },
    provider_sources::provider_source_for_path,
    register_hermes_explicit_source_backed_route,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("replacement.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("HERMES_SOURCE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("native.complete_text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

fn provider_family_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
    [path.to_path_buf(), path.with_extension("db-wal")]
        .into_iter()
        .filter(|member| member.exists())
        .map(|member| {
            (
                member.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(member).unwrap(),
            )
        })
        .collect()
}

fn provider_directory_names(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn projected_event_bodies(
    candidate: &HermesSourceCandidate,
    snapshot: &SqliteSourceReadSnapshot,
) -> Vec<String> {
    let mut bodies = Vec::new();
    project_hermes_snapshot(candidate, snapshot.connection().unwrap(), &mut |page| {
        for record in page.records {
            if let HermesSourceBackedRecord::Event(event) = record {
                bodies.push(event.content.normalized_body.unwrap_or_default());
            }
        }
        Ok(())
    })
    .unwrap();
    bodies
}

#[test]
fn online_backup_stays_stable_across_later_wal_commit_and_next_open_sees_it() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let path = temp.path().join("profile/state.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null
             );
             create table messages (
                 id integer primary key autoincrement,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );
             insert into sessions values ('session-1', 'acp', null, 1782259200.0);
             insert into messages (session_id, role, content, timestamp)
                 values ('session-1', 'assistant', 'admitted message', 1782259201.0);",
        )
        .unwrap();
    let candidate = HermesSourceCandidate::automatic(
        &data_root,
        provider_source_for_path(CaptureProvider::Hermes, path.clone()),
    )
    .unwrap();

    let names_before = provider_directory_names(&path);
    let bytes_before = provider_family_bytes(&path);
    let (authority, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    assert_eq!(
        authority.snapshot_counters().logical_online_backup_opens(),
        1
    );
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message"]
    );
    let terminal = snapshot.terminal_revalidator();
    snapshot.finish().unwrap();
    terminal().unwrap();
    assert_eq!(provider_directory_names(&path), names_before);
    assert_eq!(provider_family_bytes(&path), bytes_before);

    let (_authority, snapshot) = open_root_authorized_snapshot_with_hook(&data_root, &path, || {
        writer
            .execute(
                "insert into messages (session_id, role, content, timestamp)
                     values ('session-1', 'assistant', 'later message', 1782259202.0)",
                [],
            )
            .unwrap();
    })
    .unwrap();
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message"]
    );
    let terminal = snapshot.terminal_revalidator();
    snapshot.finish().unwrap();
    terminal().unwrap();

    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message", "later message"]
    );
    snapshot.finish().unwrap();
}

fn create_refresh_fixture(path: &Path, message_count: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null
             );
             create table messages (
                 id integer primary key autoincrement,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );
             insert into sessions values ('session-1', 'acp', null, 1782259200.0);",
        )
        .unwrap();
    let mut insert = connection
        .prepare(
            "insert into messages (session_id, role, content, timestamp)
             values ('session-1', 'assistant', ?1, ?2)",
        )
        .unwrap();
    for ordinal in 0..message_count {
        insert
            .execute((
                format!("fixture message {ordinal:05}"),
                1_782_259_201_f64 + ordinal as f64,
            ))
            .unwrap();
    }
}

fn fixture_registry(data_root: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, database.to_path_buf()),
        data_root,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap();
    registry
}

fn fixture_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn cold_and_noop_refresh_use_one_visible_logical_row_traversal() {
    const MESSAGES: u64 = 512;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_refresh_fixture(&database, MESSAGES);
    let registry = fixture_registry(&data_root, &database);
    let source_names = provider_directory_names(&database);
    let source_bytes = provider_family_bytes(&database);

    reset_logical_row_traversals();
    let mut progress = Vec::new();
    let cold = refresh_source_backed_generation_with_progress(
        &index_root,
        &registry,
        fixture_writer_options(),
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, MESSAGES + 1);
    assert!(cold.sources[0].frontier().is_none());
    assert!(progress.iter().any(|update| {
        update.phase == "refreshing"
            && update.current_source.as_deref() == database.to_str()
            && update.completed_records == Some(0)
            && update.completed_bytes.is_some_and(|bytes| bytes > 0)
    }));
    let route_terminal = progress
        .iter()
        .rev()
        .find(|update| update.current_source.as_deref() == database.to_str())
        .expect("terminal Hermes route progress");
    assert_eq!(route_terminal.completed_records, Some(MESSAGES));
    assert_eq!(
        route_terminal.completed_bytes,
        Some(cold.sources[0].counts().certified_bytes)
    );
    let terminal = progress.last().expect("terminal Hermes progress");
    assert_eq!(terminal.phase, "committed");
    assert!(terminal.current_source.is_none());
    assert!(terminal.completed_records.is_none());
    assert!(terminal.completed_bytes.is_none());
    assert_eq!(provider_directory_names(&database), source_names);
    assert_eq!(provider_family_bytes(&database), source_bytes);

    reset_logical_row_traversals();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(provider_directory_names(&database), source_names);
    assert_eq!(provider_family_bytes(&database), source_bytes);

    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("vacuum").unwrap();
    drop(connection);
    let vacuumed_names = provider_directory_names(&database);
    let vacuumed_bytes = provider_family_bytes(&database);
    reset_logical_row_traversals();
    let physically_changed =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        physically_changed.commit.generation_id,
        cold.commit.generation_id
    );
    assert_eq!(physically_changed.sources, cold.sources);
    assert_eq!(provider_directory_names(&database), vacuumed_names);
    assert_eq!(provider_family_bytes(&database), vacuumed_bytes);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into messages (session_id, role, content, timestamp)
             values ('session-1', 'assistant', 'changed message', 1782264200.0)",
            [],
        )
        .unwrap();
    drop(connection);
    reset_logical_row_traversals();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_ne!(changed.sources, cold.sources);
    assert_eq!(changed.sources[0].counts().complete_records, MESSAGES + 2);

    reset_logical_row_traversals();
    let changed_noop =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        changed_noop.commit.generation_id,
        changed.commit.generation_id
    );
    assert_eq!(changed_noop.sources, changed.sources);
}
