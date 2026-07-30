use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{EventRecord, VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use tempfile::TempDir;
use uuid::Uuid;

use super::native_path::source_backed::{
    nanoclaw_source_key, set_before_source_backed_finish_hook, NanoClawDocumentTreeAdapter,
};
use super::position::{nanoclaw_message_locator, NanoClawMessageSource};
use super::project::NanoClawSourceBackedProject;
use super::*;
use crate::complete_content::{
    source_access::set_nanoclaw_before_source_set_revalidation,
    sqlite::CompleteContentSqliteQueryBudget, AuthorizedSourceRoute, CompleteContentErrorKind,
    CompleteContentSourceFamily, CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
};
use crate::provider::source_backed::{
    family::document::register_replacement_document_tree_route_with_authority,
    refresh_source_backed_generation, SourceBackedCoordinatorError, SourceBackedProviderRegistry,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, NANOCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

const SOURCE_BACKED_LINEAGE: [u8; 32] = [0x4a; 32];

fn create_project(temp: &TempDir, name: &str, sessions: usize) -> PathBuf {
    let root = temp.path().join(name);
    let data = root.join("data");
    fs::create_dir_all(data.join("v2-sessions")).unwrap();
    let central = Connection::open(data.join("v2.db")).unwrap();
    central
        .execute_batch(
            "create table agent_groups (
                id text primary key, name text, folder text, agent_provider text
            );
            create table messaging_groups (
                id text primary key, channel_type text, platform_id text,
                instance text, name text
            );
            create table sessions (
                id text primary key, agent_group_id text not null,
                messaging_group_id text, thread_id text, agent_provider text,
                status text, container_status text, last_active integer,
                created_at integer
            );
            insert into agent_groups values (
                'ag-1', 'Personal', '/workspace/nanoclaw', 'codex'
            );
            insert into messaging_groups values (
                'mg-1', 'telegram', 'chat-1', 'default', 'DM'
            );",
        )
        .unwrap();
    for index in 0..sessions {
        central
            .execute(
                "insert into sessions values (
                    ?1, 'ag-1', 'mg-1', ?2, 'codex', 'active', 'running',
                    ?3, ?4
                )",
                rusqlite::params![
                    format!("session-{index:04}"),
                    format!("thread-{index:04}"),
                    1_782_259_202_000_i64 + index as i64,
                    1_782_259_200_000_i64 + index as i64,
                ],
            )
            .unwrap();
    }
    root
}

fn create_message_stores(root: &Path, session_id: &str) -> (PathBuf, PathBuf) {
    let session_dir = root
        .join("data")
        .join("v2-sessions")
        .join("ag-1")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let inbound_path = session_dir.join("inbound.db");
    Connection::open(&inbound_path)
        .unwrap()
        .execute_batch(
            "create table messages_in (
                id text primary key, seq integer, kind text, timestamp integer,
                status text, trigger text, platform_id text, channel_type text,
                thread_id text, content text, source_session_id text, on_wake integer
            );",
        )
        .unwrap();
    let outbound_path = session_dir.join("outbound.db");
    Connection::open(&outbound_path)
        .unwrap()
        .execute_batch(
            "create table messages_out (
                id text primary key, seq integer, in_reply_to text, timestamp integer,
                kind text, platform_id text, channel_type text, thread_id text,
                content text
            );",
        )
        .unwrap();
    (inbound_path, outbound_path)
}

fn insert_inbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_in values (
                ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                'thread', ?4, null, 0
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

fn insert_outbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_out values (
                ?1, ?2, null, ?3, 'chat', 'chat-1', 'telegram', 'thread', ?4
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

type PersistentDiskState = Vec<(PathBuf, Option<(Vec<u8>, u64, std::time::SystemTime)>)>;

fn sqlite_persistent_disk_state(databases: &[&Path]) -> PersistentDiskState {
    let mut state = Vec::new();
    for database in databases {
        // Stock WAL readers may update volatile SHM reader marks.
        for suffix in ["", "-wal", "-journal"] {
            let path = if suffix.is_empty() {
                database.to_path_buf()
            } else {
                let mut value = database.as_os_str().to_os_string();
                value.push(suffix);
                PathBuf::from(value)
            };
            let contents = match fs::read(&path) {
                Ok(contents) => {
                    let metadata = fs::metadata(&path).unwrap();
                    Some((contents, metadata.len(), metadata.modified().unwrap()))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("failed to read {}: {error}", path.display()),
            };
            state.push((path, contents));
        }
    }
    state
}

fn route_source(root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::NanoClaw,
        path: root.to_path_buf(),
        exists: true,
        source_format: NANOCLAW_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    crate::register_nanoclaw_source_backed_route(
        &mut registry,
        route_source(root),
        SOURCE_BACKED_LINEAGE,
    )
    .unwrap();
    registry
}

fn direct_registry(root: &Path) -> SourceBackedProviderRegistry {
    let adapter =
        NanoClawDocumentTreeAdapter::new(root.to_path_buf(), SOURCE_BACKED_LINEAGE).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route_with_authority(
        &mut registry,
        route_source(root),
        SourceBackedRouteSelection::ExplicitManual,
        SourceBackedSelectorAuthority::CatalogLineage,
        adapter,
    )
    .unwrap();
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn source_events(index_root: &Path) -> Vec<EventRecord> {
    let source = nanoclaw_source_key(SOURCE_BACKED_LINEAGE).unwrap();
    let mut events = VerifiedIndex::open(index_root)
        .unwrap()
        .source_event_page(&source, None, 32)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    events
}

#[test]
fn compound_locator_recovers_exact_inbound_and_outbound_content_without_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "locator", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "exact-inbound-content");
    insert_outbound(&outbound, "outbound", 2, 2_000, "exact-outbound-content");
    let inbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let outbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Outbound, 1).unwrap();
    let project = NanoClawCompleteProject::open(
        &root,
        &[inbound_locator.clone(), outbound_locator.clone()],
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    assert_eq!(
        project.resolve(&inbound_locator).unwrap().unwrap().text,
        "exact-inbound-content"
    );
    assert_eq!(
        project.resolve(&outbound_locator).unwrap().unwrap().text,
        "exact-outbound-content"
    );

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'mutated-content' where id = 'inbound'",
            [],
        )
        .unwrap();
    assert!(project.resolve(&inbound_locator).is_err());
}

#[test]
fn document_family_cold_scan_and_grouped_hydration_are_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "document-cold", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    let full_body = format!(
        "{}nanoclaw-complete-tail",
        "full compound content ".repeat(PROVIDER_MAX_TEXT_CHARS / 10)
    );
    insert_inbound(&inbound, "inbound", 1, 1_000, &full_body);
    insert_outbound(&outbound, "outbound", 2, 2_000, "outbound body");
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(receipt.sources.len(), 1);
    let certificate = &receipt.sources[0];
    assert_eq!(certificate.parser_revision(), "nanoclaw-source-backed-v1");
    assert_eq!(certificate.counts().complete_records, 3);
    assert_eq!(certificate.counts().retained_records, 2);
    assert_eq!(certificate.counts().indexed_documents, 2);
    assert!(
        certificate.frontier().is_none(),
        "compound logical snapshots must not advertise physical replay"
    );
    let evidence: serde_json::Value =
        serde_json::from_slice(certificate.observation().revision()).unwrap();
    assert_eq!(evidence["sessions"], 1);
    assert_eq!(evidence["component_databases"], 2);

    let events = source_events(&index_root);
    assert_eq!(events.len(), 2);
    let canonical_root = fs::canonicalize(&root).unwrap().display().to_string();
    for event in &events {
        assert_eq!(event.parent_session_id, None);
        assert_eq!(event.root_session_id, event.session_id);
        assert_eq!(event.provider_session_id.as_deref(), Some("thread-0000"));
        assert_eq!(event.source_path.as_deref(), Some(canonical_root.as_str()));
        assert_eq!(event.agent_type, "codex");
        assert!(event.is_primary);
        assert_eq!(
            event.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        let NativeRecordCoordinate::ProviderNative {
            namespace,
            coordinate,
        } = event.locator.coordinate()
        else {
            panic!("NanoClaw locator was not provider-native");
        };
        assert_eq!(namespace, NANOCLAW_MESSAGE_LOCATOR_KIND);
        assert!(matches!(coordinate, TypedKey::Bytes(value) if value.len() == 17));
    }

    let requests = [1_usize, 0]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(events[index].event_id, events[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap();
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        [b"outbound body".as_slice(), full_body.as_bytes()]
    );
}

#[test]
fn logical_compound_lifecycle_discards_identical_staging_and_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "document-lifecycle", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "stable", 1, 1_000, "before");
    let registry = direct_registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let stable_event_id = source_events(&index_root)[0].event_id;
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'after-' where id = 'stable'",
            [],
        )
        .unwrap();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    let changed_event = &source_events(&index_root)[0];
    assert_eq!(changed_event.event_id, stable_event_id);
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(
                &EventHydrationRequest::new(changed_event.event_id, changed_event.locator.clone(),)
                    .unwrap()
            )
            .unwrap()
            .provider_bytes,
        b"after-"
    );

    let mutate = inbound.clone();
    let _hook = set_before_source_backed_finish_hook(move || {
        Connection::open(mutate)
            .unwrap()
            .execute(
                "update messages_in set content = 'raced!' where id = 'stable'",
                [],
            )
            .unwrap();
    });
    let retained_generation = changed.commit.generation_id;
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );

    let unavailable = temp.path().join("document-lifecycle-unavailable");
    fs::rename(&root, unavailable).unwrap();
    let error =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::RouteScan { source, .. }
            if source.kind == SourceBackedRouteErrorKind::Unavailable
    ));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
}

#[test]
fn deletion_and_unavailable_hydration_have_distinct_typed_failures() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "document-hydration-errors", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "retained");
    insert_outbound(&outbound, "outbound", 2, 2_000, "deleted");
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = source_events(&index_root);
    let deleted = before[1].clone();

    Connection::open(&outbound)
        .unwrap()
        .execute("delete from messages_out where id = 'outbound'", [])
        .unwrap();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let current = source_events(&index_root);
    assert_eq!(current.len(), 1);
    let missing_locator = SourceRecordLocator::new(
        deleted.locator.source().clone(),
        deleted.locator.coordinate().clone(),
        LocatorRevisionPolicy::ExactSourceRevision,
        current[0]
            .locator
            .certified_source_revision_digest()
            .copied(),
        *deleted.locator.record_digest(),
    )
    .unwrap();
    let missing = EventHydrationRequest::new(deleted.event_id, missing_locator).unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&missing)
            .unwrap_err()
            .kind,
        HydrationFailureKind::MissingRecord
    );

    let current_request =
        EventHydrationRequest::new(current[0].event_id, current[0].locator.clone()).unwrap();
    fs::rename(
        &root,
        temp.path().join("document-hydration-errors-unavailable"),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&current_request)
            .unwrap_err()
            .kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[cfg(target_os = "linux")]
#[test]
fn compound_inventory_retains_one_root_authority_not_one_handle_per_database() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "compact-inventory", 64);
    for index in 0..64 {
        create_message_stores(&root, &format!("session-{index:04}"));
    }
    let descriptors_before = fs::read_dir("/proc/self/fd").unwrap().count();
    let mut project = NanoClawSourceBackedProject::open(&root).unwrap();
    let descriptors_open = fs::read_dir("/proc/self/fd").unwrap().count();
    assert!(
        descriptors_open <= descriptors_before + 32,
        "compound inventory retained per-database descriptors: before={descriptors_before}, open={descriptors_open}"
    );
    assert_ne!(project.physical_fingerprint(), [0_u8; 32]);
    project.finish().unwrap();
    drop(project);
    assert!(fs::read_dir("/proc/self/fd").unwrap().count() <= descriptors_before + 2);
}

#[test]
fn document_family_reads_active_wal_without_persistent_writes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "wal-consistency", 0);
    let central_path = root.join("data").join("v2.db");
    let (inbound, outbound) = create_message_stores(&root, "session-wal");
    let central_writer = Connection::open(&central_path).unwrap();
    let central_mode: String = central_writer
        .query_row("pragma journal_mode=wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(central_mode, "wal");
    central_writer
        .execute(
            "insert into sessions values (
                'session-wal', 'ag-1', 'mg-1', 'thread-wal', 'codex',
                'active', 'running', 1782259202000, 1782259200000
            )",
            [],
        )
        .unwrap();
    central_writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();

    let component_writer = Connection::open(&inbound).unwrap();
    let component_mode: String = component_writer
        .query_row("pragma journal_mode=wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(component_mode, "wal");
    let full_body = format!("{}nanoclaw-tail", "WAL content ".repeat(400));
    component_writer
        .execute(
            "insert into messages_in values (
                'wal-message', 1, 'chat', 1000, 'done', 'message', 'chat-1',
                'telegram', 'thread', ?1, null, 0
            )",
            [full_body.as_str()],
        )
        .unwrap();
    component_writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    let before = sqlite_persistent_disk_state(&[&central_path, &inbound, &outbound]);
    assert!(before.iter().any(|(path, state)| {
        path.as_os_str().to_string_lossy().ends_with("v2.db-wal") && state.is_some()
    }));
    assert!(before.iter().any(|(path, state)| {
        path.as_os_str()
            .to_string_lossy()
            .ends_with("inbound.db-wal")
            && state.is_some()
    }));

    let registry = registry(&root);
    let index_root = temp.path().join("index");
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let events = source_events(&index_root);
    assert_eq!(events.len(), 1);
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(
                &EventHydrationRequest::new(events[0].event_id, events[0].locator.clone()).unwrap()
            )
            .unwrap()
            .provider_bytes,
        full_body.as_bytes()
    );
    assert_eq!(
        sqlite_persistent_disk_state(&[&central_path, &inbound, &outbound]),
        before
    );
}

#[test]
fn production_route_is_thin_authority_aware_and_below_the_loc_gate() {
    let module_source = include_str!("../nanoclaw.rs");
    let native_path_source = include_str!("native_path.rs");
    let source_backed_source = include_str!("native_path/source_backed.rs");
    let project_source = include_str!("project.rs");
    let scanner_source = include_str!("source.rs");
    let rows_source = include_str!("rows.rs");

    assert!(!native_path_source.contains("mod lifecycle;"));
    assert!(!native_path_source.contains("mod publication;"));
    assert!(!native_path_source.contains("mod scanner;"));
    for source in [source_backed_source, scanner_source, rows_source] {
        assert!(!source.contains("ctx_history_store"));
        assert!(!source.contains("EventSearchBulkGuard"));
        assert!(!source.contains("NativePathPublicationGroup"));
    }
    assert!(!source_backed_source.contains("publication::"));
    assert!(!module_source.contains("ctx_history_store"));
    for (name, source) in [
        ("source_backed", source_backed_source),
        ("project", project_source),
        ("scanner", scanner_source),
        ("rows", rows_source),
    ] {
        assert!(source.lines().count() < 1_000, "{name} exceeded LOC gate");
    }
    for obsolete in [
        "scan_nanoclaw_source_backed",
        "NanoClawSourceBackedPage",
        "NanoClawSourceBackedReceipt",
        "Vec<LexicalDocument>",
    ] {
        assert!(
            !source_backed_source.contains(obsolete),
            "obsolete lifecycle: {obsolete}"
        );
    }
    let registration = include_str!("../../source_backed/registration/families/document.rs");
    let nanoclaw_registration = registration
        .split("pub fn register_nanoclaw_source_backed_route")
        .nth(1)
        .unwrap()
        .split("pub(super) fn register_rovodev_route")
        .next()
        .unwrap();
    assert!(!nanoclaw_registration.contains("captured_route_driver"));
    assert!(
        nanoclaw_registration.contains("register_replacement_document_tree_route_with_authority")
    );
    assert!(nanoclaw_registration.contains("SourceBackedSelectorAuthority::CatalogLineage"));
}

fn nanoclaw_broker_route(root: &Path) -> AuthorizedSourceRoute {
    AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: root.to_path_buf(),
        source_root: root.parent().map(Path::to_path_buf),
        source_identity: Some("nanoclaw-root-safety".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    }
}

fn complete_content_locator(
    locator: &crate::native_source::NativeLocator,
) -> CompleteContentSourceLocator {
    CompleteContentSourceLocator::new(locator.kind(), locator.value().to_vec()).unwrap()
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_snapshot_stays_exact_after_live_leaf_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-exact", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-snapshot");
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_broker_route(&root),
            std::slice::from_ref(&source_locator),
            event_id,
        )
        .unwrap();

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();

    let project = access
        .open_nanoclaw_project(
            std::slice::from_ref(&locator),
            CompleteContentSqliteQueryBudget::new(),
            event_id,
        )
        .unwrap();
    let resolved = project.resolve(&locator).unwrap().unwrap();
    assert_eq!(resolved.text, "inside-nanoclaw-snapshot");
    assert!(!resolved.text.contains("OUTSIDE_NANOCLAW"));
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_root_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-root", 1);
    let moved = temp.path().join("moved-broker-root");
    let replacement = create_project(&temp, "replacement-broker-root", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-root");
    let (replacement_inbound, _) = create_message_stores(&replacement, "session-0000");
    insert_inbound(
        &replacement_inbound,
        "inbound",
        1,
        1_000,
        "OUTSIDE_NANOCLAW_ROOT_MUST_NOT_ESCAPE",
    );
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let root = root.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&root, moved).unwrap();
                fs::rename(replacement, root).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_leaf_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-leaf", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-leaf");
    let moved = inbound.with_extension("moved");
    let replacement = inbound.with_extension("replacement");
    fs::copy(&inbound, &replacement).unwrap();
    Connection::open(&replacement)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_LEAF_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let inbound = inbound.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&inbound, moved).unwrap();
                fs::rename(replacement, inbound).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}
