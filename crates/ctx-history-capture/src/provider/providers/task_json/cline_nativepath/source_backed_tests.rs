use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    },
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, NativeRecordCoordinate,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::{
    discover_provider_sources_for_provider_with_context,
    provider::source_backed::{
        family::document::{
            register_replacement_document_tree_route, DocumentLeafExecutionPolicy,
            ReplacementDocumentTree,
        },
        refresh_source_backed_generation, SourceBackedCoordinatorError,
        SourceBackedProviderRegistry, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
    },
    provider_source_for_path, register_landed_source_backed_route, DiscoveryContext,
    DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport, ProviderImportSupport,
    ProviderSource, ProviderSourceKind, ProviderSourceStatus, CLINE_TASK_JSON_SOURCE_FORMAT,
    ROO_TASK_JSON_SOURCE_FORMAT,
};

use super::source::ClineFileStamp;
use super::source_backed::{
    cline_task_json_source_backed_adapter, cline_task_json_source_backed_resolver,
    roo_task_json_source_backed_adapter, roo_task_json_source_backed_resolver,
    TaskJsonFixtureOperations,
};

#[derive(Debug)]
pub(super) struct TaskJsonScanActivity {
    barrier: Mutex<Option<Arc<Barrier>>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl TaskJsonScanActivity {
    fn new(participants: usize) -> Arc<Self> {
        Arc::new(Self {
            barrier: Mutex::new(Some(Arc::new(Barrier::new(participants)))),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    pub(super) fn begin(self: &Arc<Self>) -> TaskJsonScanActivityGuard {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let barrier = self
            .barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        TaskJsonScanActivityGuard {
            activity: Arc::clone(self),
        }
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn disable_barrier(&self) {
        *self
            .barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

pub(super) struct TaskJsonScanActivityGuard {
    activity: Arc<TaskJsonScanActivity>,
}

impl Drop for TaskJsonScanActivityGuard {
    fn drop(&mut self) {
        let previous = self.activity.active.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0);
    }
}

#[test]
fn task_json_routes_cold_noop_replace_and_delete_through_shared_engine() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        lifecycle_case(provider);
    }
}

#[test]
fn task_json_production_routes_declare_exact_independent_leaves() {
    for adapter in [
        cline_task_json_source_backed_adapter(&[]),
        roo_task_json_source_backed_adapter(&[]),
    ] {
        assert_eq!(
            adapter.leaf_execution_policy(),
            DocumentLeafExecutionPolicy::Independent
        );
    }
}

#[test]
fn task_json_thousand_task_cold_membership_and_scan_work_is_linear() {
    const TASKS: usize = 1_000;

    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = ManyTaskFixture::new(provider, TASKS);
        let operations = Arc::new(TaskJsonFixtureOperations::default());
        let registry = registry(
            provider,
            fixture.source(),
            Some(Arc::clone(&operations)),
            None,
        );
        let index_root = fixture._temp.path().join("linear-cold-index");

        let cold = refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("cold-index 1,000 deterministic task JSON fixtures");

        assert_eq!(cold.sources.len(), TASKS);
        assert_eq!(
            operations.ordinal_membership_probes(),
            TASKS,
            "cold membership must use one stable-ordinal probe per task"
        );
        assert_eq!(
            operations.projection_scans(),
            TASKS,
            "cold projection must scan each task exactly once"
        );
        assert_eq!(operations.hydration_scans(), 0);
    }
}

#[test]
fn task_json_independent_leaves_have_one_vs_four_generation_and_event_parity() {
    const TASKS: usize = 8;

    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = ManyTaskFixture::new_with_messages(provider, TASKS);
        let serial_activity = TaskJsonScanActivity::new(1);
        let parallel_activity = TaskJsonScanActivity::new(4);
        let serial_registry = registry_with_execution(
            provider,
            fixture.source(),
            None,
            None,
            Some((1, Arc::clone(&serial_activity))),
        );
        let parallel_registry = registry_with_execution(
            provider,
            fixture.source(),
            None,
            None,
            Some((4, Arc::clone(&parallel_activity))),
        );
        let serial_root = fixture._temp.path().join("serial-leaves-index");
        let parallel_root = fixture._temp.path().join("parallel-leaves-index");

        let serial_cold =
            refresh_source_backed_generation(&serial_root, &serial_registry, writer_options())
                .expect("cold task JSON generation with one leaf worker");
        let parallel_cold =
            refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
                .expect("cold task JSON generation with four leaf workers");
        assert_eq!(
            parallel_cold.commit.generation_id,
            serial_cold.commit.generation_id
        );
        assert_eq!(parallel_cold.sources, serial_cold.sources);
        assert_eq!(
            all_retained_events(&parallel_root),
            all_retained_events(&serial_root)
        );
        assert_eq!(serial_activity.peak(), 1);
        assert_eq!(parallel_activity.peak(), 4);

        let serial_noop =
            refresh_source_backed_generation(&serial_root, &serial_registry, writer_options())
                .expect("no-op task JSON generation with one leaf worker");
        let parallel_noop =
            refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
                .expect("no-op task JSON generation with four leaf workers");
        assert_eq!(
            serial_noop.commit.generation_id,
            serial_cold.commit.generation_id
        );
        assert_eq!(
            parallel_noop.commit.generation_id,
            parallel_cold.commit.generation_id
        );

        serial_activity.disable_barrier();
        parallel_activity.disable_barrier();
        fixture.replace_task_api(3, "parallel replacement body");
        let serial_changed =
            refresh_source_backed_generation(&serial_root, &serial_registry, writer_options())
                .expect("changed task JSON generation with one leaf worker");
        let parallel_changed =
            refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
                .expect("changed task JSON generation with four leaf workers");
        assert_eq!(
            parallel_changed.commit.generation_id,
            serial_changed.commit.generation_id
        );
        assert_eq!(parallel_changed.sources, serial_changed.sources);
        let serial_events = all_retained_events(&serial_root);
        let parallel_events = all_retained_events(&parallel_root);
        assert_eq!(parallel_events, serial_events);
        assert_eq!(
            hydrate_events(&parallel_registry, &parallel_events),
            hydrate_events(&serial_registry, &serial_events)
        );

        fixture.delete_task(0);
        let serial_deleted =
            refresh_source_backed_generation(&serial_root, &serial_registry, writer_options())
                .expect("deleted task JSON generation with one leaf worker");
        let parallel_deleted =
            refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
                .expect("deleted task JSON generation with four leaf workers");
        assert_eq!(
            parallel_deleted.commit.generation_id,
            serial_deleted.commit.generation_id
        );
        assert_eq!(parallel_deleted.sources, serial_deleted.sources);
        assert_eq!(parallel_deleted.removals, serial_deleted.removals);
        assert_eq!(
            all_retained_events(&parallel_root),
            all_retained_events(&serial_root)
        );
    }
}

#[test]
fn task_json_grouped_hydration_is_ordered_single_scan_and_atomic() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let operations = Arc::new(TaskJsonFixtureOperations::default());
        let registry = registry(
            provider,
            fixture.source(),
            Some(Arc::clone(&operations)),
            None,
        );
        let index_root = fixture._temp.path().join("hydration-index");
        refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("index task JSON fixture");
        let events = retained_events(&index_root);
        assert_eq!(events.len(), 2);
        let requests = events
            .iter()
            .rev()
            .map(|event| {
                EventHydrationRequest::new(event.event_id, event.locator.clone())
                    .expect("valid task event locator")
            })
            .collect::<Vec<_>>();
        let batch = BatchHydrationRequest::new(requests.clone()).expect("valid grouped request");
        let hydrated = registry
            .resolver_registry()
            .hydrate_batch(&batch)
            .expect("hydrate exact grouped task records");
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
        assert_eq!(operations.hydration_scans(), 1);
        assert!(hydrated
            .records()
            .iter()
            .any(|record| record.provider_bytes.ends_with(b"task-json-tail")));

        fixture.replace_api("stale grouped body");
        let error = registry
            .resolver_registry()
            .hydrate_batch(&batch)
            .expect_err("changed task must fail the whole hydration group");
        assert_eq!(error.kind, HydrationFailureKind::StaleSourceEvidence);
        assert_eq!(operations.hydration_scans(), 1);
    }
}

#[test]
fn task_json_unavailable_root_is_explicit_and_preserves_generation() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let registry = registry(provider, fixture.source(), None, None);
        let index_root = fixture._temp.path().join("unavailable-index");
        let cold = refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("publish retained task JSON generation");
        let retained = event_ids(&retained_events(&index_root));
        let displaced = fixture.root.with_extension("temporarily-unavailable");
        fs::rename(&fixture.root, displaced).expect("make selected task root unavailable");
        let error = refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect_err("unavailable task root must fail closed");
        assert!(matches!(
            error,
            SourceBackedCoordinatorError::RouteScan {
                source: crate::provider::source_backed::SourceBackedRouteError {
                    kind: SourceBackedRouteErrorKind::Unavailable,
                    ..
                },
                ..
            }
        ));
        let verified = VerifiedIndex::open(&index_root).expect("reopen retained task index");
        assert_eq!(verified.generation_id(), cold.commit.generation_id);
        assert_eq!(event_ids(&retained_events(&index_root)), retained);
    }
}

#[test]
fn task_json_commit_time_inventory_race_fails_closed() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let calls = Arc::new(AtomicUsize::new(0));
        let api = fixture.api.clone();
        let hook_calls = Arc::clone(&calls);
        let hook = Arc::new(move || {
            if hook_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                replace_json(&api, &messages("commit fence race"));
            }
        });
        let registry = registry(provider, fixture.source(), None, Some(hook));
        let index_root = fixture._temp.path().join("race-index");
        let error = refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect_err("terminal task inventory race must abort publication");
        assert!(matches!(
            error,
            SourceBackedCoordinatorError::Index(_) | SourceBackedCoordinatorError::RouteScan { .. }
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the shared engine performs one terminal commit fence"
        );
    }
}

#[test]
fn task_json_catalog_is_compact_and_registration_has_no_captured_driver() {
    assert!(
        std::mem::size_of::<ClineFileStamp>() <= 40,
        "a compact file stamp must not retain an opened provider file"
    );
    let source = include_str!("source.rs");
    assert!(!source.contains("Arc<OpenedProviderSourceFile>"));

    let registration = include_str!("../../../source_backed/registration/families/document.rs");
    let task_route = registration
        .split("pub(super) fn register_task_json_route")
        .nth(1)
        .and_then(|tail| tail.split("/// Registers one explicit NanoClaw").next())
        .expect("task JSON registration body");
    assert!(!task_route.contains("captured_route_driver"));
    assert!(task_route.contains("register_replacement_document_tree_route"));
}

#[test]
fn task_json_production_registration_uses_shared_document_route() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut registry,
            fixture.source(),
            SourceBackedRouteSelection::Automatic,
        )
        .expect("register production task JSON route");
        let index_root = fixture._temp.path().join("production-registration-index");
        refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("run production task JSON route");
        assert_eq!(retained_events(&index_root).len(), 2);
    }
}

#[test]
fn source_backed_resolvers_reject_swapped_authority_roots() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let registry = registry(provider, fixture.source(), None, None);
        let index_root = fixture._temp.path().join("swap-index");
        refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("index task before root swap");
        let event = retained_events(&index_root).remove(0);
        let request = EventHydrationRequest::new(event.event_id, event.locator)
            .expect("valid task event request");
        let displaced = fixture.root.with_extension("displaced");
        fs::rename(&fixture.root, &displaced).expect("displace selected authority root");
        Fixture::write_task(provider, &fixture.task, fixture.task_id, "cold body");

        let error = registry
            .resolver_registry()
            .hydrate_event(&request)
            .expect_err("swapped root must not satisfy retained source evidence");
        assert_eq!(error.kind, HydrationFailureKind::StaleSourceEvidence);
    }
}

#[test]
fn current_cline_sdk_format_remains_detected_but_unsupported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sdk = temp.path().join("sdk-sessions");
    write_json(&sdk.join("abc/abc.json"), &json!({"id": "abc"}));
    write_json(&sdk.join("abc/abc.messages.json"), &json!([]));
    let detected = provider_source_for_path(CaptureProvider::Cline, sdk);
    assert_eq!(detected.source_kind, ProviderSourceKind::DetectionOnly);
    assert_eq!(detected.import_support, ProviderImportSupport::Unsupported);
    assert_eq!(detected.status, ProviderSourceStatus::Unsupported);
}

#[test]
fn roo_external_workspace_root_requires_discovery_consent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let external = temp.path().join("external-roo");
    fs::create_dir_all(workspace.join(".git")).expect("git boundary");
    write_json(
        &workspace.join(".vscode/settings.json"),
        &json!({"roo-cline.customStoragePath": external}),
    );
    Fixture::write_task(
        CaptureProvider::RooCode,
        &external.join("tasks/roo-external"),
        "roo-external",
        "outside consent",
    );
    let context = DiscoveryContext::new(
        &home,
        &workspace,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs {
            config: Some(home.join(".config")),
            ..DiscoveryPlatformDirs::default()
        },
    );
    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::RooCode);
    assert!(report.sources.iter().all(|source| source.path != external));
    assert!(!report.issues.is_empty());
}

fn lifecycle_case(provider: CaptureProvider) {
    let fixture = Fixture::new(provider);
    let operations = Arc::new(TaskJsonFixtureOperations::default());
    let registry = registry(
        provider,
        fixture.source(),
        Some(Arc::clone(&operations)),
        None,
    );
    let index_root = fixture._temp.path().join("lifecycle-index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("cold task JSON route");
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(operations.ordinal_membership_probes(), 1);
    assert_eq!(operations.projection_scans(), 1);
    let source = cold.sources[0].observation().source().clone();
    let cold_events = retained_events(&index_root);
    assert_eq!(cold_events.len(), 2);
    assert!(cold_events.iter().all(|event| {
        event.locator.source().exact_descriptor_eq(&source)
            && event.parent_session_id.is_none()
            && event.root_session_id == event.session_id
            && event.provider_session_id.is_some()
            && event.branch.is_none()
            && event.source_path.as_deref().is_some_and(|path| {
                matches!(
                    path,
                    "api_conversation_history.json" | "ui_messages.json" | "claude_messages.json"
                )
            })
            && event.agent_type == "primary"
            && event.is_primary
            && matches!(
                event.locator.coordinate(),
                NativeRecordCoordinate::TreeRecord { .. }
            )
    }));
    assert!(hydrate_events(&registry, &cold_events)
        .iter()
        .any(|record| record.provider_bytes.ends_with(b"task-json-tail")));
    let cold_ids = event_ids(&cold_events);

    let unchanged = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("unchanged task JSON route");
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(
        operations.ordinal_membership_probes(),
        1,
        "cheap unchanged discovery must perform zero membership probes"
    );
    assert_eq!(
        operations.projection_scans(),
        1,
        "cheap unchanged discovery must perform zero parses"
    );

    fixture.replace_api("replacement body");
    let replacement = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("replacement task JSON route");
    assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);
    assert_eq!(operations.ordinal_membership_probes(), 2);
    assert_eq!(operations.projection_scans(), 2);
    let replacement_events = retained_events(&index_root);
    assert_eq!(event_ids(&replacement_events), cold_ids);
    assert!(hydrate_events(&registry, &replacement_events)
        .iter()
        .any(|record| record.provider_bytes == b"replacement body"));

    fs::remove_dir_all(&fixture.task).expect("remove authoritative task");
    let deleted = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("delete task JSON source through complete inventory");
    assert!(deleted.sources.is_empty());
    assert!(deleted
        .removals
        .iter()
        .any(|removal| removal.deletion.source().exact_descriptor_eq(&source)));
    assert_eq!(
        operations.ordinal_membership_probes(),
        2,
        "complete deletion inventory must not probe a removed source"
    );
    assert_eq!(
        operations.projection_scans(),
        2,
        "complete deletion inventory must not parse a removed source"
    );
}

fn registry(
    provider: CaptureProvider,
    source: ProviderSource,
    operations: Option<Arc<TaskJsonFixtureOperations>>,
    terminal_hook: Option<Arc<dyn Fn() + Send + Sync>>,
) -> SourceBackedProviderRegistry {
    registry_with_execution(provider, source, operations, terminal_hook, None)
}

fn registry_with_execution(
    provider: CaptureProvider,
    source: ProviderSource,
    operations: Option<Arc<TaskJsonFixtureOperations>>,
    terminal_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    execution: Option<(usize, Arc<TaskJsonScanActivity>)>,
) -> SourceBackedProviderRegistry {
    let selected = vec![source.clone()];
    let mut resolver = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_resolver(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_resolver(&selected),
        _ => unreachable!(),
    }
    .expect("task JSON resolver");
    if let Some(operations) = operations.as_ref() {
        resolver = resolver.with_fixture_operations(Arc::clone(operations));
    }
    let mut adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&selected),
        _ => unreachable!(),
    }
    .with_resolver(resolver);
    if let Some(operations) = operations {
        adapter = adapter.with_fixture_operations(operations);
    }
    if let Some(hook) = terminal_hook {
        adapter = adapter.with_terminal_revalidation_hook(hook);
    }
    if let Some((leaf_workers, scan_activity)) = execution {
        adapter = adapter
            .with_leaf_workers(leaf_workers)
            .with_scan_activity(scan_activity);
    }
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .expect("register task JSON replacement route");
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn retained_events(index_root: &Path) -> Vec<ctx_history_index::EventRecord> {
    let index = VerifiedIndex::open(index_root).expect("open retained task index");
    let source = index.manifest().sources[0].observation().source().clone();
    let mut events = index
        .source_event_page(&source, None, 8)
        .expect("task event page")
        .items;
    events.sort_by_key(|event| event.event_sequence);
    events
}

fn all_retained_events(index_root: &Path) -> Vec<ctx_history_index::EventRecord> {
    let index = VerifiedIndex::open(index_root).expect("open retained task index");
    index
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            index
                .source_event_page(source.observation().source(), None, 8)
                .expect("task event page")
                .items
        })
        .collect()
}

fn event_ids(events: &[ctx_history_index::EventRecord]) -> Vec<ctx_history_core::StableEntityId> {
    events.iter().map(|event| event.event_id).collect()
}

fn hydrate_events(
    registry: &SourceBackedProviderRegistry,
    events: &[ctx_history_index::EventRecord],
) -> Vec<ctx_history_core::HydratedProviderRecord> {
    let requests = events
        .iter()
        .map(|event| {
            EventHydrationRequest::new(event.event_id, event.locator.clone())
                .expect("valid retained task locator")
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests).expect("valid retained task batch");
    registry
        .resolver_registry()
        .hydrate_batch(&batch)
        .expect("hydrate retained task events")
        .into_records()
}

fn exact_source(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    ProviderSource {
        provider,
        path,
        exists: true,
        source_format: match provider {
            CaptureProvider::Cline => CLINE_TASK_JSON_SOURCE_FORMAT,
            CaptureProvider::RooCode => ROO_TASK_JSON_SOURCE_FORMAT,
            _ => unreachable!(),
        },
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    task: PathBuf,
    api: PathBuf,
    provider: CaptureProvider,
    task_id: &'static str,
}

struct ManyTaskFixture {
    _temp: TempDir,
    root: PathBuf,
    provider: CaptureProvider,
}

impl ManyTaskFixture {
    fn new(provider: CaptureProvider, tasks: usize) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(match provider {
            CaptureProvider::Cline => "cline-many-data",
            CaptureProvider::RooCode => "roo-many-data",
            _ => unreachable!(),
        });
        for ordinal in 0..tasks {
            let task_id = format!("task-{ordinal:04}");
            let task = root.join("tasks").join(&task_id);
            fs::create_dir_all(&task).expect("linear task directory");
            let metadata = json!({
                "id": task_id,
                "task": format!("deterministic task metadata {ordinal:04}"),
                "workspaceDirectory": "/workspace/task-json",
                "createdAt": "2026-07-28T10:00:00Z"
            });
            write_json(
                &task.join(match provider {
                    CaptureProvider::Cline => "task_metadata.json",
                    CaptureProvider::RooCode => "history_item.json",
                    _ => unreachable!(),
                }),
                &metadata,
            );
        }
        Self {
            _temp: temp,
            root,
            provider,
        }
    }

    fn source(&self) -> ProviderSource {
        exact_source(self.provider, self.root.clone())
    }

    fn new_with_messages(provider: CaptureProvider, tasks: usize) -> Self {
        let fixture = Self::new(provider, tasks);
        for ordinal in 0..tasks {
            write_json(
                &fixture
                    .root
                    .join("tasks")
                    .join(format!("task-{ordinal:04}"))
                    .join("api_conversation_history.json"),
                &messages(&format!("parallel task body {ordinal:04}")),
            );
        }
        fixture
    }

    fn replace_task_api(&self, ordinal: usize, body: &str) {
        replace_json(
            &self
                .root
                .join("tasks")
                .join(format!("task-{ordinal:04}"))
                .join("api_conversation_history.json"),
            &messages(body),
        );
    }

    fn delete_task(&self, ordinal: usize) {
        fs::remove_dir_all(self.root.join("tasks").join(format!("task-{ordinal:04}")))
            .expect("delete deterministic task");
    }
}

impl Fixture {
    fn new(provider: CaptureProvider) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(match provider {
            CaptureProvider::Cline => "cline-data",
            CaptureProvider::RooCode => "roo-data",
            _ => unreachable!(),
        });
        let task_id = match provider {
            CaptureProvider::Cline => "cline-task",
            CaptureProvider::RooCode => "roo-task",
            _ => unreachable!(),
        };
        let task = root.join("tasks").join(task_id);
        Self::write_task(provider, &task, task_id, &source_fixture_body());
        if provider == CaptureProvider::Cline {
            write_json(
                &root.join("state/taskHistory.json"),
                &json!([{
                    "id": task_id,
                    "task": "source-backed fixture",
                    "workspaceDirectory": "/workspace/task-json"
                }]),
            );
        }
        Self {
            _temp: temp,
            root,
            api: task.join("api_conversation_history.json"),
            task,
            provider,
            task_id,
        }
    }

    fn source(&self) -> ProviderSource {
        exact_source(self.provider, self.root.clone())
    }

    fn write_task(provider: CaptureProvider, task: &Path, task_id: &str, body: &str) {
        fs::create_dir_all(task).expect("task directory");
        write_json(&task.join("api_conversation_history.json"), &messages(body));
        match provider {
            CaptureProvider::Cline => {
                write_json(&task.join("ui_messages.json"), &json!([]));
                write_json(
                    &task.join("task_metadata.json"),
                    &json!({
                        "taskId": task_id,
                        "task": "source-backed fixture",
                        "workspaceDirectory": "/workspace/task-json",
                        "createdAt": "2026-07-28T10:00:00Z"
                    }),
                );
            }
            CaptureProvider::RooCode => {
                write_json(
                    &task.join("history_item.json"),
                    &json!({
                        "id": task_id,
                        "task": "source-backed fixture",
                        "cwd": "/workspace/task-json",
                        "ts": "2026-07-28T10:00:00Z"
                    }),
                );
                write_json(
                    &task.join("_index.json"),
                    &json!({
                        "id": task_id,
                        "lastModified": "2026-07-28T10:01:00Z",
                        "model": "roo-test"
                    }),
                );
            }
            _ => unreachable!(),
        }
    }

    fn replace_api(&self, body: &str) {
        replace_json(&self.api, &messages(body));
    }
}

fn messages(body: &str) -> Value {
    json!([
        {
            "id": "stable-user-message",
            "role": "user",
            "content": body
        },
        {
            "id": "stable-assistant-message",
            "role": "assistant",
            "content": "assistant body"
        }
    ])
}

fn source_fixture_body() -> String {
    format!("{}task-json-tail", "cold body ".repeat(400))
}

fn replace_json(path: &Path, value: &Value) {
    let replacement = path.with_extension("replacement");
    write_json(&replacement, value);
    fs::rename(replacement, path).expect("atomically replace task component");
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture parent directory");
    fs::write(path, serde_json::to_vec(value).expect("serialize fixture")).expect("write fixture");
}
