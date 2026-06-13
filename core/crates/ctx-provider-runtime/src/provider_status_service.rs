use ctx_managed_installs as installer;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix::ProviderMatrix;
use ctx_providers::adapters::ProviderStatus;

use crate::provider_launch::status::{
    mark_provider_status_with_managed_config_error, provider_status_for_target,
};
use crate::ProviderRuntimeHost;

#[derive(Debug)]
pub enum ProviderStatusResponseError {
    NotFound { provider_id: String },
}

pub async fn refresh_provider_statuses(
    state: &ctx_managed_installs::ManagedInstallHostObject,
) -> anyhow::Result<()> {
    ctx_managed_installs::refresh_provider_statuses(state).await
}

pub async fn providers_statuses_response<H>(
    state: &H,
    target: InstallTarget,
    include_matrix_providers: bool,
) -> Vec<ProviderStatus>
where
    H: ProviderRuntimeHost,
{
    let (managed, managed_config_error) =
        crate::provider_launch::config::load_managed_agent_server_config_with_error(
            state.data_root(),
        )
        .await;
    let runtime = state.provider_runtime();
    let matrix = runtime.load_provider_matrix(state.data_root()).await;
    let provider_ids = provider_status_ids(state, &matrix, include_matrix_providers).await;
    let mut out = Vec::with_capacity(provider_ids.len());
    for provider_id in provider_ids {
        let status = if managed_config_error.is_some() {
            runtime
                .provider_status_without_target_bootstrap(&provider_id, target)
                .await
        } else {
            provider_status_for_target(state, &managed, &matrix, &provider_id, target).await
        };
        out.push(status);
    }
    decorate_provider_statuses(state, &matrix, &managed_config_error, target, &mut out).await;
    out
}

pub async fn provider_status_response<H>(
    state: &H,
    provider_id: &str,
    target: InstallTarget,
) -> Result<ProviderStatus, ProviderStatusResponseError>
where
    H: ProviderRuntimeHost,
{
    let (managed, managed_config_error) =
        crate::provider_launch::config::load_managed_agent_server_config_with_error(
            state.data_root(),
        )
        .await;
    let runtime = state.provider_runtime();
    let matrix = runtime.load_provider_matrix(state.data_root()).await;
    ensure_known_provider(state, &matrix, provider_id).await?;

    let mut status = if managed_config_error.is_some() {
        runtime
            .provider_status_without_target_bootstrap(provider_id, target)
            .await
    } else {
        provider_status_for_target(state, &managed, &matrix, provider_id, target).await
    };
    decorate_provider_runtime_details(
        state,
        &matrix,
        managed_config_error.as_deref(),
        target,
        &mut status,
    )
    .await;
    Ok(status)
}

pub async fn provider_status_ids<H>(
    state: &H,
    matrix: &ProviderMatrix,
    include_matrix_providers: bool,
) -> Vec<String>
where
    H: ProviderRuntimeHost,
{
    state
        .provider_runtime()
        .visible_provider_status_ids(matrix, include_matrix_providers)
        .await
}

pub async fn ensure_known_provider<H>(
    state: &H,
    matrix: &ProviderMatrix,
    provider_id: &str,
) -> Result<(), ProviderStatusResponseError>
where
    H: ProviderRuntimeHost,
{
    if state
        .provider_runtime()
        .is_known_provider_id(matrix, provider_id)
        .await
    {
        return Ok(());
    }

    Err(ProviderStatusResponseError::NotFound {
        provider_id: provider_id.to_string(),
    })
}

pub async fn decorate_provider_statuses<H>(
    state: &H,
    matrix: &ProviderMatrix,
    managed_config_error: &Option<String>,
    target: InstallTarget,
    statuses: &mut [ProviderStatus],
) where
    H: ProviderRuntimeHost,
{
    let show_fake = std::env::var("CTX_SHOW_FAKE_PROVIDER")
        .ok()
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
        .unwrap_or(false);
    for status in statuses {
        decorate_provider_list_status(
            state,
            matrix,
            managed_config_error.as_deref(),
            target,
            show_fake,
            status,
        )
        .await;
    }
}

pub async fn decorate_provider_list_status<H>(
    state: &H,
    matrix: &ProviderMatrix,
    managed_config_error: Option<&str>,
    target: InstallTarget,
    show_fake: bool,
    status: &mut ProviderStatus,
) where
    H: ProviderRuntimeHost,
{
    if status.provider_id == "fake" {
        status.details.insert(
            "ui_hidden".into(),
            if show_fake { "false" } else { "true" }.into(),
        );
    }
    status
        .details
        .insert("install_target".into(), target.as_str().to_string());
    decorate_provider_runtime_details(state, matrix, managed_config_error, target, status).await;
}

pub async fn decorate_provider_runtime_details<H>(
    state: &H,
    matrix: &ProviderMatrix,
    managed_config_error: Option<&str>,
    target: InstallTarget,
    status: &mut ProviderStatus,
) where
    H: ProviderRuntimeHost,
{
    if let Some(bytes) =
        installer::managed_install_download_size_bytes(matrix, &status.provider_id, target)
    {
        status
            .details
            .insert("install_download_size_bytes".into(), bytes.to_string());
    }
    let running_install = state
        .provider_runtime()
        .find_running_install(&status.provider_id, Some(target))
        .await;
    state.publish_provider_install_ops_events(running_install.ops_events);
    if let Some(install_id) = running_install.install_id {
        status
            .details
            .insert("install_running".into(), "true".into());
        status
            .details
            .insert("install_id".into(), install_id.to_string());
    }
    if let Some(config_error) = managed_config_error {
        mark_provider_status_with_managed_config_error(status, config_error);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use ctx_provider_install::install_state::{
        InstallEventLevel, InstallId, InstallProgressEvent, InstallState, InstallTarget,
    };
    use ctx_provider_matrix::{ProviderMatrix, ProviderMatrixEntry, ProviderMatrixEntryKind};
    use ctx_providers::adapters::{ProviderHealth, ProviderStatus, ProviderUsability};

    use super::*;
    use crate::provider_install_tracker::ProviderInstallOpsEvent;
    use crate::ProviderRuntime;

    struct TestProviderRuntimeHost {
        data_root: tempfile::TempDir,
        runtime: ProviderRuntime,
        published_install_events: Mutex<Vec<ProviderInstallOpsEvent>>,
    }

    impl TestProviderRuntimeHost {
        fn new() -> Self {
            Self {
                data_root: tempfile::tempdir().expect("create provider-runtime tempdir"),
                runtime: ProviderRuntime::new(HashMap::new()),
                published_install_events: Mutex::new(Vec::new()),
            }
        }

        fn published_install_events(&self) -> Vec<ProviderInstallOpsEvent> {
            self.published_install_events
                .lock()
                .expect("install event lock poisoned")
                .clone()
        }
    }

    impl ProviderRuntimeHost for TestProviderRuntimeHost {
        fn data_root(&self) -> &Path {
            self.data_root.path()
        }

        fn current_ctx_version(&self) -> Option<String> {
            None
        }

        fn provider_runtime(&self) -> &ProviderRuntime {
            &self.runtime
        }

        fn publish_provider_install_ops_events(&self, events: Vec<ProviderInstallOpsEvent>) {
            self.published_install_events
                .lock()
                .expect("install event lock poisoned")
                .extend(events);
        }
    }

    fn test_matrix(entries: &[(&str, ProviderMatrixEntryKind)]) -> ProviderMatrix {
        ProviderMatrix {
            version: 3,
            generated_at: None,
            providers: entries
                .iter()
                .map(|(provider_id, kind)| ProviderMatrixEntry {
                    id: (*provider_id).to_string(),
                    kind: *kind,
                    display_name: None,
                    tier: None,
                    command: None,
                    managed_install: None,
                    provider_dependencies: Vec::new(),
                    dependencies: Vec::new(),
                    version_probe: None,
                    releases: Vec::new(),
                })
                .collect(),
        }
    }

    fn provider_status(provider_id: &str) -> ProviderStatus {
        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        }
    }

    #[tokio::test]
    async fn known_provider_accepts_runtime_statuses_and_matrix_entries() {
        let host = TestProviderRuntimeHost::new();
        let matrix = test_matrix(&[("matrix-provider", ProviderMatrixEntryKind::Harness)]);
        host.runtime
            .upsert_provider_status(
                "runtime-provider".to_string(),
                provider_status("runtime-provider"),
            )
            .await;

        assert!(ensure_known_provider(&host, &matrix, "runtime-provider")
            .await
            .is_ok());
        assert!(ensure_known_provider(&host, &matrix, "matrix-provider")
            .await
            .is_ok());
        let err = ensure_known_provider(&host, &matrix, "missing")
            .await
            .expect_err("missing provider should be rejected");
        assert!(matches!(
            err,
            ProviderStatusResponseError::NotFound { provider_id } if provider_id == "missing"
        ));
    }

    #[tokio::test]
    async fn list_decoration_marks_fake_visibility_and_install_target() {
        let host = TestProviderRuntimeHost::new();
        let matrix = test_matrix(&[]);
        let target = InstallTarget::Host;

        let mut hidden = provider_status("fake");
        decorate_provider_list_status(&host, &matrix, None, target, false, &mut hidden).await;
        assert_eq!(
            hidden.details.get("ui_hidden").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            hidden.details.get("install_target").map(String::as_str),
            Some(target.as_str())
        );

        let mut visible = provider_status("fake");
        decorate_provider_list_status(&host, &matrix, None, target, true, &mut visible).await;
        assert_eq!(
            visible.details.get("ui_hidden").map(String::as_str),
            Some("false")
        );
    }

    #[tokio::test]
    async fn runtime_details_publish_stale_install_reconciliation_events() {
        let host = TestProviderRuntimeHost::new();
        let matrix = test_matrix(&[]);
        let install_id = InstallId::new_v4();
        let now = chrono::Utc::now();
        let mut install = InstallState::new("mistral".to_string(), Some(InstallTarget::Container));
        install.started_at = now - chrono::Duration::minutes(9);
        install.events.push_back(InstallProgressEvent {
            install_id,
            provider_id: "mistral".to_string(),
            target: Some(InstallTarget::Container),
            at: now - chrono::Duration::minutes(8),
            stage: "venv".to_string(),
            message: "Creating virtualenv...".to_string(),
            level: InstallEventLevel::Info,
            bytes: None,
            total_bytes: None,
            attempt: None,
            error_code: None,
        });
        host.runtime
            .insert_install_state_for_testing(install_id, install)
            .await;

        let mut status = provider_status("mistral");
        decorate_provider_runtime_details(
            &host,
            &matrix,
            None,
            InstallTarget::Container,
            &mut status,
        )
        .await;

        assert!(!status.details.contains_key("install_running"));
        let events = host.published_install_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "provider_install_failed");
        assert_eq!(events[0].provider_id, "mistral");
    }
}
