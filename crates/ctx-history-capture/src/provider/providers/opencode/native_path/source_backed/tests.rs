#[cfg(unix)]
use std::process::Command;
use std::{fs, path::Path};

use ctx_history_core::{
    CaptureProvider, EventRole, EventType, RepositoryFileInvocationKind, TypedKey,
};
#[cfg(unix)]
use ctx_history_core::{
    RepositoryCandidateKind, RepositoryEvidenceKind, RepositoryFileObservationKind,
};
use ctx_history_index::{CoreSourceEventPage, VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

use super::super::query::{
    source_backed_event_order_sql, source_backed_event_sql,
    source_backed_fallback_events_by_rowids_sql, source_backed_fallback_sort_key_sql,
};
use super::ordering::{
    OPENCODE_HYDRATION_BATCH_BYTES, OPENCODE_HYDRATION_BATCH_ROWS,
    OPENCODE_HYDRATION_SINGLETON_MAX_BYTES,
};
use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_detailed_progress,
        refresh_source_backed_generation_with_progress, SourceBackedCoordinatorError,
        SourceBackedCurrentSourceProgressStage, SourceBackedProviderRegistry,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

mod current_schema;
mod temp_authority;

use current_schema::create_current_fixture;

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
        include_str!("invocation.rs"),
        include_str!("projection.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("strict_tool_call_projection"));
    assert!(production.contains("map_or(lexical_body"));
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
fn strict_native_tool_call_preserves_exact_target_range_and_lexical_prefix() {
    let body = json!({
        "type": "tool",
        "tool": "write_file",
        "state": {"input": {"path": "src/write.rs", "content": "exact"}}
    });
    let prefix = "tool call: write_file";
    let projected = strict_tool_call_projection(&body, prefix).unwrap();
    assert!(projected.normalized_body.starts_with(prefix));
    assert_eq!(&projected.normalized_body[..prefix.len()], prefix);
    let [invocation] = projected.file_invocations.as_slice() else {
        panic!("expected one exact native invocation");
    };
    assert_eq!(invocation.operation_ordinal, 0);
    assert_eq!(invocation.tool_name.as_deref(), Some("write_file"));
    assert_eq!(invocation.path, "src/write.rs");
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Write);
    let range = invocation.normalized_text_range.unwrap();
    assert_eq!(
        &projected.normalized_body[range.start as usize..range.end as usize],
        serde_json::to_string(&body).unwrap()
    );
}

#[test]
fn strict_native_tool_call_ambiguity_and_overflow_abstain_without_cross_call_inference() {
    let ambiguous = json!({"type": "tool", "tool": "edit_file", "state": {"input": {
        "path": "src/a.rs", "file_path": "src/a.rs"
    }}});
    let projected = strict_tool_call_projection(&ambiguous, "tool call: edit_file").unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Opaque)
    );

    let recursive_decoy = json!({
        "type": "tool",
        "tool": "edit_file",
        "state": {
            "input": {"replacement": "no exact target"},
            "metadata": {"path": "src/decoy.rs"}
        }
    });
    let projected = strict_tool_call_projection(&recursive_decoy, "tool call: edit_file").unwrap();
    assert!(projected.file_invocations.is_empty());

    let paths = (0..=MAX_STRICT_FILE_INVOCATIONS)
        .map(|index| format!("src/{index}.rs"))
        .collect::<Vec<_>>();
    let overflow = json!({
        "type": "tool",
        "tool": "read_file",
        "state": {"input": {"files": paths}}
    });
    let projected = strict_tool_call_projection(&overflow, "tool call: read_file").unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Capacity)
    );
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("overflow-opencode.db");
    drop(write_current_schema(&database, temp.path(), &overflow));
    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected the overflowing call to remain a Core record");
    };
    assert!(record.repository_file_invocation_evidence.is_empty());
    assert!(record.repository_abstentions.iter().any(|abstention| {
        abstention.reason == ctx_history_core::RepositoryAbstentionReason::CandidateLimitExceeded
            && abstention.detail.as_deref() == Some("opencode_file_invocation_evidence_overflow")
    }));

    for name in [
        "READ_FILE",
        "Read_File",
        "grep",
        "glob",
        "search",
        "apply_patch",
        "patch",
    ] {
        let body = json!({"type": "tool", "tool": name, "state": {"input": {"path": "src/no.rs"}}});
        let projected = strict_tool_call_projection(&body, "old lexical body").unwrap();
        assert!(projected.file_invocations.is_empty(), "promoted {name}");
        assert_eq!(projected.normalized_body, "old lexical body");
        assert_eq!(
            projected.abstention,
            Some(StrictInvocationAbstention::Opaque)
        );
    }

    let generic_wrapper =
        json!({"tool_calls": [{"name": "read_file", "arguments": {"path": "src/no.rs"}}]});
    let projected = strict_tool_call_projection(&generic_wrapper, "old lexical body").unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(projected.normalized_body, "old lexical body");

    let byte_overflow = json!({"type": "tool", "tool": "read_file", "state": {"input": {
        "files": (0..5).map(|index| format!("{}-{index}", "x".repeat(16 * 1024 - 2))).collect::<Vec<_>>()
    }}});
    let projected = strict_tool_call_projection(&byte_overflow, "tool call: read_file").unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Capacity)
    );
    assert!(strict_text_range(0, u32::MAX as usize + 1).is_none());
}

#[test]
fn result_shapes_are_classified_before_strict_invocation_projection() {
    let body = json!({
        "type": "tool",
        "tool": "edit_file",
        "result_outcome": "failure",
        "path": "src/result-only.rs"
    });
    assert_eq!(
        projection::source_backed_retained_event_kind("tool", "tool", &body),
        OpenCodeNativeEventKind::ToolOutput
    );
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
                OpenCodeScanOutput::Progress(_) => {}
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

#[test]
fn detailed_progress_reports_backup_fingerprint_and_one_pass_scan() {
    const ROWS: i64 = 96;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS);
    let source = provider_source_for_path(CaptureProvider::OpenCode, database);
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        &temp.path().join("data-root"),
    )
    .unwrap();
    let mut progress = Vec::new();

    let receipt = refresh_source_backed_generation_with_detailed_progress(
        temp.path().join("index"),
        &registry,
        WriterOptions::default(),
        |update| {
            if let Some(progress_update) = update.current_source_progress {
                progress.push(progress_update);
            }
            Ok(())
        },
    )
    .unwrap();

    assert!(progress.iter().any(|update| {
        matches!(
            update.stage,
            SourceBackedCurrentSourceProgressStage::OnlineBackup
                | SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
        )
    }));
    assert!(progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::LogicalFingerprint
    }));
    let scan = progress
        .iter()
        .filter(|update| update.stage == SourceBackedCurrentSourceProgressStage::LogicalScan)
        .collect::<Vec<_>>();
    assert_eq!(scan.first().unwrap().logical_rows_scanned, Some(0));
    assert_eq!(scan.last().unwrap().logical_rows_scanned, Some(ROWS as u64));
    assert_eq!(
        scan.last().unwrap().logical_certified_bytes,
        Some(receipt.sources[0].counts().certified_bytes)
    );
}

#[test]
fn detailed_progress_callback_failure_remains_systemic() {
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
                    "injected OpenCode progress callback failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError {
            kind: SourceBackedRouteErrorKind::Unavailable,
            detail,
        }) if detail.contains("injected OpenCode progress callback failure")
    ));
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
fn unrelated_sibling_creation_does_not_invalidate_the_sqlite_source_family() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_directory = temp.path().join("source");
    let database = source_directory.join("opencode.sqlite");
    fs::create_dir_all(&source_directory).unwrap();
    drop(write_current_schema(
        &database,
        temp.path(),
        &json!({
            "role": "user",
            "text": "source-family authority"
        }),
    ));

    let data_root = temp.path().join("data-root");
    let authorized =
        open_root_authorized_snapshot_retained_with_hook(&data_root, &database, || {
            fs::write(source_directory.join("unrelated.txt"), "unrelated churn").unwrap();
        })
        .unwrap();
    authorized.sqlite_snapshot.finish().unwrap();
    authorized.source_root.revalidate_same_object().unwrap();
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
    assert_eq!(cold.schema_event_validation_traversals, 2);
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
    let sort_key_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_fallback_sort_key_sql(&schema)
        ))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let hydration_sql = source_backed_fallback_events_by_rowids_sql(&schema, 64);
    let hydration_plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {hydration_sql}",))
        .unwrap()
        .query_map(
            rusqlite::params_from_iter(std::iter::repeat_n(rusqlite::types::Null, 64)),
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        sort_key_plan
            .iter()
            .chain(&hydration_plan)
            .all(|step| !step.contains("USE TEMP B-TREE")),
        "indexed current-schema batches used SQLite temporary sorting: {sort_key_plan:?} {hydration_plan:?}"
    );
    assert!(
        sort_key_plan
            .iter()
            .chain(&hydration_plan)
            .all(|step| { !step.contains("CORRELATED") && !step.contains("VIRTUAL TABLE") }),
        "indexed current-schema query repeated JSON-tree traversal in SQLite: {sort_key_plan:?} {hydration_plan:?}"
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
    assert!(cold.fallback_disk_sort);
    assert_eq!(cold.fallback_sort_rows, ROWS);
    assert_eq!(cold.fallback_payload_hydrations, ROWS);
    assert!(cold.ordering_data_statements < ROWS / 8);
    assert!(cold.ordering_sort_key_batches < ROWS / 8);
    assert!(cold.ordering_hydration_batches < ROWS / 8);
    assert!(cold.max_sort_key_batch_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(cold.max_buffered_payload_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(cold.max_buffered_payload_bytes <= OPENCODE_HYDRATION_BATCH_BYTES);
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
