use std::sync::{Arc, Mutex};

use ctx_history_core::{EventRole, EventType};
use ctx_history_index::{IndexError, VerifiedIndex, WriterOptions};
use rusqlite::{config::DbConfig, Connection};
use serde_json::json;

use super::*;
use crate::provider::source_backed::{
    refresh_source_backed_generation, register_crush_source_backed_route,
    SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

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

    fn replace(&self, observation: CrushProjectInventoryObservationV0) {
        *self.observation.lock().unwrap() = observation;
    }

    fn work(&self) -> (u64, Vec<CrushSnapshotWorkV0>) {
        let work = self.work.lock().unwrap();
        (work.projection_passes, work.snapshots.clone())
    }
}

impl CrushProjectInventorySourceV0 for TestInventory {
    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
        Ok(self.observation.lock().unwrap().clone())
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
fn multi_project_route_watches_every_exact_sqlite_family_and_authority_parent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_parent = temp.path().join("project-a");
    let second_parent = temp.path().join("project-b");
    std::fs::create_dir_all(&first_parent).unwrap();
    std::fs::create_dir_all(&second_parent).unwrap();
    let first = first_parent.join("crush.db");
    let second = second_parent.join("crush.db");
    write_database(&first, "session-a", "message-a", "alpha");
    write_database(&second, "session-b", "message-b", "beta");
    let inventory = Arc::new(TestInventory::new(inventory(
        b"watch-inventory",
        vec![
            database("project-a", &first),
            database("project-b", &second),
        ],
    )));
    let mut registry = SourceBackedProviderRegistry::new();
    register_crush_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Crush,
            path: first.clone(),
            exists: true,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::ExplicitManual,
        crate::test_provider_sqlite_data_root(),
        inventory,
    )
    .unwrap();

    let catalog = registry.watch_catalog();
    let (route, targets) = catalog.route_targets().next().unwrap();
    assert_eq!(targets.len(), 10);
    for database in [&first, &second] {
        assert!(targets.contains(database));
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut companion = database.as_os_str().to_os_string();
            companion.push(suffix);
            assert!(targets.contains(&std::path::PathBuf::from(companion)));
        }
    }
    assert!(targets.contains(&first_parent));
    assert!(targets.contains(&second_parent));
    assert!(catalog.certify_route_observation(route).is_none());
}

#[test]
fn inventory_route_projects_each_logical_leaf_and_deletes_safely() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("route-first.db");
    let second = temp.path().join("route-second.db");
    write_database(&first, "session-a", "message-a", "alpha");
    write_database(&second, "session-b", "message-b", "beta");
    let route_inventory = Arc::new(TestInventory::new(inventory(
        b"route-inventory-1",
        vec![
            database("route-project-a", &first),
            database("route-project-b", &second),
        ],
    )));
    let mut registry = SourceBackedProviderRegistry::new();
    register_crush_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Crush,
            path: temp.path().to_path_buf(),
            exists: true,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::ExplicitManual,
        crate::test_provider_sqlite_data_root(),
        Arc::clone(&route_inventory),
    )
    .unwrap();
    let index_root = temp.path().join("route-index");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    let cold = refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    let (projection_passes, snapshots) = route_inventory.work();
    assert_eq!(projection_passes, 2);
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots
        .iter()
        .all(|work| work.immutable_snapshot_opens == 1
            && work.copied_snapshot_opens == 0
            && work.source_bytes_copied == 0
            && work.max_active_snapshots == 1));

    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);
    assert_eq!(unchanged.sources, cold.sources);
    let (projection_passes, snapshots) = route_inventory.work();
    assert_eq!(projection_passes, 2);
    assert_eq!(snapshots.len(), 4);
    assert!(snapshots[..2]
        .iter()
        .all(|work| work.immutable_snapshot_opens == 1
            && work.copied_snapshot_opens == 0
            && work.source_bytes_copied == 0
            && work.max_active_snapshots == 1));
    assert!(snapshots[2..]
        .iter()
        .all(|work| work.immutable_snapshot_opens == 0
            && work.copied_snapshot_opens == 0
            && work.source_bytes_copied == 0
            && work.terminal_fences == 0
            && work.terminal_revalidations == 0
            && work.max_active_snapshots == 0));
    let source = crush_source_key(TypedKey::utf8("route-project-a").unwrap()).unwrap();
    let deleted_source = crush_source_key(TypedKey::utf8("route-project-b").unwrap()).unwrap();
    let pinned = VerifiedIndex::open(&index_root).unwrap();
    let cold_page = pinned.core_source_event_page(&source, None, 8).unwrap();
    assert_eq!(cold_page.items.len(), 1);
    let cold_event_id = cold_page.items[0].event_id;
    let cold_session_id = cold_page.items[0].session_id;
    assert_eq!(
        cold_page.items[0].core_record.content.meaningful_text(),
        "alpha"
    );

    let moved = temp.path().join("route-first-moved.db");
    std::fs::rename(&first, &moved).unwrap();
    assert_eq!(
        pinned
            .core_record_by_id(cold_event_id.as_uuid())
            .unwrap()
            .unwrap()
            .content
            .meaningful_text(),
        "alpha"
    );
    std::fs::rename(&moved, &first).unwrap();

    Connection::open(&first)
        .unwrap()
        .execute(
            "update messages set parts = ?1 where id = 'message-a'",
            [json!([{"type": "text", "data": {"text": "alpha changed"}}]).to_string()],
        )
        .unwrap();
    assert_eq!(
        pinned
            .core_record_by_id(cold_event_id.as_uuid())
            .unwrap()
            .unwrap()
            .content
            .meaningful_text(),
        "alpha"
    );
    refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_eq!(route_inventory.work().0, 3);
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    let rewritten_page = rewritten.core_source_event_page(&source, None, 8).unwrap();
    assert_eq!(rewritten_page.items.len(), 1);
    assert_eq!(rewritten_page.items[0].event_id, cold_event_id);
    assert_eq!(rewritten_page.items[0].session_id, cold_session_id);
    assert_eq!(
        rewritten_page.items[0]
            .core_record
            .content
            .meaningful_text(),
        "alpha changed"
    );

    route_inventory.replace(inventory(
        b"route-inventory-2",
        vec![database("route-project-a", &first)],
    ));
    let deleted = refresh_source_backed_generation(&index_root, &registry, options).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(route_inventory.work().0, 3);

    let latest = VerifiedIndex::open(&index_root).unwrap();
    let events = latest
        .core_source_event_page(&source, None, 8)
        .unwrap()
        .items;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].core_record.content.meaningful_text(),
        "alpha changed"
    );
    assert!(matches!(
        latest.core_source_event_page(&deleted_source, None, 8),
        Err(IndexError::SourceEventSourceNotRetained(_))
    ));
    let pinned_deleted = pinned
        .core_source_event_page(&deleted_source, None, 8)
        .unwrap();
    assert_eq!(pinned_deleted.items.len(), 1);
    assert_eq!(
        pinned_deleted.items[0]
            .core_record
            .content
            .meaningful_text(),
        "beta"
    );
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
    assert_ne!(
        alpha_event.parent_session_id,
        Some(alpha_event.root_session_id)
    );
    assert_ne!(alpha_event.root_session_id, alpha_event.session_id);
    assert_eq!(alpha_event.branch, None);
    assert_eq!(alpha_event.agent_type, AgentType::Subagent.as_str());
    assert!(!alpha_event.is_primary);
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
    assert_eq!(beta_event.root_session_id, beta_event.session_id);
    assert_eq!(beta_event.agent_type, AgentType::Primary.as_str());
    assert!(beta_event.is_primary);
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

fn record_for_only_message(source: &OpenedSource) -> CoreRecord {
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
        CrushRecordProjection::Rejection => {
            panic!("expected the test message to project")
        }
    };
    core_record(
        source,
        &load_session_parents(source.connection().unwrap(), &source.schema.session_columns)
            .unwrap(),
        &row,
        &session,
        &projection,
    )
    .unwrap()
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
    let registry = crush_registry(temp.path(), Arc::new(inventory.clone()));
    let index_root = temp.path().join("full-body-index");
    refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let source = crush_source_key(TypedKey::utf8("full-body-project").unwrap()).unwrap();
    let page = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_source_event_page(&source, None, 8)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let record = &page.items[0].core_record;
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
    let encoded_arguments = record
        .content
        .meaningful_text()
        .lines()
        .find_map(|line| line.strip_prefix("tool input: "))
        .expect("complete structured Crush tool arguments");
    let decoded_arguments: serde_json::Value = serde_json::from_str(encoded_arguments).unwrap();
    assert_eq!(decoded_arguments, tool_arguments);
    assert!(encoded_arguments.contains("crush-structured-tail"));
    assert_eq!(
        record
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/native_message/parts/1/data/input/path"))
            .and_then(serde_json::Value::as_str),
        Some("src/main.rs")
    );
    let structured = record.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured
            .pointer("/native_session/title")
            .and_then(serde_json::Value::as_str),
        Some("test")
    );
    assert_eq!(
        structured
            .pointer("/native_session/prompt_tokens")
            .and_then(serde_json::Value::as_i64),
        Some(1)
    );
    assert_eq!(
        structured
            .pointer("/native_session/created_at_unix_ms")
            .and_then(serde_json::Value::as_i64),
        Some(1000)
    );
    let encoded = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains("\"locator\""));
    assert!(!encoded.contains("\"source_path\""));
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
    let registry = crush_registry(temp.path(), Arc::new(inventory));
    let index_root = temp.path().join("large-result-index");
    refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let source = crush_source_key(TypedKey::utf8("large-result-project").unwrap()).unwrap();
    let page = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_source_event_page(&source, None, 8)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let record = &page.items[0].core_record;
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
    let structured = record.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured
            .pointer("/provider_native_result/result_outcome")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
    assert_eq!(
        structured
            .pointer("/provider_native_result/call_id")
            .and_then(serde_json::Value::as_str),
        Some("call-large")
    );
    assert!(!structured.to_string().contains("crush-large-head-"));
    assert!(!structured.to_string().contains("crush-large-tail"));
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

fn crush_registry(root: &Path, inventory: Arc<TestInventory>) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_crush_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Crush,
            path: root.to_path_buf(),
            exists: true,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::ExplicitManual,
        crate::test_provider_sqlite_data_root(),
        inventory,
    )
    .unwrap();
    registry
}

fn database(project: &str, path: &Path) -> CrushProjectDatabaseV0 {
    CrushProjectDatabaseV0::new(TypedKey::utf8(project).unwrap(), path.to_path_buf()).unwrap()
}

fn write_database(path: &Path, session_id: &str, message_id: &str, body: &str) {
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

fn add_session_lineage(path: &Path, child: &str, parent: &str, root: &str) {
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
