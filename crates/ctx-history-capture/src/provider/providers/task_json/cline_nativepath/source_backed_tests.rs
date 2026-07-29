use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    CaptureProvider, CertifiedSourceDeletion, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, NativeRecordCoordinate,
};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::{
    discover_provider_sources_for_provider_with_context, provider_source_for_path,
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    CLINE_TASK_JSON_SOURCE_FORMAT, ROO_TASK_JSON_SOURCE_FORMAT,
};

use super::source_backed::{
    estimated_documents_bytes, TaskJsonSourceBackedCompletion, TaskJsonSourceBackedPage,
};
use super::{
    cline_task_json_source_backed_adapter, cline_task_json_source_backed_resolver,
    roo_task_json_source_backed_adapter, roo_task_json_source_backed_resolver,
};

#[test]
fn cline_source_backed_cold_exact_replacement_and_delete_ready() {
    lifecycle_case(CaptureProvider::Cline);
}

#[test]
fn roo_source_backed_cold_exact_replacement_and_delete_ready() {
    lifecycle_case(CaptureProvider::RooCode);
}

#[test]
fn source_backed_resolvers_reject_swapped_authority_roots() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let fixture = Fixture::new(provider);
        let selected = exact_source(provider, fixture.root.clone());
        let (pages, _) = scan(provider, &selected);
        let document = &pages[0].documents[0];
        let request = EventHydrationRequest::new(document.event_id, document.locator.clone())
            .expect("valid task event request");
        let resolver = resolver(provider, std::slice::from_ref(&selected));
        let displaced = fixture.root.with_extension("displaced");
        fs::rename(&fixture.root, &displaced).expect("displace selected authority root");
        Fixture::write_task(provider, &fixture.task, fixture.task_id, "cold body");

        let error = resolver
            .hydrate_event(&request)
            .expect_err("swapped root must not satisfy retained authority");
        assert_eq!(error.kind, HydrationFailureKind::StaleSourceEvidence);
    }
}

#[test]
fn unavailable_roots_remain_explicit_without_source_opens() {
    for provider in [CaptureProvider::Cline, CaptureProvider::RooCode] {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut selected = exact_source(provider, temp.path().join("unavailable"));
        selected.exists = false;
        selected.status = ProviderSourceStatus::Missing;
        let (_, completion) = scan(provider, &selected);
        assert!(completion.inventories.is_empty());
        assert!(completion.tasks.is_empty());
        assert_eq!(completion.unavailable.as_ref(), &[selected]);
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

    let mut adapter = cline_task_json_source_backed_adapter(&[detected.clone()]);
    assert_eq!(adapter.detected_but_unsupported(), &[detected]);
    assert!(adapter
        .next_page()
        .expect("drain unsupported selection")
        .is_none());
    let completion = adapter.finish().expect("finish unsupported selection");
    assert!(completion.inventories.is_empty());
    assert!(completion.tasks.is_empty());
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

    let mut adapter = roo_task_json_source_backed_adapter(&report.sources);
    assert!(adapter
        .next_page()
        .expect("drain consent-filtered roots")
        .is_none());
    let completion = adapter.finish().expect("finish consent-filtered roots");
    assert!(completion.tasks.is_empty());
    assert!(completion.inventories.is_empty());
}

fn lifecycle_case(provider: CaptureProvider) {
    let fixture = Fixture::new(provider);
    let selected = exact_source(provider, fixture.root.clone());

    let (cold_pages, cold) = scan(provider, &selected);
    assert_eq!(cold.inventories.len(), 1);
    assert_eq!(cold.tasks.len(), 1);
    assert_eq!(cold.inventories[0].observed_sources(), 1);
    assert!(cold.detected_but_unsupported.is_empty());
    assert!(cold.unavailable.is_empty());
    assert!(!cold_pages.is_empty());
    assert!(cold_pages.iter().all(|page| {
        page.documents.len() <= 64
            && estimated_documents_bytes(&page.documents) <= 4 * 1024 * 1024
            && page.documents.iter().all(|document| {
                document.parent_session_id.is_none()
                    && document.root_session_id == document.session_id
                    && document.provider_session_id.is_some()
                    && document.branch.is_none()
                    && document.source_path.as_ref().is_some_and(|path| {
                        matches!(
                            path.as_str(),
                            "api_conversation_history.json"
                                | "ui_messages.json"
                                | "claude_messages.json"
                        )
                    })
                    && document.is_primary
                    && document.agent_type
                        == match provider {
                            CaptureProvider::Cline => "cline",
                            CaptureProvider::RooCode => "roo-code",
                            _ => unreachable!(),
                        }
                    && matches!(
                        document.locator.coordinate(),
                        NativeRecordCoordinate::TreeRecord { .. }
                    )
            })
    }));
    let full_body = source_fixture_body();
    let full_document = cold_pages
        .iter()
        .flat_map(|page| &page.documents)
        .find(|document| document.body.ends_with("task-json-tail"))
        .expect("full source-backed body");
    assert_eq!(full_document.body, full_body);
    let cold_ids = event_ids(&cold_pages);
    let cold_request =
        EventHydrationRequest::new(full_document.event_id, full_document.locator.clone())
            .expect("valid cold task event request");
    let cold_source = cold.tasks[0].source.clone();
    let cold_certificate = cold.tasks[0].certified_source.clone();
    let resolver = resolver(provider, &[selected.clone()]);
    let hydrated = resolver
        .hydrate_event(&cold_request)
        .expect("hydrate exact native item");
    assert_eq!(hydrated.provider_bytes, full_document.body.as_bytes());

    let (exact_pages, exact) = scan(provider, &selected);
    assert_eq!(event_ids(&exact_pages), cold_ids);
    assert_eq!(exact.tasks[0].source, cold_source);
    assert_eq!(exact.tasks[0].certified_source, cold_certificate);

    fixture.replace_api("replacement body");
    let stale = resolver
        .hydrate_event(&cold_request)
        .expect_err("old exact locator must go stale");
    assert_eq!(stale.kind, HydrationFailureKind::StaleSourceEvidence);
    let (replacement_pages, replacement) = scan(provider, &selected);
    assert_eq!(event_ids(&replacement_pages), cold_ids);
    assert_eq!(replacement.tasks[0].source, cold_source);
    assert_ne!(
        replacement.tasks[0].certified_source.content_digest(),
        cold_certificate.content_digest()
    );

    fs::remove_dir_all(&fixture.task).expect("remove authoritative task");
    let (deleted_pages, deleted) = scan(provider, &selected);
    assert!(deleted_pages.is_empty());
    assert!(deleted.tasks.is_empty());
    assert_eq!(deleted.inventories.len(), 1);
    assert_eq!(deleted.inventories[0].observed_sources(), 0);
    let deletion = CertifiedSourceDeletion::from_inventory(cold_source, &deleted.inventories[0])
        .expect("complete inventory proves deletion");
    assert!(deletion.verifies(&deleted.inventories[0]));
}

fn scan(
    provider: CaptureProvider,
    selected: &ProviderSource,
) -> (
    Vec<TaskJsonSourceBackedPage>,
    TaskJsonSourceBackedCompletion,
) {
    let mut adapter = match provider {
        CaptureProvider::Cline => {
            cline_task_json_source_backed_adapter(std::slice::from_ref(selected))
        }
        CaptureProvider::RooCode => {
            roo_task_json_source_backed_adapter(std::slice::from_ref(selected))
        }
        _ => unreachable!(),
    };
    let mut pages = Vec::new();
    while let Some(page) = adapter.next_page().expect("stream source-backed page") {
        pages.push(page);
    }
    let completion = adapter.finish().expect("finish source-backed scan");
    (pages, completion)
}

fn resolver(
    provider: CaptureProvider,
    selected: &[ProviderSource],
) -> super::source_backed::TaskJsonSourceBackedResolver {
    match provider {
        CaptureProvider::Cline => {
            cline_task_json_source_backed_resolver(selected).expect("Cline resolver")
        }
        CaptureProvider::RooCode => {
            roo_task_json_source_backed_resolver(selected).expect("Roo resolver")
        }
        _ => unreachable!(),
    }
}

fn event_ids(pages: &[TaskJsonSourceBackedPage]) -> Vec<ctx_history_core::StableEntityId> {
    pages
        .iter()
        .flat_map(|page| page.documents.iter().map(|document| document.event_id))
        .collect()
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
        let replacement = self.task.join("api_conversation_history.replacement");
        write_json(&replacement, &messages(body));
        fs::rename(&replacement, &self.api).expect("atomically replace API history");
        assert!(self.api.is_file());
        assert!(matches!(
            self.provider,
            CaptureProvider::Cline | CaptureProvider::RooCode
        ));
        assert!(!self.task_id.is_empty());
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

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture parent directory");
    fs::write(path, serde_json::to_vec(value).expect("serialize fixture")).expect("write fixture");
}
