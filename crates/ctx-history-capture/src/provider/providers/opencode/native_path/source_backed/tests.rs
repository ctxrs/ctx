#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::process::Command;
use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{CaptureProvider, EventRole, EventType, TypedKey};
#[cfg(unix)]
use ctx_history_core::{
    RepositoryCandidateKind, RepositoryEvidenceKind, RepositoryFileObservationKind,
};
use ctx_history_index::{CoreSourceEventPage, VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_progress,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

fn write_current_schema(
    path: &Path,
    directory: &Path,
    part_data: &serde_json::Value,
) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 project_id text not null,
                 workspace_id text,
                 parent_id text,
                 slug text not null,
                 directory text not null,
                 title text not null,
                 version text not null,
                 agent text,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
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
             create index part_session_idx on part(session_id);
             create table event (
                 id text primary key,
                 aggregate_id text not null,
                 seq integer not null,
                 type text not null,
                 data text not null
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into session values (
                 'current-session', 'project-1', null, null, 'current-session', ?1,
                 'Current session', '1.18.11', 'build', 1782259200000, 1782259202000
             )",
            [directory.to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'current-user', 'current-session', 1782259200000, 1782259200000, ?1
             )",
            [json!({
                "role": "user",
                "time": {"created": 1782259200000_i64},
                "text": "current OpenCode prompt"
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'current-assistant', 'current-session', 1782259201000, 1782259202000, ?1
             )",
            [json!({
                "role": "assistant",
                "time": {"created": 1782259201000_i64},
                "modelID": "gpt-test"
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'current-part', 'current-assistant', 'current-session',
                 1782259201000, 1782259201000, ?1
             )",
            [part_data.to_string()],
        )
        .unwrap();
    for sequence in 0..9_i64 {
        connection
            .execute(
                "insert into event values (?1, 'current-session', ?2, 'history.updated', '{}')",
                params![format!("event-{sequence}"), sequence],
            )
            .unwrap();
    }
    connection
}

fn scan_current_schema(
    path: &Path,
) -> (
    OpenCodeLogicalObservation,
    OpenCodeSourceBackedScan,
    Vec<CoreRecord>,
) {
    scan_current_schema_result(
        path,
        crate::test_provider_sqlite_data_root(),
        OPENCODE_FALLBACK_SCRATCH_MAX_BYTES,
    )
    .unwrap()
}

fn scan_current_schema_result(
    path: &Path,
    data_root: &Path,
    scratch_limit: u64,
) -> OpenCodeSourceBackedResult<(
    OpenCodeLogicalObservation,
    OpenCodeSourceBackedScan,
    Vec<CoreRecord>,
)> {
    let authorized = open_root_authorized_snapshot_retained(data_root, path)?;
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection()?,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )?;
    let mut records = Vec::new();
    let scan = scan_pinned_source_with_scratch_limit(
        path,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        scratch_limit,
        &mut |output| {
            if let OpenCodeScanOutput::Document(record) = output {
                records.push(record);
            }
            Ok(())
        },
    )?;
    Ok((observation, scan, records))
}

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("projection.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let body = if searchable.is_empty()"));
    assert!(production.contains("RepositoryAttributor"));
    assert!(production.contains("apply_annotation"));
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

#[test]
fn metadata_only_session_message_yields_message_part_events_through_production_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_metadata_and_message_part_fixture(&database);

    let page = project_fixture(&database, temp.path());

    assert!(page.terminal);
    assert_eq!(
        page.source.schema_variant(),
        "opencode-family-message_part-v1"
    );
    assert_eq!(page.items.len(), 2);
    for item in &page.items {
        assert_eq!(
            item.event.event_type.parse::<EventType>().unwrap(),
            EventType::Message
        );
        assert_eq!(item.event.role, item.core_record.role);
    }
    let mut projected = page
        .items
        .iter()
        .map(|item| {
            (
                item.core_record.content.normalized_body.clone().unwrap(),
                item.event.role.clone().unwrap(),
                item.event.native_event_id.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    projected.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        projected,
        vec![
            (
                "assistant conversation from part".to_owned(),
                EventRole::Assistant.as_str().to_owned(),
                TypedKey::Utf8("part-assistant".to_owned()),
            ),
            (
                "user conversation from part".to_owned(),
                EventRole::User.as_str().to_owned(),
                TypedKey::Utf8("part-user".to_owned()),
            ),
        ]
    );
}

#[test]
fn agent_switched_current_event_has_canonical_unknown_role_through_production_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_agent_switched_fixture(&database);

    let page = project_fixture(&database, temp.path());

    assert!(page.terminal);
    assert_eq!(
        page.source.schema_variant(),
        "opencode-family-session_message_seq-v1"
    );
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(
        item.event.event_type.parse::<EventType>().unwrap(),
        EventType::Notice
    );
    assert_eq!(
        item.event
            .role
            .as_deref()
            .unwrap()
            .parse::<EventRole>()
            .unwrap(),
        EventRole::Unknown
    );
    assert_eq!(
        item.core_record.role.as_deref(),
        Some(EventRole::Unknown.as_str())
    );
    assert_eq!(
        item.core_record.content.normalized_body.as_deref(),
        Some("agent switched from build to plan")
    );
    assert_eq!(
        item.event.native_event_id,
        Some(TypedKey::Utf8("metadata-agent".to_owned()))
    );
}

fn project_fixture(database: &Path, root: &Path) -> CoreSourceEventPage {
    let data_root = root.join("data-root");
    let index_root = root.join("index");
    let source = provider_source_for_path(CaptureProvider::OpenCode, database.to_path_buf());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &data_root,
    )
    .unwrap();
    let refresh = refresh_source_backed_generation(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(refresh.sources.len(), 1);
    let source = refresh.sources[0].observation().source().clone();
    VerifiedIndex::open(index_root)
        .unwrap()
        .core_source_event_page(&source, None, 16)
        .unwrap()
}

fn refresh_fixture_with_work(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> super::adapter::OpenCodeSqliteWorkCounters {
    let _ = super::adapter::take_last_work_counters();
    refresh_source_backed_generation(
        index_root,
        registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    super::adapter::take_last_work_counters().unwrap()
}

fn create_indexed_synthetic_fixture(path: &Path, rows: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
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
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create unique index session_message_session_seq_idx
                 on session_message(session_id, seq);
             insert into session values (
                 'session-1', null, '/tmp/project', 'main', 'build', 0, 0
             );",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for sequence in 0..rows {
        let body = json!({
            "role": "user",
            "time": {"created": sequence},
            "text": format!("synthetic OpenCode event {sequence}")
        })
        .to_string();
        transaction
            .execute(
                "insert into session_message values (?1, 'session-1', 'message', ?2, ?2, ?2, ?3)",
                params![format!("event-{sequence:08}"), sequence, body],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

#[test]
fn rejection_heavy_scan_reports_authoritative_completed_bytes_before_finishing() {
    const ROWS: i64 = 128;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "update session_message set data = 'not-json' where seq < ?1",
            [ROWS - 1],
        )
        .unwrap();
    drop(connection);

    let authorized =
        open_root_authorized_snapshot_retained(crate::test_provider_sqlite_data_root(), &database)
            .unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let mut completed_bytes = Vec::new();
    let mut accepted = 0_u64;
    let scan = scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |output| {
            match output {
                OpenCodeScanOutput::Begin(_) => {}
                OpenCodeScanOutput::CompletedBytes(bytes) => completed_bytes.push(bytes),
                OpenCodeScanOutput::Document(_) => accepted = accepted.saturating_add(1),
            }
            Ok(())
        },
    )
    .unwrap();

    let counts = scan.certificate.counts();
    assert_eq!(counts.complete_records, ROWS as u64);
    assert_eq!(counts.retained_records, 1);
    assert_eq!(counts.rejected_records, (ROWS - 1) as u64);
    assert_eq!(accepted, 1);
    assert_eq!(completed_bytes.len(), ROWS as usize);
    assert_eq!(completed_bytes.iter().sum::<u64>(), counts.certified_bytes);
}

#[test]
fn rejection_heavy_production_refresh_advances_bytes_and_clears_them_terminally() {
    const ROWS: i64 = 128;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "update session_message set data = 'not-json' where seq < ?1",
            [ROWS - 1],
        )
        .unwrap();
    drop(connection);

    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::OpenCode, database.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &data_root,
    )
    .unwrap();
    let mut progress = Vec::new();
    let refresh = refresh_source_backed_generation_with_progress(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();

    let counts = refresh.sources[0].counts();
    assert_eq!(counts.retained_records, 1);
    assert_eq!(counts.rejected_records, (ROWS - 1) as u64);
    assert!(progress.iter().any(|update| {
        update.phase == "refreshing"
            && update.current_source.as_deref() == database.to_str()
            && update.completed_records == Some(0)
            && update.completed_bytes.is_some_and(|bytes| bytes > 0)
    }));
    let terminal = progress.last().expect("terminal refresh progress");
    assert_eq!(terminal.phase, "committed");
    assert!(terminal.current_source.is_none());
    assert!(terminal.completed_records.is_none());
    assert!(terminal.completed_bytes.is_none());
}

fn create_indexed_message_part_fixture(path: &Path, rows: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
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
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
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
             create index part_message_time_id_idx
                 on part(message_id, time_created, id);
             create index part_session_idx on part(session_id);
             insert into session values (
                 'session-1', null, '/tmp/project', 'main', 'build', 0, 0
             );",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for sequence in 0..rows {
        let message_id = format!("message-{sequence:08}");
        transaction
            .execute(
                "insert into message values (?1, 'session-1', ?2, ?2, ?3)",
                params![
                    message_id,
                    sequence,
                    json!({"role": "user", "time": {"created": sequence}}).to_string()
                ],
            )
            .unwrap();
        transaction
            .execute(
                "insert into part values (?1, ?2, 'session-1', ?3, ?3, ?4)",
                params![
                    format!("part-{sequence:08}"),
                    message_id,
                    sequence,
                    json!({
                        "type": "text",
                        "text": format!("synthetic current OpenCode part {sequence}")
                    })
                    .to_string()
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn create_multisession_message_part_fixture(path: &Path, sessions: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
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
             create index part_message_time_id_idx
                 on part(message_id, time_created, id);
             create index part_session_idx on part(session_id);",
        )
        .unwrap();
    let metadata_padding = "m".repeat(4 * 1024);
    let payload_padding = "p".repeat(4 * 1024);
    let transaction = connection.transaction().unwrap();
    for sequence in 0..sessions {
        let session_id = format!("session-{sequence:08}");
        let message_id = format!("message-{sequence:08}");
        let parent_id = (sequence % 17 != 0).then(|| format!("session-{:08}", sequence - 1));
        transaction
            .execute(
                "insert into session values (?1, ?2, '/tmp/project', 'main', ?3, ?4, ?4)",
                params![
                    session_id,
                    parent_id,
                    format!("agent-{sequence}-{metadata_padding}"),
                    sequence
                ],
            )
            .unwrap();
        transaction
            .execute(
                "insert into message values (?1, ?2, ?3, ?3, ?4)",
                params![
                    message_id,
                    session_id,
                    sequence,
                    json!({"role": "user", "time": {"created": sequence}}).to_string()
                ],
            )
            .unwrap();
        transaction
            .execute(
                "insert into part values (?1, ?2, ?3, ?4, ?4, ?5)",
                params![
                    format!("part-{sequence:08}"),
                    message_id,
                    session_id,
                    sequence,
                    json!({
                        "type": "text",
                        "text": format!("event-{sequence}-{payload_padding}")
                    })
                    .to_string()
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn drop_message_part_stream_indexes(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "drop index message_session_time_created_id_idx;
             drop index part_message_id_id_idx;
             drop index part_message_time_id_idx;
             drop index part_session_idx;",
        )
        .unwrap();
}

fn directory_file_bytes(path: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

#[cfg(unix)]
fn directory_write_stamp(path: &Path) -> (i64, i64, i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[test]
fn admitted_opencode_backup_stays_stable_across_later_wal_commit_and_next_open_advances() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "create table session (
                 id text primary key,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create unique index session_message_session_seq_idx
                 on session_message(session_id, seq);
             insert into session values ('session-1', 1, 1);",
        )
        .unwrap();
    writer
        .execute(
            "insert into session_message values (
                 'message-1', 'session-1', 'message', 1, 1, 1, ?1
             )",
            [json!({"role": "user", "text": "admitted OpenCode message"}).to_string()],
        )
        .unwrap();
    let data_root = temp.path().join("data-root");
    let authorized =
        open_root_authorized_snapshot_retained_with_hook(&data_root, &database, || {
            writer
                .execute(
                    "insert into session_message values (
                         'message-2', 'session-1', 'message', 2, 2, 2, ?1
                     )",
                    [json!({"role": "assistant", "text": "later OpenCode message"}).to_string()],
                )
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        authorized
            .sqlite_authority
            .snapshot_counters()
            .logical_online_backup_opens(),
        1
    );
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let terminal = authorized.sqlite_snapshot.terminal_revalidator();
    let OpenCodeAuthorizedSnapshot {
        source_root,
        sqlite_authority,
        sqlite_snapshot,
    } = authorized;
    let mut admitted = Vec::new();
    scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        sqlite_snapshot,
        &mut |output| {
            if let OpenCodeScanOutput::Document(record) = output {
                admitted.push(record.content.normalized_body.unwrap_or_default());
            }
            Ok(())
        },
    )
    .unwrap();
    terminal().unwrap();
    assert_eq!(admitted, vec!["admitted OpenCode message"]);
    drop((source_root, sqlite_authority));

    let (_, _, refreshed) = scan_current_schema(&database);
    assert_eq!(
        refreshed
            .into_iter()
            .map(|record| record.content.normalized_body.unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["admitted OpenCode message", "later OpenCode message"]
    );
}

#[test]
fn indexed_synthetic_cold_and_changed_use_one_snapshot_and_one_logical_row_traversal() {
    const ROWS: u64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS as i64);

    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    let mut sql = source_backed_event_sql(&schema);
    sql.push_str(source_backed_event_order_sql(&schema));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(plan.iter().any(|step| {
        step.contains("session_message_session_seq_idx")
            || step.contains("sqlite_autoindex_session_message")
    }));
    assert!(
        plan.iter().all(|step| !step.contains("USE TEMP B-TREE")),
        "unexpected query plan: {plan:?}"
    );
    drop(connection);

    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::OpenCode, database.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &data_root,
    )
    .unwrap();

    let cold = refresh_fixture_with_work(&index_root, &registry);
    assert_eq!(cold.snapshot_opens, 1);
    assert_eq!(cold.logical_online_backup_opens, 1);
    assert_eq!(cold.schema_probe_passes, 1);
    assert_eq!(cold.schema_event_validation_traversals, 3);
    assert_eq!(cold.logical_fingerprint_passes, 0);
    assert_eq!(cold.logical_row_traversals, 1);
    assert_eq!(cold.projection_passes, 1);
    assert_eq!(cold.logical_rows_projected, ROWS);
    assert_eq!(cold.documents_staged, ROWS);

    let unchanged = refresh_fixture_with_work(&index_root, &registry);
    assert_eq!(unchanged.snapshot_opens, 1);
    assert_eq!(unchanged.exact_replays, 1);
    assert_eq!(unchanged.logical_row_traversals, 0);
    assert_eq!(unchanged.logical_rows_projected, 0);

    let connection = Connection::open(&database).unwrap();
    let sequence = ROWS as i64;
    connection
        .execute(
            "insert into session_message values (?1, 'session-1', 'message', ?2, ?2, ?2, ?3)",
            params![
                format!("event-{sequence:08}"),
                sequence,
                json!({
                    "role": "assistant",
                    "time": {"created": sequence},
                    "text": "changed synthetic event"
                })
                .to_string()
            ],
        )
        .unwrap();
    drop(connection);

    let changed = refresh_fixture_with_work(&index_root, &registry);
    assert_eq!(changed.snapshot_opens, 1);
    assert_eq!(changed.logical_online_backup_opens, 1);
    assert_eq!(changed.logical_fingerprint_passes, 0);
    assert_eq!(changed.logical_row_traversals, 1);
    assert_eq!(changed.projection_passes, 1);
    assert_eq!(changed.logical_rows_projected, ROWS + 1);
    assert_eq!(changed.documents_staged, ROWS + 1);
}

#[test]
fn indexed_message_part_cold_scan_preserves_chronology_with_bounded_message_sort() {
    const ROWS: u64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_message_part_fixture(&database, ROWS as i64);

    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert_eq!(schema.family, OpenCodeNativeSchemaFamily::MessagePart);
    assert!(schema.message_part_indexed_streaming);
    let message_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_indexed_message_ids_sql()
        ))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        message_plan
            .iter()
            .any(|step| step.contains("message_session_time_created_id_idx")),
        "indexed current-schema query did not use the message stream index: {message_plan:?}"
    );
    let part_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_indexed_part_rowids_sql()
        ))
        .unwrap()
        .query_map([rusqlite::types::Null], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        part_plan
            .iter()
            .any(|step| step.contains("part_message_time_id_idx")),
        "indexed current-schema query did not use the part stream index: {part_plan:?}"
    );
    assert!(
        message_plan
            .iter()
            .chain(&part_plan)
            .all(|step| !step.contains("USE TEMP B-TREE")),
        "indexed current-schema key stream used SQLite temporary sorting: {message_plan:?} {part_plan:?}"
    );
    assert!(
        message_plan
            .iter()
            .chain(&part_plan)
            .all(|step| { !step.contains("CORRELATED") && !step.contains("VIRTUAL TABLE") }),
        "indexed current-schema query repeated JSON-tree traversal in SQLite: {message_plan:?} {part_plan:?}"
    );
    drop(connection);

    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::OpenCode, database);
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &data_root,
    )
    .unwrap();

    let cold = refresh_fixture_with_work(&index_root, &registry);
    assert_eq!(cold.snapshot_opens, 1);
    assert_eq!(cold.logical_online_backup_opens, 1);
    assert_eq!(cold.schema_probe_passes, 1);
    assert_eq!(cold.schema_event_validation_traversals, 2);
    assert_eq!(cold.logical_fingerprint_passes, 0);
    assert_eq!(cold.logical_row_traversals, 1);
    assert_eq!(cold.projection_passes, 1);
    assert_eq!(cold.logical_rows_projected, ROWS);
    assert_eq!(cold.documents_staged, ROWS);
    assert_eq!(cold.max_buffered_documents, 1);
}

#[test]
fn indexed_message_part_partial_sort_routes_to_private_bounded_scratch() {
    const PARTS: u64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_message_part_fixture(&database, 0);
    let payload_padding = "p".repeat(4 * 1024);
    let mut connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into message values ('message-1', 'session-1', 0, 0, ?1)",
            [json!({"role": "user", "time": {"created": 0}}).to_string()],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for sequence in 0..PARTS {
        let time = i64::try_from(PARTS - sequence).unwrap();
        transaction
            .execute(
                "insert into part values (?1, 'message-1', 'session-1', ?2, ?2, ?3)",
                params![
                    format!("part-{sequence:08}"),
                    time,
                    json!({
                        "type": "text",
                        "text": format!("event-{sequence}-{payload_padding}")
                    })
                    .to_string()
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);

    let (_, direct_scan, direct_records) = scan_current_schema(&database);
    assert!(!direct_scan.bounds.fallback_disk_sort);
    assert_eq!(direct_scan.bounds.max_buffered_payload_rows, 0);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute("drop index part_message_time_id_idx", [])
        .unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(schema.message_part_indexed_streaming);
    let plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_indexed_part_rowids_sql()
        ))
        .unwrap()
        .query_map([rusqlite::types::Null], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("USE TEMP B-TREE")),
        "partial-index fixture did not require SQLite temporary sorting: {plan:?}"
    );
    drop(connection);

    let (_, external_scan, external_records) = scan_current_schema(&database);
    assert_eq!(external_records, direct_records);
    assert_eq!(external_scan.source, direct_scan.source);
    assert_eq!(external_scan.certificate, direct_scan.certificate);
    assert!(external_scan.bounds.fallback_disk_sort);
    assert_eq!(external_scan.bounds.fallback_sort_rows, PARTS);
    assert_eq!(external_scan.bounds.fallback_payload_hydrations, PARTS);
    assert_eq!(external_scan.bounds.max_buffered_payload_rows, 1);
    assert!(external_scan.bounds.fallback_scratch_bytes > 0);
}

#[test]
fn multisession_missing_index_fallback_is_equivalent_bounded_and_read_only() {
    const SESSIONS: u64 = 2_048;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_multisession_message_part_fixture(&database, SESSIONS as i64);

    let (_, indexed_scan, indexed_records) = scan_current_schema(&database);
    assert_eq!(indexed_scan.bounds.session_rows_scanned, SESSIONS);
    assert_eq!(indexed_scan.bounds.session_metadata_loads, SESSIONS);
    assert_eq!(indexed_scan.bounds.max_buffered_session_metadata, 1);
    assert_eq!(indexed_scan.bounds.max_session_ancestry_depth, 16);
    assert_eq!(indexed_scan.bounds.fallback_payload_hydrations, 0);
    assert_eq!(indexed_scan.bounds.max_buffered_payload_rows, 0);
    assert!(!indexed_scan.bounds.fallback_disk_sort);

    drop_message_part_stream_indexes(&database);
    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let fallback_schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!fallback_schema.message_part_indexed_streaming);
    let mut fallback_sql = source_backed_event_sql(&fallback_schema);
    fallback_sql.push_str(source_backed_event_order_sql(&fallback_schema));
    let fallback_plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {fallback_sql}"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        fallback_plan
            .iter()
            .any(|step| step.contains("USE TEMP B-TREE FOR ORDER BY")),
        "missing-index fixture did not exercise the fallback sorter: {fallback_plan:?}"
    );
    let sort_key_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_fallback_sort_key_sql(&fallback_schema)
        ))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        sort_key_plan
            .iter()
            .all(|step| !step.contains("USE TEMP B-TREE")),
        "replacement key scan must not retain the ambient SQLite sorter: {sort_key_plan:?}"
    );
    drop(connection);

    let before_database = fs::read(&database).unwrap();
    let mut before_siblings = fs::read_dir(database.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    before_siblings.sort();
    let (_, fallback_scan, fallback_records) = scan_current_schema(&database);
    let mut after_siblings = fs::read_dir(database.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    after_siblings.sort();

    assert_eq!(fallback_records, indexed_records);
    assert_eq!(fallback_scan.source, indexed_scan.source);
    assert_eq!(fallback_scan.certificate, indexed_scan.certificate);
    assert_eq!(
        fallback_scan.certificate.counts(),
        indexed_scan.certificate.counts()
    );
    assert_eq!(
        fallback_scan.certificate.content_digest(),
        indexed_scan.certificate.content_digest()
    );
    assert_eq!(fallback_scan.bounds.session_rows_scanned, SESSIONS);
    assert_eq!(fallback_scan.bounds.session_metadata_loads, SESSIONS);
    assert_eq!(fallback_scan.bounds.max_buffered_session_metadata, 1);
    assert_eq!(fallback_scan.bounds.max_session_ancestry_depth, 16);
    assert_eq!(fallback_scan.bounds.fallback_payload_hydrations, SESSIONS);
    assert_eq!(fallback_scan.bounds.max_buffered_payload_rows, 1);
    assert!(fallback_scan.bounds.fallback_disk_sort);
    assert_eq!(fallback_scan.bounds.fallback_sort_rows, SESSIONS);
    assert!(fallback_scan.bounds.fallback_scratch_bytes > 0);
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(after_siblings, before_siblings);
}

#[cfg(unix)]
#[test]
fn fallback_external_sort_ignores_ambient_sqlite_tmpdir_without_transient_provider_writes() {
    const CHILD_MARKER: &str = "CTX_TEST_OPENCODE_AMBIENT_TMP_CHILD";
    const CHILD_ROOT: &str = "CTX_TEST_OPENCODE_AMBIENT_TMP_ROOT";
    if std::env::var_os(CHILD_MARKER).is_some() {
        const SESSIONS: u64 = 8_192;
        let root = std::path::PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let provider = root.join("provider");
        let database = provider.join("opencode.sqlite");
        let data_root = root.join("ctx-data");
        create_multisession_message_part_fixture(&database, SESSIONS as i64);
        drop_message_part_stream_indexes(&database);
        let before_bytes = directory_file_bytes(&provider);
        let before_stamp = directory_write_stamp(&provider);

        let (_, scan, records) =
            scan_current_schema_result(&database, &data_root, OPENCODE_FALLBACK_SCRATCH_MAX_BYTES)
                .unwrap();

        assert_eq!(records.len() as u64, SESSIONS);
        assert_eq!(scan.bounds.fallback_sort_rows, SESSIONS);
        assert!(scan.bounds.fallback_disk_sort);
        assert!(
            scan.bounds.fallback_scratch_bytes > 512 * 1024,
            "fixture must exceed the fixed scratch page cache and exercise disk-backed ordering"
        );
        assert_eq!(directory_file_bytes(&provider), before_bytes);
        assert_eq!(directory_write_stamp(&provider), before_stamp);
        let scratch_root = data_root.join("tmp/provider-sqlite-scratch");
        assert_eq!(fs::read_dir(scratch_root).unwrap().count(), 0);
        return;
    }

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    fs::create_dir_all(&provider).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(
            "fallback_external_sort_ignores_ambient_sqlite_tmpdir_without_transient_provider_writes",
        )
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CHILD_ROOT, temp.path())
        .env("SQLITE_TMPDIR", &provider)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated SQLITE_TMPDIR proof failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fallback_scratch_enospc_is_typed_and_preserves_the_provider() {
    const SESSIONS: u64 = 2_048;
    const SCRATCH_LIMIT: u64 = 64 * 1024;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("opencode.sqlite");
    let data_root = temp.path().join("ctx-data");
    create_multisession_message_part_fixture(&database, SESSIONS as i64);
    drop_message_part_stream_indexes(&database);
    let before = directory_file_bytes(&provider);

    let error = scan_current_schema_result(&database, &data_root, SCRATCH_LIMIT).unwrap_err();

    match error {
        OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Sqlite {
            operation,
            source: rusqlite::Error::SqliteFailure(error, _),
        }) => {
            assert_eq!(operation, "writing the private OpenCode ordering index");
            assert_eq!(error.code, rusqlite::ErrorCode::DiskFull);
        }
        other => panic!("unexpected bounded-scratch error: {other:?}"),
    }
    assert_eq!(directory_file_bytes(&provider), before);
    let scratch_root = data_root.join("tmp/provider-sqlite-scratch");
    assert_eq!(fs::read_dir(scratch_root).unwrap().count(), 0);
}

#[test]
fn unwritable_fallback_scratch_root_is_typed_and_preserves_the_provider() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("opencode.sqlite");
    let data_root = temp.path().join("ctx-data");
    create_multisession_message_part_fixture(&database, 64);
    drop_message_part_stream_indexes(&database);
    let before = directory_file_bytes(&provider);
    let authorized = open_root_authorized_snapshot_retained(&data_root, &database).unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    fs::write(
        data_root.join("tmp/provider-sqlite-scratch"),
        b"not a directory",
    )
    .unwrap();

    let error = scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |_| Ok(()),
    )
    .unwrap_err();

    match error {
        OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Io {
            operation, ..
        }) => assert_eq!(
            operation,
            "creating the private provider SQLite scratch root"
        ),
        other => panic!("unexpected unavailable-scratch error: {other:?}"),
    }
    assert_eq!(directory_file_bytes(&provider), before);
}

#[test]
fn partial_or_incompatible_part_indexes_do_not_enable_unqualified_streaming() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_message_part_fixture(&database, 8);
    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;

    connection
        .execute_batch(
            "drop index part_message_id_id_idx;
             create index partial_part_message_id_id_idx on part(message_id, id)
                 where message_id <> '';",
        )
        .unwrap();
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!schema.message_part_indexed_streaming);

    connection
        .execute_batch(
            "drop index partial_part_message_id_id_idx;
             create index nocase_part_message_id_id_idx
                 on part(message_id collate nocase, id);",
        )
        .unwrap();
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!schema.message_part_indexed_streaming);

    connection
        .execute_batch(
            "drop index nocase_part_message_id_id_idx;
             create index descending_part_message_id_id_idx
                 on part(message_id desc, id);",
        )
        .unwrap();
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!schema.message_part_indexed_streaming);
}

#[test]
fn message_part_v5_order_is_independent_of_index_presence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.sqlite");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "later assistant part"}),
    );
    connection
        .execute(
            "insert into part values (
                 'current-user-part', 'current-user', 'current-session',
                 1782259200000, 1782259200000, ?1
             )",
            [json!({"type": "text", "text": "earlier user part"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'part-z-early', 'current-assistant', 'current-session',
                 1782259201001, 1782259201001, ?1
             )",
            [json!({"type": "text", "text": "earlier nonlexical part"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'part-a-late', 'current-assistant', 'current-session',
                 1782259201002, 1782259201002, ?1
             )",
            [json!({"type": "text", "text": "later nonlexical part"}).to_string()],
        )
        .unwrap();
    drop(connection);

    let (_, indexed_scan, indexed) = scan_current_schema(&database);
    let indexed_order = indexed
        .iter()
        .map(|record| {
            (
                record.event_sequence,
                record.content.normalized_body.clone().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        indexed_order,
        vec![
            (0, "earlier user part".to_owned()),
            (1, "later assistant part".to_owned()),
            (2, "earlier nonlexical part".to_owned()),
            (3, "later nonlexical part".to_owned()),
        ]
    );

    let connection = Connection::open(&database).unwrap();
    connection
        .execute("drop index part_message_id_id_idx", [])
        .unwrap();
    drop(connection);
    let (_, unindexed_scan, unindexed) = scan_current_schema(&database);
    let unindexed_order = unindexed
        .iter()
        .map(|record| {
            (
                record.event_sequence,
                record.content.normalized_body.clone().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(unindexed_order, indexed_order);
    assert_eq!(unindexed_scan.source, indexed_scan.source);
    assert_eq!(
        unindexed_scan.certificate.counts(),
        indexed_scan.certificate.counts()
    );
    assert_eq!(
        unindexed_scan.certificate.content_digest(),
        indexed_scan.certificate.content_digest()
    );
}

#[test]
fn relationship_mismatch_disables_indexed_streaming_without_changing_generation_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.sqlite");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "assistant part"}),
    );
    connection
        .execute(
            "insert into session values (
                 'other-session', 'project-1', null, null, 'other-session', ?1,
                 'Other session', '1.18.11', 'build', 1782259200000, 1782259202000
             )",
            [temp.path().to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "update part set session_id = 'other-session' where id = 'current-part'",
            [],
        )
        .unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!schema.message_part_indexed_streaming);
    drop(connection);

    let (_, indexed_scan, indexed_records) = scan_current_schema(&database);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("drop index part_message_id_id_idx", [])
        .unwrap();
    drop(connection);
    let (_, unindexed_scan, unindexed_records) = scan_current_schema(&database);

    assert_eq!(unindexed_records, indexed_records);
    assert_eq!(
        unindexed_scan.certificate.counts(),
        indexed_scan.certificate.counts()
    );
    assert_eq!(
        unindexed_scan.certificate.content_digest(),
        indexed_scan.certificate.content_digest()
    );
}

#[test]
fn unsafe_unreferenced_message_fails_with_complete_or_partial_part_index() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_message_part_fixture(&database, 8);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into message values (
                 'unreferenced', 'session-1', 'not-an-integer', 99, '{}'
             )",
            [],
        )
        .unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let indexed_error = OpenCodeNativeSchema::probe(&connection, dialect).unwrap_err();
    assert!(indexed_error
        .to_string()
        .contains("message parent identity/order rows are unsafe"));

    connection
        .execute_batch(
            "drop index part_message_id_id_idx;
             create index partial_part_message_id_id_idx on part(message_id, id)
                 where message_id <> '';",
        )
        .unwrap();
    let partial_error = OpenCodeNativeSchema::probe(&connection, dialect).unwrap_err();
    assert!(partial_error
        .to_string()
        .contains("message parent identity/order rows are unsafe"));
}

#[test]
fn kilo_and_mimocode_opt_into_one_logical_online_backup_and_streaming_pass() {
    for provider in [CaptureProvider::Kilo, CaptureProvider::MiMoCode] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("source/history.sqlite");
        create_indexed_synthetic_fixture(&database, 32);
        let source = provider_source_for_path(provider, database);
        let mut registry = SourceBackedProviderRegistry::new();
        register_source_backed_route(
            &mut registry,
            source,
            SourceBackedRouteSelection::ExplicitManual,
            &temp.path().join("data-root"),
        )
        .unwrap();

        let work = refresh_fixture_with_work(&temp.path().join("index"), &registry);
        assert_eq!(work.snapshot_opens, 1, "{provider:?}");
        assert_eq!(work.logical_online_backup_opens, 1, "{provider:?}");
        assert_eq!(work.logical_fingerprint_passes, 0, "{provider:?}");
        assert_eq!(work.logical_row_traversals, 1, "{provider:?}");
        assert_eq!(work.logical_rows_projected, 32, "{provider:?}");
    }
}

fn create_metadata_and_message_part_fixture(path: &Path) {
    let connection = create_current_fixture(path);
    connection
        .execute_batch(
            r#"create table message (
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
             insert into session_message values (
                 'metadata-agent', 'session-1', 'agent-switched', 1, 1, 1,
                 '{"text":"metadata agent notice must not be emitted"}'
             );
             insert into session_message values (
                 'metadata-model', 'session-1', 'model-switched', 2, 2, 2,
                 '{"text":"metadata model notice must not be emitted"}'
             );
             insert into message values (
                 'message-user', 'session-1', 10, 10, '{"role":"user"}'
             );
             insert into part values (
                 'part-user', 'message-user', 'session-1', 11, 11,
                 '{"type":"text","text":"user conversation from part"}'
             );
             insert into message values (
                 'message-assistant', 'session-1', 20, 20, '{"role":"assistant"}'
             );
             insert into part values (
                 'part-assistant', 'message-assistant', 'session-1', 21, 21,
                 '{"type":"text","text":"assistant conversation from part"}'
             );"#,
        )
        .unwrap();
}

fn create_agent_switched_fixture(path: &Path) {
    let connection = create_current_fixture(path);
    connection
        .execute_batch(
            r#"insert into session_message values (
                 'metadata-agent', 'session-1', 'agent-switched', 1, 2, 2,
                 '{"agent":"plan","text":"agent switched from build to plan"}'
             );"#,
        )
        .unwrap();
}

fn create_current_fixture(path: &Path) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             insert into session values ('session-1', 1, 1);",
        )
        .unwrap();
    connection
}

#[test]
fn current_11811_shape_selects_populated_message_part_over_empty_session_message() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "current OpenCode response"}),
    );
    let counts = connection
        .query_row(
            "select
                 (select count(*) from event),
                 (select count(*) from message),
                 (select count(*) from part),
                 (select count(*) from session),
                 (select count(*) from session_message)",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(counts, (9, 2, 1, 1, 0));

    let schema = OpenCodeNativeSchema::probe(
        &connection,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(schema.family, OpenCodeNativeSchemaFamily::MessagePart);
    assert!(!schema.capability_digest.is_empty());
}

#[test]
fn independently_populated_representations_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "message part representation"}),
    );
    connection
        .execute(
            "insert into session_message values (
                 'competing-message', 'current-session', 'user', 1,
                 1782259200000, 1782259200000, ?1
             )",
            [json!({
                "role": "user",
                "time": {"created": 1782259200000_i64},
                "text": "session message representation"
            })
            .to_string()],
        )
        .unwrap();

    let error = OpenCodeNativeSchema::probe(
        &connection,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap_err();
    assert!(error.to_string().contains(
        "ambiguous populated message schema families: session_message_seq, message_part"
    ));
}

#[cfg(unix)]
fn run_git(path: &Path, arguments: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

#[cfg(unix)]
#[test]
fn current_schema_projects_shared_repository_attribution_and_preserves_native_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    run_git(&repository, &["init", "-q"]);
    run_git(&repository, &["config", "user.name", "ctx test"]);
    run_git(
        &repository,
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/opencode-current.git",
        ],
    );
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn current() {}\n").unwrap();
    run_git(&repository, &["add", "src/lib.rs"]);
    run_git(&repository, &["commit", "-qm", "fixture"]);

    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        &repository,
        &json!({
            "type": "tool",
            "tool": "edit",
            "state": {
                "input": {
                    "command": "git status --short",
                    "workdir": repository,
                    "path": "src/lib.rs"
                }
            }
        }),
    );
    drop(connection);
    let mut permissions = fs::metadata(&database).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&database, permissions).unwrap();

    let (observation, scan, records) = scan_current_schema(&database);
    assert_eq!(
        observation.schema.family,
        OpenCodeNativeSchemaFamily::MessagePart
    );
    assert_eq!(scan.certificate.counts().complete_records, 1);
    assert_eq!(scan.certificate.counts().indexed_documents, 1);
    let [record] = records.as_slice() else {
        panic!("expected one current-schema Core record");
    };
    assert!(record
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("git status --short")));
    assert_eq!(
        record.provider_session_id.as_deref(),
        Some("current-session")
    );
    assert!(record.native_event_id.is_some());
    assert_eq!(
        record.cwd.as_deref(),
        Some(repository.to_string_lossy().as_ref())
    );
    assert_eq!(
        record.metadata["provider_native_file_touches"],
        json!(["src/lib.rs"])
    );
    assert!(record.metadata.contains_key("repository_association"));
    assert_eq!(record.repository_bindings.len(), 1);
    assert_eq!(
        record.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/opencode-current"
    );
    assert!(record.repository_bindings[0]
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
    assert_eq!(record.repository_file_observations.len(), 1);
    assert_eq!(
        record.repository_file_observations[0].relative_path,
        "src/lib.rs"
    );
    assert_eq!(
        record.repository_file_observations[0].kind,
        RepositoryFileObservationKind::Modified
    );
    assert_eq!(
        record
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::SessionCwd)
            .collect::<Vec<_>>(),
        vec![repository.to_string_lossy().as_ref()]
    );
    assert_eq!(
        record
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::FileActivityPath)
            .collect::<Vec<_>>(),
        vec![repository.join("src/lib.rs").to_string_lossy().as_ref()]
    );
    record.validate_contract().unwrap();
}
