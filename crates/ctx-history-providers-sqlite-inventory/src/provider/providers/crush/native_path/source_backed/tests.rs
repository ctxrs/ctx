use std::sync::{Arc, Mutex};

use ctx_history_core::{EventRole, EventType, ProviderNativeSessionRelationship, TypedKey};
use rusqlite::{config::DbConfig, Connection};
use serde_json::json;

use super::*;

#[test]
fn root_scope_composes_with_crush_projects_and_preserves_unqualified_identity() {
    use ctx_history_core::{CaptureProvider, SourceAnchorScope, SourceKey};

    let project = TypedKey::utf8("shared-project").unwrap();
    let released = SourceKey::derive(
        CaptureProvider::Crush.as_str(),
        crate::CRUSH_SQLITE_SOURCE_FORMAT,
        CRUSH_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(CRUSH_SOURCE_ANCHOR_NAMESPACE, project.clone()).unwrap(),
    )
    .unwrap();
    let unqualified =
        crush_source_key_scoped(project.clone(), SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first =
        crush_source_key_scoped(project.clone(), SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = crush_source_key_scoped(project, SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    assert_ne!(
        crush_session_id(&first, "shared-session").unwrap(),
        crush_session_id(&second, "shared-session").unwrap()
    );

    let sibling = crush_source_key_scoped(
        TypedKey::utf8("sibling-project").unwrap(),
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap();
    assert_ne!(first.identity(), sibling.identity());
}

#[derive(Clone)]
struct TestInventory {
    observation: Arc<Mutex<CrushProjectInventoryObservationV0>>,
    work: Arc<Mutex<TestInventoryWork>>,
}

#[derive(Default)]
struct TestInventoryWork {
    projection_passes: u64,
    snapshots: Vec<CrushSnapshotWorkV0>,
}

impl TestInventory {
    fn new(observation: CrushProjectInventoryObservationV0) -> Self {
        Self {
            observation: Arc::new(Mutex::new(observation)),
            work: Arc::default(),
        }
    }

    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
        Ok(self.observation.lock().unwrap().clone())
    }
}

impl CrushProjectInventorySourceV0 for TestInventory {
    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
        self.observe()
    }

    fn record_projection_pass(&self) {
        let mut work = self.work.lock().unwrap();
        work.projection_passes = work.projection_passes.saturating_add(1);
    }

    fn record_snapshot_work(&self, work: CrushSnapshotWorkV0) {
        self.work.lock().unwrap().snapshots.push(work);
    }
}

#[test]
fn query_time_corrupt_and_notadb_keep_provider_content_provenance() {
    for code in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("query-provenance.db");
        write_database(&path, "session", "message", "body");
        let frozen = bind_inventory(
            crate::test_provider_sqlite_data_root(),
            inventory(b"query-provenance", vec![database("project", &path)]),
        )
        .unwrap();
        let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
        let raw = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);

        let diagnosed = diagnose_crush_provider_query_error(
            &source.read_snapshot,
            CrushSourceBackedErrorV0::Sqlite(raw),
            crate::provider_sources::SqliteFailurePhase::Projection,
        );
        let CrushSourceBackedErrorV0::SqliteSource(error) = diagnosed else {
            panic!("expected diagnosed SQLite source error");
        };
        assert!(error.source().is_provider_corruption());
        assert!(!error.source().is_ctx_owned_corruption());
        let expected_artifact = match source.read_snapshot.strategy() {
            #[cfg(target_os = "linux")]
            ctx_history_source_sqlite::SqliteSourceSnapshotStrategy::ImmutableMain => {
                crate::provider_sources::SqliteArtifactKind::ProviderDatabase
            }
            ctx_history_source_sqlite::SqliteSourceSnapshotStrategy::CopiedFamily => {
                crate::provider_sources::SqliteArtifactKind::PrivateSourceCopy
            }
            #[cfg(target_os = "linux")]
            ctx_history_source_sqlite::SqliteSourceSnapshotStrategy::PinnedReadOnlyWal => {
                crate::provider_sources::SqliteArtifactKind::ProviderDatabase
            }
        };
        assert_eq!(
            error.source().diagnostic().unwrap().artifact,
            expected_artifact
        );
    }
}

#[test]
fn source_backed_multi_db_root_guards_and_complete_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("first.db");
    let second = temp.path().join("second.db");
    write_database(&first, "session-a", "message-a", "alpha exact body");
    write_database(&second, "session-b", "message-b", "beta exact body");
    add_session_lineage(&first, "session-a", "middle-a", "root-a");
    let inventory = TestInventory::new(inventory(
        b"inventory-1",
        vec![
            database("project-a", &first),
            database("project-b", &second),
        ],
    ));
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    assert_eq!(frozen.databases.len(), 2);

    let first_path = std::fs::canonicalize(&first).unwrap();
    let first_database = frozen
        .databases
        .iter()
        .find(|database| database.canonical_path == first_path)
        .unwrap()
        .clone();
    let first_source = open_source(first_database).unwrap();
    let alpha_event = record_for_only_message(&first_source);
    assert_eq!(
        alpha_event.provider_session_id.as_deref(),
        Some("session-a")
    );
    assert!(alpha_event.parent_session_id.is_some());
    assert_eq!(alpha_event.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(alpha_event.root_session_id, None);
    assert_eq!(
        alpha_event.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(alpha_event.content.meaningful_text(), "alpha exact body");
    assert_eq!(
        alpha_event.native_event_id,
        Some(TypedKey::utf8("message-a").unwrap())
    );
    let encoded = String::from_utf8(alpha_event.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains("\"locator\""));
    assert!(!encoded.contains("\"source_path\""));
    assert!(finish_opened_source(first_source).unwrap());

    let second_path = std::fs::canonicalize(&second).unwrap();
    let second_database = frozen
        .databases
        .iter()
        .find(|database| database.canonical_path == second_path)
        .unwrap()
        .clone();
    let second_source = open_source(second_database).unwrap();
    let beta_event = record_for_only_message(&second_source);
    assert_eq!(beta_event.parent_session_id, None);
    assert_eq!(beta_event.agent_scope, Some(AgentScope::Primary));
    assert_eq!(beta_event.root_session_id, None);
    assert_eq!(beta_event.session_relationship, None);
    assert_eq!(beta_event.content.meaningful_text(), "beta exact body");
    assert!(finish_opened_source(second_source).unwrap());
}

#[test]
fn source_backed_replacement_keeps_ids_and_replaces_complete_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("project.db");
    write_database(&path, "session", "message", "before replacement");
    let inventory = TestInventory::new(inventory(
        b"inventory-stable",
        vec![database("project", &path)],
    ));
    let opening = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    let source = open_source(opening.databases.into_iter().next().unwrap()).unwrap();
    let before = record_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());

    let replacement = temp.path().join("replacement.db");
    write_database(&replacement, "session", "message", "after replacement");
    Connection::open(&replacement)
        .unwrap()
        .execute("update messages set rowid = 99 where id = 'message'", [])
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    let replacement = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    let source = open_source(replacement.databases.into_iter().next().unwrap()).unwrap();
    let after = record_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());
    assert_eq!(after.event_id, before.event_id);
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.native_event_id, before.native_event_id);
    assert_eq!(after.event_sequence, before.event_sequence);
    assert_eq!(before.content.meaningful_text(), "before replacement");
    assert_eq!(after.content.meaningful_text(), "after replacement");
}

fn record_for_only_message(source: &OpenedSource) -> ctx_history_core::CoreRecord {
    let frontier = CrushNativeFrontier { after_rowid: None };
    let candidate = super::super::query::next_candidate(
        source.connection().unwrap(),
        &source.schema,
        &frontier,
    )
    .unwrap()
    .unwrap();
    let CrushLoadedRow {
        row,
        session: Some(session),
        ..
    } = super::super::query::load_message_batch(
        source.connection().unwrap(),
        &source.schema,
        &[candidate],
    )
    .unwrap()
    .remove(&candidate.rowid)
    .unwrap()
    .unwrap()
    else {
        panic!("expected one parented Crush message row");
    };
    let projection = match project_message(&row, Some(&session)).unwrap() {
        CrushRecordProjection::Message(projection) => projection,
        CrushRecordProjection::Rejection => panic!("expected the test message to project"),
    };
    core_record(source, &row, &session, &projection).unwrap()
}

#[test]
fn row_local_projection_failure_becomes_a_record_rejection() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("row-local.db");
    write_database(&path, "session", "message", "body");
    Connection::open(&path)
        .unwrap()
        .execute(
            "update messages set id = ?1 where id = 'message'",
            ["x".repeat(70 * 1024)],
        )
        .unwrap();
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory(b"row-local", vec![database("project", &path)]),
    )
    .unwrap();
    let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
    let candidate = super::super::query::next_candidate(
        source.connection().unwrap(),
        &source.schema,
        &CrushNativeFrontier { after_rowid: None },
    )
    .unwrap()
    .unwrap();
    let CrushLoadedRow {
        row,
        session: Some(session),
        ..
    } = super::super::query::load_message_batch(
        source.connection().unwrap(),
        &source.schema,
        &[candidate],
    )
    .unwrap()
    .remove(&candidate.rowid)
    .unwrap()
    .unwrap()
    else {
        panic!("expected one parented Crush message row");
    };
    let CrushRecordProjection::Message(projection) = project_message(&row, Some(&session)).unwrap()
    else {
        panic!("expected the oversized identity row to reach Core projection");
    };

    let error = core_record(&source, &row, &session, &projection).unwrap_err();
    assert!(crush_row_projection_error(&error));
    assert!(finish_opened_source(source).unwrap());
}

#[test]
fn row_local_projection_filter_preserves_core_invariants() {
    assert!(crush_row_projection_error(
        &CrushSourceBackedErrorV0::Projection(ProjectionContractError::FieldTooLarge {
            field: "typed_key_utf8",
            actual: 2,
            maximum: 1,
        })
    ));
    for error in [
        CrushSourceBackedErrorV0::Projection(ProjectionContractError::SourceChanged),
        CrushSourceBackedErrorV0::Projection(ProjectionContractError::InvalidDerivedIdentity),
        CrushSourceBackedErrorV0::CoreRecord(CoreRecordError::Projection(
            ProjectionContractError::SourceChanged,
        )),
        CrushSourceBackedErrorV0::CoreRecord(CoreRecordError::InvalidSessionRelationship),
    ] {
        assert!(!crush_row_projection_error(&error), "{error:?}");
    }
}

#[test]
fn source_backed_message_round_trips_full_policy_body_and_structured_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("full-body.db");
    let text = format!("crush-head-{}-crush-tail", "x".repeat(20_000));
    let tool_arguments = json!({
        "command": format!("{}crush-structured-tail", "a".repeat(20_000)),
        "path": "src/main.rs",
    });
    write_database(&path, "session", "message", &text);
    Connection::open(&path)
        .unwrap()
        .execute(
            "update messages set parts = ?1 where id = 'message'",
            [json!([
                {"type": "text", "data": {"text": text}},
                {"type": "tool_call", "data": {"name": "shell", "input": tool_arguments}}
            ])
            .to_string()],
        )
        .unwrap();
    let inventory = TestInventory::new(inventory(
        b"full-body-inventory",
        vec![database("full-body-project", &path)],
    ));
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
    let record = record_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());
    assert_eq!(record.provider_session_id.as_deref(), Some("session"));
    assert_eq!(
        record.native_event_id,
        Some(TypedKey::utf8("message").unwrap())
    );
    assert_eq!(record.occurred_at_unix_ms, Some(1001));
    assert_eq!(record.event_type, EventType::ToolCall.as_str());
    assert_eq!(record.role.as_deref(), Some(EventRole::Assistant.as_str()));
    assert!(record.content.meaningful_text().starts_with(&text));
    assert!(record
        .content
        .meaningful_text()
        .contains("crush-tail\ntool call: shell"));
}

#[test]
fn source_backed_indivisible_result_larger_than_page_target_is_complete_once() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("large-result.db");
    let body = format!(
        "crush-large-head-{}-crush-large-tail",
        "x".repeat(8 * 1024 * 1024)
    );
    write_database(&path, "session", "message", "placeholder");
    Connection::open(&path)
        .unwrap()
        .execute(
            "update messages set role = 'tool', parts = ?1 where id = 'message'",
            [json!([{
                "type": "tool_result",
                "data": {
                    "content": body,
                    "status": "success",
                    "tool_call_id": "call-large",
                    "name": "shell"
                }
            }])
            .to_string()],
        )
        .unwrap();
    let inventory = TestInventory::new(inventory(
        b"large-result-inventory",
        vec![database("large-result-project", &path)],
    ));
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
    let record = record_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());
    assert_eq!(record.event_type, EventType::ToolOutput.as_str());
    assert!(record
        .content
        .meaningful_text()
        .starts_with("crush-large-head-"));
    assert!(record
        .content
        .meaningful_text()
        .ends_with("-crush-large-tail"));
    assert!(record.content.meaningful_text().len() > 8 * 1024 * 1024);
}

#[test]
fn stock_sqlite_snapshot_scan_sees_committed_content_retained_in_active_wal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("wal-project.db");
    write_database(&path, "wal-session", "wal-message", "main database body");
    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("pragma journal_mode = wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute_batch("pragma wal_autocheckpoint = 0")
        .unwrap();
    let parts = json!([{"type": "text", "data": {"text": "committed Crush WAL body"}}]).to_string();
    writer
        .execute(
            "update messages set parts = ?1 where id = 'wal-message'",
            [parts],
        )
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("wal-project.db-wal").exists());
    assert!(path.with_file_name("wal-project.db-shm").exists());

    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory(b"wal-inventory", vec![database("wal-project", &path)]),
    )
    .unwrap();
    let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
    let record = record_for_only_message(&source);
    assert_eq!(record.content.meaningful_text(), "committed Crush WAL body");
    assert!(finish_opened_source(source).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn stock_sqlite_snapshot_finish_precedes_publication_revalidation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("project.db");
    let replacement = temp.path().join("replacement.db");
    write_database(&path, "session", "message", "opening body");
    write_database(
        &replacement,
        "session",
        "message",
        "replacement after finish",
    );
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory(b"finish-order", vec![database("project", &path)]),
    )
    .unwrap();
    let opened = open_source(frozen.databases[0].clone()).unwrap();
    let replaced_path = path.clone();
    set_before_source_publication_revalidation(Some(Box::new(move || {
        std::fs::rename(&replacement, &replaced_path).unwrap();
    })));

    assert!(!finish_opened_source(opened).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn retained_database_leaf_fence_rejects_exact_path_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("project.db");
    let replacement = temp.path().join("replacement.db");
    write_database(&path, "session", "message", "opening body");
    write_database(&replacement, "session", "message", "replacement body");
    let frozen = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory(b"leaf-fence", vec![database("project", &path)]),
    )
    .unwrap();
    let database = &frozen.databases[0];

    std::fs::rename(&replacement, &path).unwrap();

    assert!(matches!(
        database.database_file.revalidate_same_object_leaf(),
        Err(CaptureError::SourceChangedDuringCapture)
            | Err(CaptureError::InvalidProviderTranscriptPath { .. })
    ));
}

fn inventory(
    revision: &[u8],
    databases: Vec<CrushProjectDatabaseV0>,
) -> CrushProjectInventoryObservationV0 {
    CrushProjectInventoryObservationV0::new(
        TypedKey::utf8("test-crush-project-registry").unwrap(),
        revision.to_vec(),
        databases,
    )
    .unwrap()
}

fn database(project: &str, path: &std::path::Path) -> CrushProjectDatabaseV0 {
    CrushProjectDatabaseV0::new(TypedKey::utf8(project).unwrap(), path.to_path_buf()).unwrap()
}

fn write_database(path: &std::path::Path, session_id: &str, message_id: &str, body: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                id text primary key,
                parent_session_id text,
                title text,
                prompt_tokens integer,
                completion_tokens integer,
                cost real,
                created_at integer,
                updated_at integer,
                summary_message_id text
            );
            create table messages (
                id text primary key,
                session_id text not null,
                role text not null,
                parts text not null,
                created_at integer,
                updated_at integer,
                provider text,
                model text,
                is_summary_message integer not null default 0
            );",
        )
        .unwrap();
    connection
        .execute(
            "insert into sessions (
                id, parent_session_id, title, prompt_tokens, completion_tokens,
                cost, created_at, updated_at, summary_message_id
             ) values (?1, null, 'test', 1, 1, 0, 1000, 2000, null)",
            [session_id],
        )
        .unwrap();
    let parts = json!([{"type": "text", "data": {"text": body}}]).to_string();
    connection
        .execute(
            "insert into messages (
                id, session_id, role, parts, created_at, updated_at, provider,
                model, is_summary_message
             ) values (?1, ?2, 'assistant', ?3, 1001, 1001, 'test', 'model', 0)",
            (message_id, session_id, parts),
        )
        .unwrap();
}

fn add_session_lineage(path: &std::path::Path, child: &str, parent: &str, root: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "insert into sessions (
                id, parent_session_id, title, prompt_tokens, completion_tokens,
                cost, created_at, updated_at, summary_message_id
             ) values (?1, null, 'root', 1, 1, 0, 900, 900, null)",
            [root],
        )
        .unwrap();
    connection
        .execute(
            "insert into sessions (
                id, parent_session_id, title, prompt_tokens, completion_tokens,
                cost, created_at, updated_at, summary_message_id
             ) values (?1, ?2, 'parent', 1, 1, 0, 950, 950, null)",
            (parent, root),
        )
        .unwrap();
    connection
        .execute(
            "update sessions
                set parent_session_id = ?2
              where id = ?1",
            (child, parent),
        )
        .unwrap();
}
