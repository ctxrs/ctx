use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
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
        family::document::register_replacement_document_tree_route,
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
};

#[test]
fn task_json_routes_cold_noop_replace_and_delete_through_shared_engine() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        lifecycle_case(provider);
    }
}

#[test]
fn task_json_grouped_hydration_is_ordered_single_scan_and_atomic() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let hydration_scans = Arc::new(AtomicUsize::new(0));
        let registry = registry(
            provider,
            fixture.source(),
            None,
            Some(Arc::clone(&hydration_scans)),
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
        assert_eq!(hydration_scans.load(Ordering::Relaxed), 1);
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
        assert_eq!(hydration_scans.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn task_json_unavailable_root_is_explicit_and_preserves_generation() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let registry = registry(provider, fixture.source(), None, None, None);
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
        let registry = registry(provider, fixture.source(), None, None, Some(hook));
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
        let registry = registry(provider, fixture.source(), None, None, None);
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
    let projection_scans = Arc::new(AtomicUsize::new(0));
    let registry = registry(
        provider,
        fixture.source(),
        Some(Arc::clone(&projection_scans)),
        None,
        None,
    );
    let index_root = fixture._temp.path().join("lifecycle-index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("cold task JSON route");
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(projection_scans.load(Ordering::Relaxed), 1);
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
        projection_scans.load(Ordering::Relaxed),
        1,
        "cheap unchanged discovery must perform zero parses"
    );

    fixture.replace_api("replacement body");
    let replacement = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect("replacement task JSON route");
    assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);
    assert_eq!(projection_scans.load(Ordering::Relaxed), 2);
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
        projection_scans.load(Ordering::Relaxed),
        2,
        "complete deletion inventory must not parse a removed source"
    );
}

fn registry(
    provider: CaptureProvider,
    source: ProviderSource,
    projection_scans: Option<Arc<AtomicUsize>>,
    hydration_scans: Option<Arc<AtomicUsize>>,
    terminal_hook: Option<Arc<dyn Fn() + Send + Sync>>,
) -> SourceBackedProviderRegistry {
    let selected = vec![source.clone()];
    let mut resolver = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_resolver(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_resolver(&selected),
        _ => unreachable!(),
    }
    .expect("task JSON resolver");
    if let Some(scans) = hydration_scans {
        resolver = resolver.with_hydration_scans(scans);
    }
    let mut adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&selected),
        _ => unreachable!(),
    }
    .with_resolver(resolver);
    if let Some(scans) = projection_scans {
        adapter = adapter.with_projection_scans(scans);
    }
    if let Some(hook) = terminal_hook {
        adapter = adapter.with_terminal_revalidation_hook(hook);
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
