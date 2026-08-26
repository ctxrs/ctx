use std::{fs, path::Path};

use ctx_history_capture_model::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind,
};
use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use super::*;
use crate::lifecycle::SourceBackedRouteErrorKind;

#[path = "tests/inventory_mutation_tests.rs"]
mod inventory_mutation;

const PARENT: &str = "parent-session";
const CHILD: &str = "child-session";

#[test]
fn root_scope_composes_once_with_hermes_profile_and_session_lineage() {
    use ctx_history_core::{SourceAnchorScope, SourceKey};

    let provider_source = |profile: &str| crate::ProviderSource {
        provider: CaptureProvider::Hermes,
        path: std::path::PathBuf::from(format!("/tmp/profiles/{profile}/state.db")),
        exists: true,
        source_format: HERMES_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: crate::ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    };
    let released = SourceKey::derive_provider_native(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_PROFILE_SOURCE_SCHEMA_VARIANT,
        1,
        HERMES_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8("alpha").unwrap(),
    )
    .unwrap();
    let unqualified = HermesSourceCandidate::automatic("/tmp", provider_source("alpha"))
        .unwrap()
        .source;
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first_profile = HermesSourceCandidate::automatic_scoped(
        "/tmp",
        provider_source("alpha"),
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap()
    .source;
    let second_profile = HermesSourceCandidate::automatic_scoped(
        "/tmp",
        provider_source("alpha"),
        SourceAnchorScope::Lineage([0x22; 32]),
    )
    .unwrap()
    .source;
    let first_session = hermes_session_source_key(&first_profile, "shared-session").unwrap();
    let second_session = hermes_session_source_key(&second_profile, "shared-session").unwrap();
    assert_ne!(first_session.identity(), second_session.identity());
    assert_ne!(
        projection_context(&first_session).session_id,
        projection_context(&second_session).session_id
    );

    let expected_child_anchor = SourceAnchor::provider_native(
        HERMES_SESSION_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::composite(vec![
            TypedKey::bytes(
                first_profile
                    .identity()
                    .encode_canonical()
                    .unwrap()
                    .to_vec(),
            )
            .unwrap(),
            TypedKey::utf8("shared-session").unwrap(),
        ])
        .unwrap(),
    )
    .unwrap();
    let expected_child = SourceKey::derive(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_SESSION_SOURCE_SCHEMA_VARIANT,
        1,
        expected_child_anchor,
    )
    .unwrap();
    assert!(expected_child.exact_descriptor_eq(&first_session));

    let sibling_profile = HermesSourceCandidate::automatic_scoped(
        "/tmp",
        provider_source("beta"),
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap()
    .source;
    assert_ne!(first_profile.identity(), sibling_profile.identity());
}

#[test]
fn direct_core_projection_is_complete_and_has_no_recursive_ancestry_sql() {
    let production = [
        include_str!("../source_backed.rs"),
        include_str!("projection.rs"),
        include_str!("replacement.rs"),
    ]
    .join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("HERMES_SOURCE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("native.complete_text"));
    assert!(production.contains("parent_session_id"));
    assert!(!production.to_ascii_lowercase().contains("with recursive"));
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
    assert_eq!(
        HERMES_SOURCE_PARSER_REVISION,
        "hermes-source-backed-v5-optional-admission"
    );
}

#[test]
fn terminal_finish_and_revalidation_preserve_typed_sqlite_failures() {
    let changed = replacement::route_hermes_terminal_revalidation::<()>(Err(
        SqliteSourceAccessError::SourceChanged,
    ))
    .unwrap_err();
    assert_eq!(changed.kind, SourceBackedRouteErrorKind::SourceChanged);

    let cleanup = SqliteSourceAccessError::ScratchIoUnavailable {
        operation: "cleaning the Hermes terminal regression snapshot",
        path: "hermes-terminal.sqlite".into(),
        source: std::io::Error::from(std::io::ErrorKind::StorageFull),
    }
    .with_cleanup_status(SqliteCleanupStatus::Failed);
    let cleanup = replacement::route_hermes_terminal_revalidation::<()>(Err(cleanup)).unwrap_err();
    assert_eq!(
        cleanup.kind,
        SourceBackedRouteErrorKind::ResourceUnavailable
    );
    assert!(cleanup.detail.contains("cleanup_status=failed"));
}

fn create_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 ended_at real,
                 message_count integer default 0,
                 cwd text,
                 git_branch text,
                 git_repo_root text
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null,
                 active integer not null default 1,
                 compacted integer not null default 0
             );
             insert into sessions
                 (id, source, parent_session_id, started_at, message_count, cwd, git_branch, git_repo_root)
                 values
                 ('parent-session', 'acp', null, 1782259200.0, 1, '/repo/parent', 'main', '/repo'),
                 ('child-session', 'acp', 'parent-session', 1782259201.0, 1, '/repo/child', 'feature', '/repo');
             insert into messages (id, session_id, role, content, timestamp) values
                 (10, 'parent-session', 'assistant', 'parent stable needle', 1782259202.0),
                 (20, 'child-session', 'assistant', 'child stable needle', 1782259203.0);",
        )
        .unwrap();
}

fn candidate(data_root: &Path, database: &Path) -> HermesSourceCandidate {
    hermes_source_backed_explicit(
        data_root,
        database,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap()
}

fn session_source(candidate: &HermesSourceCandidate, session: &str) -> SourceKey {
    hermes_session_source_key(&candidate.source, session).unwrap()
}

fn projection_context(source: &SourceKey) -> HermesSessionContext {
    let native_session_key =
        NativeSessionKey::native_id(HERMES_SESSION_NAMESPACE, TypedKey::utf8(PARENT).unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: HERMES_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .unwrap();
    HermesSessionContext {
        session_id,
        parent_session_id: None,
        agent_scope: AgentScope::Primary,
        branch: None,
        workspace: None,
        cwd: None,
    }
}

fn projection_message_row(id: i64, role: &str) -> HermesMessageRow {
    HermesMessageRow {
        id,
        session_id: PARENT.to_owned(),
        role: role.to_owned(),
        content: Some("complete provider content".to_owned()),
        tool_call_id: None,
        tool_calls: None,
        tool_name: None,
        timestamp: 1_782_259_202.0,
        token_count: None,
        finish_reason: None,
        reasoning: None,
        reasoning_content: None,
        reasoning_details: None,
        codex_reasoning_items: None,
        codex_message_items: None,
        platform_message_id: None,
        observed: 0,
        active: 1,
        compacted: 0,
    }
}

fn oversized_optional_metadata() -> String {
    "x".repeat(64 * 1024 + 1)
}

#[test]
fn optional_activity_values_are_omitted_without_losing_raw_content_or_emitting_empty_activity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let candidate = candidate(&temp.path().join("data-root"), &database);
    let source = session_source(&candidate, PARENT);
    let context = projection_context(&source);

    for (offset, invalid_call_id) in [String::new(), oversized_optional_metadata()]
        .into_iter()
        .enumerate()
    {
        let mut row = projection_message_row(100 + offset as i64, "assistant");
        row.tool_call_id = Some(invalid_call_id.clone());
        row.tool_calls = Some("{}".to_owned());
        row.tool_name = Some("run".to_owned());

        let record = project_message(&source, offset as u64, row, &context).unwrap();
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["tool_call_id"].as_str(),
            Some(invalid_call_id.as_str())
        );
        assert_eq!(record.content.activity, None);

        let mut row = projection_message_row(102 + offset as i64, "tool");
        row.tool_call_id = Some(invalid_call_id.clone());
        let record = project_message(&source, 2 + offset as u64, row, &context).unwrap();
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["tool_call_id"].as_str(),
            Some(invalid_call_id.as_str())
        );
        assert_eq!(record.content.activity, None);
    }

    for (offset, invalid_tool_name) in [String::new(), oversized_optional_metadata()]
        .into_iter()
        .enumerate()
    {
        let mut row = projection_message_row(110 + offset as i64, "assistant");
        row.tool_call_id = Some("call-110".to_owned());
        row.tool_calls = Some("{}".to_owned());
        row.tool_name = Some(invalid_tool_name.clone());

        let record = project_message(&source, 10 + offset as u64, row, &context).unwrap();
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["tool_name"].as_str(),
            Some(invalid_tool_name.as_str())
        );
        assert_eq!(record.content.activity, None);
    }

    for (offset, invalid_status) in [String::new(), oversized_optional_metadata()]
        .into_iter()
        .enumerate()
    {
        let mut row = projection_message_row(120 + offset as i64, "tool");
        row.tool_call_id = Some("call-120".to_owned());
        row.finish_reason = Some(invalid_status.clone());

        let record = project_message(&source, 20 + offset as u64, row, &context).unwrap();
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["status"].as_str(),
            Some(invalid_status.as_str())
        );
        let activity = record.content.activity.unwrap();
        assert!(activity.provider_call_id.is_some());
        assert_eq!(activity.result.unwrap().status, None);
    }
}

#[test]
fn optional_facts_omit_empty_and_oversized_values_while_preserving_order_and_exact_text() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let candidate = candidate(&temp.path().join("data-root"), &database);
    let source = session_source(&candidate, PARENT);
    let mut context = projection_context(&source);
    context.branch = Some(String::new());
    context.workspace = Some("x".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES + 1));

    let record = project_message(
        &source,
        0,
        projection_message_row(130, "assistant"),
        &context,
    )
    .unwrap();
    assert_eq!(record.content.activity, None);

    context.branch = Some(" feature ".to_owned());
    context.cwd = Some(" /repo/worktree ".to_owned());
    let record = project_message(
        &source,
        1,
        projection_message_row(131, "assistant"),
        &context,
    )
    .unwrap();
    let facts = record.content.activity.unwrap().facts;
    assert_eq!(
        facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (LiteralFactKind::Branch, " feature "),
            (LiteralFactKind::SessionCwd, " /repo/worktree "),
        ]
    );
}

fn automatic_candidate(data_root: &Path, database: &Path) -> HermesSourceCandidate {
    HermesSourceCandidate::automatic(
        data_root,
        crate::ProviderSource {
            provider: CaptureProvider::Hermes,
            path: database.to_path_buf(),
            exists: true,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: crate::ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
    )
    .unwrap()
}

#[test]
fn renamed_profile_control_is_rejected_for_a_different_profile_descriptor() {
    let receipt = HermesRefreshReceipt {
        kind: HERMES_ROUTE_CONTROL_KIND.to_owned(),
        version: HERMES_ROUTE_CONTROL_VERSION,
        profile_source_descriptor: [1; 32],
        database_identity: [2; 32],
        schema_evidence: [3; 32],
        session_rowid: 10,
        message_rowid: 20,
        last_successful_exhaustive_at_ms: 1000,
        exact_due_at_ms: 2000,
        exhaustive_sequence: 1,
        mode: "incremental".to_owned(),
        outcome: "successful".to_owned(),
    };
    let control = serde_json::to_vec(&receipt).unwrap();
    assert_eq!(
        hermes_route_control_exact_due_for_profile(&control, [9; 32], 1500),
        None
    );
    assert_eq!(
        hermes_route_control_exact_due_for_profile(&control, [1; 32], 1500),
        Some(false)
    );
}

#[test]
fn session_source_keys_are_profile_scoped_and_stable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("profiles/alpha/state.db");
    let second = temp.path().join("profiles/beta/state.db");
    create_fixture(&first);
    create_fixture(&second);
    let first_candidate = automatic_candidate(temp.path(), &first);
    let second_candidate = automatic_candidate(temp.path(), &second);

    assert_eq!(
        session_source(&first_candidate, PARENT),
        session_source(&first_candidate, PARENT)
    );
    assert_ne!(
        session_source(&first_candidate, PARENT),
        session_source(&second_candidate, PARENT)
    );
    assert_ne!(
        session_source(&first_candidate, PARENT),
        session_source(&first_candidate, CHILD)
    );
}

fn projected_bodies(
    candidate: &HermesSourceCandidate,
    snapshot: &crate::provider_sources::SqliteSourceReadSnapshot,
    session: &str,
) -> Vec<String> {
    let mut inventory =
        observe_hermes_session_inventory::<crate::registration::tests::NoopLifecycle>(
            candidate,
            snapshot.connection().unwrap(),
            &mut |_| Ok(()),
        )
        .unwrap();
    let leaf = &inventory
        .leaves
        .iter()
        .find(|leaf| leaf.provider_leaf.provider_session_id == session)
        .unwrap()
        .provider_leaf;
    let mut bodies = Vec::new();
    project_hermes_session_snapshot(
        candidate,
        leaf,
        &inventory.schema,
        snapshot.connection().unwrap(),
        inventory.message_spool.as_mut().unwrap(),
        &mut |page| {
            bodies.extend(page.records.into_iter().filter_map(|record| match record {
                HermesSourceBackedRecord::Event(event) => event.content.normalized_body,
                HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => {
                    None
                }
            }));
            Ok(())
        },
    )
    .unwrap();
    bodies
}

#[test]
fn concurrent_commit_cannot_mix_hermes_inventory_and_body_observations() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let candidate = candidate(&data_root, &database);

    let (_authority, baseline) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    baseline.finish().unwrap();
    let (_authority, snapshot) =
        open_root_authorized_snapshot_with_hook(&data_root, &database, || {
            writer
                .execute_batch(
                    "insert into messages (id, session_id, role, content, timestamp)
                         values (11, 'parent-session', 'assistant', 'racing append', 1782259250.0);
                     update sessions set message_count = 2 where id = 'parent-session';",
                )
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        projected_bodies(&candidate, &snapshot, PARENT),
        vec!["parent stable needle"]
    );
    snapshot.finish().unwrap();

    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    assert_eq!(
        projected_bodies(&candidate, &snapshot, PARENT),
        vec!["parent stable needle", "racing append"]
    );
    snapshot.finish().unwrap();
}
