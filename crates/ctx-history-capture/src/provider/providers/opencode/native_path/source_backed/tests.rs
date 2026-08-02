#[cfg(unix)]
use std::process::Command;
use std::{fs, path::Path};

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
        refresh_source_backed_generation, refresh_source_backed_generation_with_detailed_progress,
        SourceBackedCoordinatorError, SourceBackedCurrentSourceProgress,
        SourceBackedCurrentSourceProgressStage, SourceBackedProviderRegistry,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
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
    [u8; 32],
) {
    let authorized =
        open_root_authorized_snapshot_retained(crate::test_provider_sqlite_data_root(), path)
            .unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let admitted_fingerprint = super::adapter::admitted_leaf_fingerprint(
        &observation.source,
        authorized.sqlite_snapshot.evidence(),
    );
    let mut records = Vec::new();
    let scan = scan_pinned_source(
        path,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |output| {
            if let OpenCodeScanOutput::Document(record) = output {
                records.push(record);
            }
            Ok(())
        },
    )
    .unwrap();
    (observation, scan, records, admitted_fingerprint)
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
    refresh_fixture_with_work_and_progress(index_root, registry).0
}

fn refresh_fixture_with_work_and_progress(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> (
    super::adapter::OpenCodeSqliteWorkCounters,
    Vec<SourceBackedCurrentSourceProgress>,
) {
    let _ = super::adapter::take_last_work_counters();
    let mut progress = Vec::new();
    refresh_source_backed_generation_with_detailed_progress(
        index_root,
        registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update| {
            if let Some(current) = update.current_source_progress {
                progress.push(current);
            }
            Ok(())
        },
    )
    .unwrap();
    (super::adapter::take_last_work_counters().unwrap(), progress)
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
    let admitted_fingerprint = super::adapter::admitted_leaf_fingerprint(
        &observation.source,
        authorized.sqlite_snapshot.evidence(),
    );
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

    let (_, _, refreshed, refreshed_fingerprint) = scan_current_schema(&database);
    assert_ne!(refreshed_fingerprint, admitted_fingerprint);
    assert_eq!(
        refreshed
            .into_iter()
            .map(|record| record.content.normalized_body.unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["admitted OpenCode message", "later OpenCode message"]
    );
    let imported = project_fixture(&database, temp.path());
    assert_eq!(
        imported
            .items
            .into_iter()
            .map(|item| item.core_record.content.normalized_body.unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["admitted OpenCode message", "later OpenCode message"]
    );
}

#[test]
fn indexed_synthetic_progress_uses_one_snapshot_and_one_logical_row_traversal() {
    const ROWS: u64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS as i64);

    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    register_projection_function(&connection, dialect).unwrap();
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    let mut sql = source_backed_event_sql(&schema);
    sql.push_str(source_backed_event_order_sql(&schema));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map([MAX_PROVIDER_SQLITE_VALUE_BYTES as i64], |row| {
            row.get::<_, String>(3)
        })
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

    let (cold, cold_progress) = refresh_fixture_with_work_and_progress(&index_root, &registry);
    assert_eq!(cold.snapshot_opens, 1);
    assert_eq!(cold.logical_online_backup_opens, 1);
    assert!(cold.logical_online_backup_steps > 0);
    assert!(cold.logical_online_backup_pages > 0);
    assert_eq!(cold.schema_probe_passes, 1);
    assert_eq!(cold.logical_fingerprint_passes, 0);
    assert_eq!(cold.logical_row_traversals, 1);
    assert_eq!(cold.projection_passes, 1);
    assert_eq!(cold.logical_rows_projected, ROWS);
    assert_eq!(cold.documents_staged, ROWS);
    assert!(cold_progress
        .iter()
        .any(|update| update.stage == SourceBackedCurrentSourceProgressStage::OnlineBackup));
    assert!(!cold_progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::LogicalFingerprint
    }));
    let logical_scan = cold_progress
        .iter()
        .filter(|update| update.stage == SourceBackedCurrentSourceProgressStage::LogicalScan)
        .collect::<Vec<_>>();
    assert_eq!(
        logical_scan.len() as u64,
        ROWS / LOGICAL_SCAN_PROGRESS_ROW_CADENCE + 2
    );
    assert_eq!(logical_scan[0].logical_rows_scanned, Some(0));
    assert!(logical_scan.windows(2).all(|pair| {
        pair[0].logical_rows_scanned <= pair[1].logical_rows_scanned
            && pair[0].logical_certified_bytes <= pair[1].logical_certified_bytes
    }));
    assert_eq!(
        logical_scan.last().unwrap().logical_rows_scanned,
        Some(ROWS)
    );

    let unchanged = refresh_fixture_with_work(&index_root, &registry);
    assert_eq!(unchanged.snapshot_opens, 1);
    assert_eq!(unchanged.logical_online_backup_opens, 1);
    assert!(unchanged.logical_online_backup_steps > 0);
    assert!(unchanged.logical_online_backup_pages > 0);
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
    assert!(changed.logical_online_backup_steps > 0);
    assert!(changed.logical_online_backup_pages > 0);
    assert_eq!(changed.logical_fingerprint_passes, 0);
    assert_eq!(changed.logical_row_traversals, 1);
    assert_eq!(changed.projection_passes, 1);
    assert_eq!(changed.logical_rows_projected, ROWS + 1);
    assert_eq!(changed.documents_staged, ROWS + 1);
}

#[test]
fn kilo_and_mimocode_progress_uses_one_online_backup_and_streaming_pass() {
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

        let (work, progress) =
            refresh_fixture_with_work_and_progress(&temp.path().join("index"), &registry);
        assert_eq!(work.snapshot_opens, 1, "{provider:?}");
        assert_eq!(work.logical_online_backup_opens, 1, "{provider:?}");
        assert_eq!(work.logical_fingerprint_passes, 0, "{provider:?}");
        assert_eq!(work.logical_row_traversals, 1, "{provider:?}");
        assert_eq!(work.logical_rows_projected, 32, "{provider:?}");
        assert!(
            progress
                .iter()
                .any(|update| update.stage == SourceBackedCurrentSourceProgressStage::OnlineBackup),
            "{provider:?}"
        );
        assert!(
            progress
                .iter()
                .any(|update| update.stage == SourceBackedCurrentSourceProgressStage::LogicalScan),
            "{provider:?}"
        );
    }
}

#[test]
fn opencode_progress_callback_failure_stays_systemic_internal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, 1);
    let source = provider_source_for_path(CaptureProvider::OpenCode, database);
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &temp.path().join("data-root"),
    )
    .unwrap();

    let error = refresh_source_backed_generation_with_detailed_progress(
        temp.path().join("index"),
        &registry,
        WriterOptions::default(),
        |update| {
            if update.current_source_progress.is_some() {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "injected OpenCode progress failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Internal,
                detail,
            },
            ..
        } if detail.contains("progress callback failed")
            && detail.contains("injected OpenCode progress failure")
    ));
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

    let (observation, scan, records, _) = scan_current_schema(&database);
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
