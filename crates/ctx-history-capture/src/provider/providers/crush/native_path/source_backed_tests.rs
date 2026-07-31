use std::sync::{Arc, Mutex};

use ctx_history_core::{BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest};
use ctx_history_index::{VerifiedIndex, WriterOptions};
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
    assert_eq!(projection_passes, 4);
    assert_eq!(snapshots.len(), 4);
    assert!(snapshots
        .iter()
        .all(|work| work.immutable_snapshot_opens == 1
            && work.copied_snapshot_opens == 0
            && work.source_bytes_copied == 0
            && work.max_active_snapshots == 1));

    Connection::open(&first)
        .unwrap()
        .execute(
            "update messages set parts = ?1 where id = 'message-a'",
            [json!([{"type": "text", "data": {"text": "alpha changed"}}]).to_string()],
        )
        .unwrap();
    refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_eq!(route_inventory.work().0, 6);

    route_inventory.replace(inventory(
        b"route-inventory-2",
        vec![database("route-project-a", &first)],
    ));
    let deleted = refresh_source_backed_generation(&index_root, &registry, options).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(route_inventory.work().0, 7);

    let source = crush_source_key(TypedKey::utf8("route-project-a").unwrap()).unwrap();
    let events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items;
    assert_eq!(events.len(), 1);
    let request =
        EventHydrationRequest::new(events[0].event_id, events[0].locator.clone()).unwrap();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(vec![request]).unwrap())
        .unwrap();
    assert_eq!(hydrated.records()[0].provider_bytes, b"alpha changed");
}

#[test]
fn source_backed_multi_db_root_guards_and_exact_hydration() {
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
    let alpha_event = document_for_only_message(&first_source);
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
    assert_eq!(alpha_event.source_path.as_deref(), first_path.to_str());
    assert_eq!(alpha_event.agent_type, AgentType::Subagent.as_str());
    assert!(!alpha_event.is_primary);
    assert!(finish_opened_source(first_source).unwrap());

    let second_path = std::fs::canonicalize(&second).unwrap();
    let second_database = frozen
        .databases
        .iter()
        .find(|database| database.canonical_path == second_path)
        .unwrap()
        .clone();
    let second_source = open_source(second_database).unwrap();
    let beta_event = document_for_only_message(&second_source);
    assert_eq!(beta_event.parent_session_id, None);
    assert_eq!(beta_event.root_session_id, beta_event.session_id);
    assert_eq!(beta_event.agent_type, AgentType::Primary.as_str());
    assert!(beta_event.is_primary);
    assert!(finish_opened_source(second_source).unwrap());

    let locator = alpha_event.locator.clone();
    let hydrated =
        CrushLocatorResolverV0::discover(crate::test_provider_sqlite_data_root(), &inventory)
            .unwrap()
            .hydrate_locators(&[&locator])
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
    assert_eq!(hydrated.provider_session_id, "session-a");
    assert_eq!(hydrated.native_record_id, "message-a");
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some("alpha exact body")
    );
}

#[test]
fn source_backed_replacement_keeps_ids_and_rejects_stale_locator() {
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
    let before = document_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());

    let replacement = temp.path().join("replacement.db");
    write_database(&replacement, "session", "message", "after replacement");
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    let replacement = bind_inventory(
        crate::test_provider_sqlite_data_root(),
        inventory.observe().unwrap(),
    )
    .unwrap();
    let source = open_source(replacement.databases.into_iter().next().unwrap()).unwrap();
    let after = document_for_only_message(&source);
    assert!(finish_opened_source(source).unwrap());
    assert_eq!(after.event_id, before.event_id);
    assert_ne!(after.locator, before.locator);
    assert!(matches!(
        CrushLocatorResolverV0::discover(crate::test_provider_sqlite_data_root(), &inventory)
            .unwrap()
            .hydrate_locators(&[&before.locator]),
        Err(CrushSourceBackedErrorV0::StaleRecordEvidence)
    ));
    let hydrated =
        CrushLocatorResolverV0::discover(crate::test_provider_sqlite_data_root(), &inventory)
            .unwrap()
            .hydrate_locators(&[&after.locator])
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some("after replacement")
    );
}

fn document_for_only_message(source: &OpenedSource) -> LexicalDocument {
    let frontier = CrushNativeFrontier {
        phase: CrushNativePhase::Messages,
        after_rowid: None,
        next_ordinal: 0,
    };
    let candidate = super::super::query::next_candidate(
        source.connection().unwrap(),
        &source.schema,
        &frontier,
    )
    .unwrap()
    .unwrap();
    let CrushHydratedRow::Message {
        row,
        session: Some(session),
        digest_values,
        ..
    } = super::super::query::hydrate_row_from_connection(
        source.connection().unwrap(),
        &source.schema,
        CrushNativePhase::Messages,
        candidate.rowid,
        candidate.observed_bytes,
    )
    .unwrap()
    else {
        panic!("expected one parented Crush message row");
    };
    let projection = match project_message(
        &row,
        Some(&session),
        &deterministic_context(&source.database.canonical_path),
    )
    .unwrap()
    {
        CrushRecordProjection::Message(projection) => projection,
        CrushRecordProjection::Rejection { .. } => {
            panic!("expected the test message to project")
        }
    };
    lexical_document(
        source,
        &load_session_parents(source.connection().unwrap(), &source.schema.session_columns)
            .unwrap(),
        &row,
        &session,
        &digest_values,
        message_record_digest_bytes(&digest_values),
        &projection,
    )
    .unwrap()
}

#[test]
fn source_backed_message_indexes_the_full_policy_body_and_hydrates_it() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("full-body.db");
    let text = format!("crush-head-{}-crush-tail", "x".repeat(3_000));
    write_database(&path, "session", "message", &text);
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
    let document = document_for_only_message(&source);
    assert_eq!(document.body, text);
    assert!(document.body.ends_with("crush-tail"));
    assert!(finish_opened_source(source).unwrap());

    let resolver =
        CrushLocatorResolverV0::discover(crate::test_provider_sqlite_data_root(), &inventory)
            .unwrap();
    let hydrated = resolver
        .hydrate_locators(&[&document.locator])
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(resolver.hydration_counters(), (1, 1));
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(text.as_str())
    );
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
    let document = document_for_only_message(&source);
    assert_eq!(document.body, "committed Crush WAL body");
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
