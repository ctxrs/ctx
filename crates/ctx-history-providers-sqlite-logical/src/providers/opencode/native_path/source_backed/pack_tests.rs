use std::{fs, path::Path};

use ctx_history_core::{
    ActivityJsonCapture, AgentScope, EventRole, EventType, LiteralFactKind,
    ProviderNativeSessionRelationship, TypedKey,
};
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
    fail_next_opened_snapshot_cleanup_for_test,
    provider::source_backed::{SourceBackedRouteError, SourceBackedRouteErrorKind},
    provider_sources::{sqlite_retry_decision, SqliteRetryDecision},
};

#[path = "tests/current_schema.rs"]
mod current_schema;
#[path = "tests/projection_contract.rs"]
mod projection_contract;
#[path = "tests/sqlite_diagnostics.rs"]
mod sqlite_diagnostics;
#[path = "tests/temp_authority.rs"]
mod temp_authority;

use current_schema::create_current_fixture;

const OVER_LIMIT_OPTIONAL_METADATA_BYTES: usize = 64 * 1024 + 1;

#[test]
fn root_scope_separates_identical_opencode_family_sessions_and_unqualified_is_released() {
    use ctx_history_core::{SourceAnchor, SourceAnchorScope, SourceKey};

    let family = OpenCodeNativeSchemaFamily::SessionMessageSeq;
    for dialect in [
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &crate::provider::providers::opencode::KILO_SQLITE_DIALECT,
        &crate::provider::providers::opencode::MIMOCODE_SQLITE_DIALECT,
    ] {
        let anchor = SourceAnchor::provider_native(
            format!("{}.sqlite-authority", dialect.provider.as_str()),
            TypedKey::utf8(SOURCE_ANCHOR_KEY).unwrap(),
        )
        .unwrap();
        let released = SourceKey::derive(
            dialect.provider.as_str(),
            dialect.source_format,
            format!("opencode-family-{}-v1", family.label()),
            SOURCE_IDENTITY_VERSION,
            anchor,
        )
        .unwrap();
        let unqualified =
            source_key_scoped(dialect, family, SourceAnchorScope::Unqualified).unwrap();
        assert!(released.exact_descriptor_eq(&unqualified));
        assert_eq!(
            released.identity().encode_canonical().unwrap(),
            unqualified.identity().encode_canonical().unwrap()
        );

        let first =
            source_key_scoped(dialect, family, SourceAnchorScope::Lineage([0x11; 32])).unwrap();
        let second =
            source_key_scoped(dialect, family, SourceAnchorScope::Lineage([0x22; 32])).unwrap();
        assert_ne!(
            session_id(&first, "shared-session").unwrap(),
            session_id(&second, "shared-session").unwrap()
        );
    }
}

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
    let (observation, scan, records, _) = scan_current_schema_result_with_rejections(
        path,
        crate::test_provider_sqlite_data_root(),
        OPENCODE_FALLBACK_SCRATCH_MAX_BYTES,
    )
    .unwrap();
    (observation, scan, records)
}

fn scan_current_schema_with_rejections(
    path: &Path,
) -> (
    OpenCodeLogicalObservation,
    OpenCodeSourceBackedScan,
    Vec<CoreRecord>,
    Vec<SourceBackedRecordRejectionDraft>,
) {
    scan_current_schema_result_with_rejections(
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
    let (observation, scan, records, _) =
        scan_current_schema_result_with_rejections(path, data_root, scratch_limit)?;
    Ok((observation, scan, records))
}

fn scan_current_schema_result_with_rejections(
    path: &Path,
    data_root: &Path,
    scratch_limit: u64,
) -> OpenCodeSourceBackedResult<(
    OpenCodeLogicalObservation,
    OpenCodeSourceBackedScan,
    Vec<CoreRecord>,
    Vec<SourceBackedRecordRejectionDraft>,
)> {
    let authorized = open_root_authorized_snapshot_retained(data_root, path)?;
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection()?,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )?;
    let mut records = Vec::new();
    let mut rejections = Vec::new();
    let scan = scan_pinned_source_with_scratch_limit(
        path,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        scratch_limit,
        &mut |output| {
            match output {
                OpenCodeScanOutput::Document(record) => records.push(record),
                OpenCodeScanOutput::Rejection(rejection) => rejections.push(rejection),
                OpenCodeScanOutput::Begin(_)
                | OpenCodeScanOutput::CompletedBytes(_)
                | OpenCodeScanOutput::Progress(_) => {}
            }
            Ok(())
        },
    )?;
    Ok((observation, scan, records, rejections))
}

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("direct-core-opencode.db");
    let body = json!({
        "type": "tool",
        "call_id": "call-direct",
        "tool": "write_file",
        "state": {"input": {"path": "src/direct.rs", "content": "exact"}}
    });
    drop(write_current_schema(&database, temp.path(), &body));

    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected one direct Core projection");
    };
    assert_eq!(record.parser_revision, PARSER_REVISION);
    assert!(record.native_event_id.is_some());
    assert_eq!(record.content.structured_content.as_ref(), Some(&body));
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("call-direct".to_owned()))
    );
    let invocation = activity.invocation.as_ref().unwrap();
    assert_eq!(invocation.tool, "write_file");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: json!({"path": "src/direct.rs", "content": "exact"}),
        }
    );
    record.validate_contract().unwrap();
}

#[test]
fn strict_native_tool_call_preserves_exact_target_range_and_lexical_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("exact-tool-opencode.db");
    let body = json!({
        "type": "tool",
        "call_id": "call-exact",
        "tool": "write_file",
        "state": {"input": {"path": "src/write.rs", "content": "exact"}}
    });
    drop(write_current_schema(&database, temp.path(), &body));
    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected one exact native invocation");
    };
    assert_eq!(record.content.meaningful_text(), "tool call: write_file");
    assert_eq!(record.content.structured_content.as_ref(), Some(&body));
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("call-exact".to_owned()))
    );
    assert_eq!(activity.invocation.as_ref().unwrap().tool, "write_file");
    assert!(activity
        .facts
        .iter()
        .any(|fact| { fact.kind == LiteralFactKind::File && fact.value == "src/write.rs" }));
}

#[test]
fn empty_command_fact_does_not_reject_the_opencode_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("empty-command-opencode.db");
    let body = json!({
        "type": "tool",
        "callID": "call-empty-command",
        "tool": "bash",
        "state": {
            "status": "error",
            "input": {
                "command": "",
                "path": "src/noop.rs",
                "description": "noop"
            }
        }
    });
    drop(write_current_schema(&database, temp.path(), &body));

    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected the empty-command call to remain a Core record");
    };
    let activity = record.content.activity.as_ref().unwrap();
    let invocation = activity.invocation.as_ref().unwrap();
    assert_eq!(invocation.tool, "bash");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: json!({
                "command": "",
                "path": "src/noop.rs",
                "description": "noop"
            })
        }
    );
    assert!(activity.facts.iter().all(|fact| !fact.value.is_empty()));
    assert!(activity
        .facts
        .iter()
        .any(|fact| fact.kind == LiteralFactKind::File && fact.value == "src/noop.rs"));
    assert!(!activity
        .facts
        .iter()
        .any(|fact| fact.kind == LiteralFactKind::Command));
    record.validate_contract().unwrap();
}

#[test]
fn empty_and_oversized_call_ids_omit_dependent_activity_without_losing_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let oversized = "c".repeat(OVER_LIMIT_OPTIONAL_METADATA_BYTES);
    let cases = [
        (
            "empty-invocation",
            json!({
                "type": "tool",
                "call_id": "",
                "tool": "read_file",
                "state": {"input": {}}
            }),
        ),
        (
            "oversized-invocation",
            json!({
                "type": "tool",
                "call_id": oversized,
                "tool": "read_file",
                "state": {"input": {}}
            }),
        ),
        (
            "empty-result",
            json!({
                "type": "tool_result",
                "call_id": "",
                "status": "completed",
                "output": "exact result"
            }),
        ),
        (
            "oversized-result",
            json!({
                "type": "tool_result",
                "call_id": "c".repeat(OVER_LIMIT_OPTIONAL_METADATA_BYTES),
                "status": "completed",
                "output": "exact result"
            }),
        ),
    ];

    for (label, body) in cases {
        let database = temp.path().join(format!("{label}-opencode.db"));
        drop(write_current_schema(&database, temp.path(), &body));

        let (_, _, records) = scan_current_schema(&database);
        let [record] = records.as_slice() else {
            panic!("expected {label} to remain a Core record");
        };
        assert_eq!(record.content.structured_content.as_ref(), Some(&body));
        let activity = record.content.activity.as_ref().unwrap();
        assert!(activity.provider_call_id.is_none());
        assert!(activity.invocation.is_none());
        assert!(activity.result.is_none());
        assert!(!activity.facts.is_empty());
        record.validate_contract().unwrap();
    }
}

#[test]
fn empty_and_oversized_activity_metadata_are_omitted_without_losing_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let oversized = "m".repeat(OVER_LIMIT_OPTIONAL_METADATA_BYTES);

    for (label, tool) in [
        ("empty-tool", String::new()),
        ("oversized-tool", oversized.clone()),
    ] {
        let body = json!({
            "type": "tool",
            "call_id": format!("call-{label}"),
            "tool": tool,
            "state": {"input": {}}
        });
        let database = temp.path().join(format!("{label}-opencode.db"));
        drop(write_current_schema(&database, temp.path(), &body));

        let (_, _, records) = scan_current_schema(&database);
        let [record] = records.as_slice() else {
            panic!("expected {label} to remain a Core record");
        };
        assert_eq!(record.content.structured_content.as_ref(), Some(&body));
        let activity = record.content.activity.as_ref().unwrap();
        assert!(activity.provider_call_id.is_none());
        assert!(activity.invocation.is_none());
        assert!(activity.result.is_none());
        assert!(!activity.facts.is_empty());
        record.validate_contract().unwrap();
    }

    for (label, status) in [
        ("empty-status", String::new()),
        ("oversized-status", oversized.clone()),
    ] {
        let body = json!({
            "type": "tool_result",
            "call_id": format!("call-{label}"),
            "status": status,
            "output": "exact result"
        });
        let database = temp.path().join(format!("{label}-opencode.db"));
        drop(write_current_schema(&database, temp.path(), &body));

        let (_, _, records) = scan_current_schema(&database);
        let [record] = records.as_slice() else {
            panic!("expected {label} to remain a Core record");
        };
        assert_eq!(record.content.structured_content.as_ref(), Some(&body));
        let activity = record.content.activity.as_ref().unwrap();
        assert!(activity.result.as_ref().unwrap().status.is_none());
        record.validate_contract().unwrap();
    }

    let body = json!({
        "type": "tool_result",
        "role": oversized,
        "call_id": "call-oversized-role",
        "status": "completed",
        "output": "exact result"
    });
    let database = temp.path().join("oversized-role-opencode.db");
    drop(write_current_schema(&database, temp.path(), &body));

    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected oversized role to remain a Core record");
    };
    assert_eq!(record.content.structured_content.as_ref(), Some(&body));
    assert!(record.role.is_none());
    record.validate_contract().unwrap();
}

#[test]
fn strict_native_tool_call_ambiguity_and_overflow_abstain_without_cross_call_inference() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("ambiguous-tool-opencode.db");
    let paths = (0..=512)
        .map(|index| format!("src/{index}.rs"))
        .collect::<Vec<_>>();
    let ambiguous = json!({
        "type": "tool",
        "call_id": "first-call",
        "callId": "second-call",
        "tool": "read_file",
        "state": {"input": {"path": paths}}
    });
    drop(write_current_schema(&database, temp.path(), &ambiguous));
    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected the ambiguous call to remain a Core record");
    };
    assert_eq!(record.content.structured_content.as_ref(), Some(&ambiguous));
    let activity = record.content.activity.as_ref().unwrap();
    assert!(activity.provider_call_id.is_none());
    assert!(activity.invocation.is_none());
    assert_eq!(
        activity
            .facts
            .iter()
            .filter(|fact| fact.kind == LiteralFactKind::File)
            .count(),
        64
    );
    assert!(activity
        .facts
        .iter()
        .all(|fact| { !fact.value.contains("metadata") && !fact.value.contains("src/512.rs") }));
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

    let (_, scan, records) = scan_current_schema(&database);
    assert_eq!(
        scan.source.schema_variant(),
        "opencode-family-message_part-v1"
    );
    assert_eq!(records.len(), 2);
    for record in &records {
        assert_eq!(record.agent_scope, Some(AgentScope::Primary));
        assert_eq!(
            record.event_type.parse::<EventType>().unwrap(),
            EventType::Message
        );
    }
    let mut projected = records
        .into_iter()
        .map(|record| {
            (
                record.content.normalized_body.unwrap(),
                record.role.unwrap(),
                record.native_event_id.unwrap(),
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

    let (_, scan, records) = scan_current_schema(&database);
    assert_eq!(
        scan.source.schema_variant(),
        "opencode-family-session_message_seq-v1"
    );
    let [record] = records.as_slice() else {
        panic!("expected one agent-switched record");
    };
    assert_eq!(
        record.event_type.parse::<EventType>().unwrap(),
        EventType::Notice
    );
    assert_eq!(record.role.as_deref(), Some("agent-switched"));
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("agent switched from build to plan")
    );
    assert_eq!(
        record.native_event_id,
        Some(TypedKey::Utf8("metadata-agent".to_owned()))
    );
}

#[test]
fn admitted_copy_stays_stable_across_later_wal_commit_and_next_open_advances() {
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
                    [json!({
                        "role": "assistant",
                        "text": "later OpenCode message"
                    })
                    .to_string()],
                )
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        authorized
            .sqlite_authority
            .snapshot_counters()
            .copied_snapshot_opens(),
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
fn unrelated_sibling_creation_does_not_invalidate_source_family() {
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
    let mut rejections = 0_u64;
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
                OpenCodeScanOutput::Rejection(_) => {
                    rejections = rejections.saturating_add(1);
                }
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
    assert_eq!(rejections, (ROWS - 1) as u64);
    assert_eq!(completed_bytes.len(), ROWS as usize);
    assert_eq!(completed_bytes.iter().sum::<u64>(), counts.certified_bytes);
}

#[test]
fn detailed_progress_reports_source_copy_and_one_pass_scan() {
    const ROWS: i64 = 96;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, ROWS);
    let mut progress = Vec::new();
    let authorized = open_root_authorized_snapshot_retained_with_progress(
        &temp.path().join("data-root"),
        &database,
        &mut |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    let observation = observe_logical_source_with_progress(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &mut |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    let scan = scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |output| {
            if let OpenCodeScanOutput::Progress(update) = output {
                progress.push(update);
            }
            Ok(())
        },
    )
    .unwrap();

    assert!(progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
    }));
    assert!(progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::LogicalFingerprint
    }));
    let logical_scan = progress
        .iter()
        .filter(|update| update.stage == SourceBackedCurrentSourceProgressStage::LogicalScan)
        .collect::<Vec<_>>();
    assert_eq!(logical_scan.first().unwrap().logical_rows_scanned, Some(0));
    assert_eq!(
        logical_scan.last().unwrap().logical_rows_scanned,
        Some(ROWS as u64)
    );
    assert_eq!(
        logical_scan.last().unwrap().logical_certified_bytes,
        Some(scan.certificate.counts().certified_bytes)
    );
}

#[test]
fn detailed_progress_callback_failure_remains_systemic() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_synthetic_fixture(&database, 1);

    let error = open_root_authorized_snapshot_retained_with_progress(
        &temp.path().join("data-root"),
        &database,
        &mut |_| {
            Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "injected OpenCode progress callback failure",
            ))
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        OpenCodeSourceBackedError::Route(SourceBackedRouteError {
            kind: SourceBackedRouteErrorKind::Unavailable,
            detail,
        }) if detail.contains("injected OpenCode progress callback failure")
    ));
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
        .prepare(&format!("EXPLAIN QUERY PLAN {hydration_sql}"))
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
            .all(|step| !step.contains("CORRELATED") && !step.contains("VIRTUAL TABLE")),
        "indexed current-schema query repeated JSON-tree traversal in SQLite: {sort_key_plan:?} {hydration_plan:?}"
    );
    assert!(
        hydration_plan
            .iter()
            .any(|step| step.contains("SEARCH p USING INTEGER PRIMARY KEY")),
        "indexed current-schema hydration was not driven by requested part rowids: {hydration_plan:?}"
    );
    drop(connection);

    let (_, scan, records) = scan_current_schema(&database);
    assert_eq!(records.len(), ROWS as usize);
    assert_eq!(scan.certificate.counts().complete_records, ROWS);
    assert_eq!(scan.certificate.counts().indexed_documents, ROWS);
    assert!(scan.bounds.fallback_disk_sort);
    assert_eq!(scan.bounds.fallback_sort_rows, ROWS);
    assert_eq!(scan.bounds.fallback_payload_hydrations, ROWS);
    assert!(scan.bounds.ordering_data_statements < ROWS / 8);
    assert!(scan.bounds.ordering_sort_key_batches < ROWS / 8);
    assert!(scan.bounds.ordering_hydration_batches < ROWS / 8);
    assert!(scan.bounds.max_sort_key_batch_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(scan.bounds.max_buffered_payload_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(scan.bounds.max_buffered_payload_bytes <= OPENCODE_HYDRATION_BATCH_BYTES);
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
    assert!(
        !OpenCodeNativeSchema::probe(&connection, dialect)
            .unwrap()
            .message_part_indexed_streaming
    );

    connection
        .execute_batch(
            "drop index partial_part_message_id_id_idx;
             create index nocase_part_message_id_id_idx
                 on part(message_id collate nocase, id);",
        )
        .unwrap();
    assert!(
        !OpenCodeNativeSchema::probe(&connection, dialect)
            .unwrap()
            .message_part_indexed_streaming
    );

    connection
        .execute_batch(
            "drop index nocase_part_message_id_id_idx;
             create index descending_part_message_id_id_idx
                 on part(message_id desc, id);",
        )
        .unwrap();
    assert!(
        !OpenCodeNativeSchema::probe(&connection, dialect)
            .unwrap()
            .message_part_indexed_streaming
    );
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
    let indexed_order = projected_order(&indexed);
    assert_eq!(
        indexed_order,
        vec![
            (0, "earlier user part".to_owned()),
            (1, "later assistant part".to_owned()),
            (2, "earlier nonlexical part".to_owned()),
            (3, "later nonlexical part".to_owned()),
        ]
    );

    Connection::open(&database)
        .unwrap()
        .execute("drop index part_message_id_id_idx", [])
        .unwrap();
    let (_, unindexed_scan, unindexed) = scan_current_schema(&database);
    assert_eq!(projected_order(&unindexed), indexed_order);
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
    assert!(
        !OpenCodeNativeSchema::probe(&connection, dialect)
            .unwrap()
            .message_part_indexed_streaming
    );
    drop(connection);

    let (_, indexed_scan, indexed_records) = scan_current_schema(&database);
    Connection::open(&database)
        .unwrap()
        .execute("drop index part_message_id_id_idx", [])
        .unwrap();
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

fn projected_order(records: &[CoreRecord]) -> Vec<(u64, String)> {
    records
        .iter()
        .map(|record| {
            (
                record.event_sequence,
                record.content.normalized_body.clone().unwrap_or_default(),
            )
        })
        .collect()
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
