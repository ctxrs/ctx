use std::time::Duration;

use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_core::redaction;
use ctx_harness_sources as harness_sources;
use ctx_harness_sources::HarnessEndpointRecord;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderStatus;
use serde_json::Value;

use crate::provider_auth::provider_auth_mode;
use crate::provider_launch::config_snapshot::{
    load_provider_launch_config_snapshot, ProviderLaunchConfigError, ProviderLaunchConfigSnapshot,
};
use crate::provider_launch::options::{
    provider_options_probe_plan, provider_supports_runtime_model_catalog, ProviderOptionsProbePlan,
};
use crate::provider_launch::probe::ProviderProbeHost;
use crate::provider_launch::probe_error::classify_probe_error;
use crate::provider_options::cache::ProviderOptionsCacheSnapshot;
use crate::provider_options::response::{
    env_probe_provider_options_response, runtime_models_provider_options_response,
    selected_endpoint_runtime_launch_options_response, ProviderOptionsProbeResult,
    ProviderOptionsResponseBase,
};
use crate::provider_runtime_probe_service::{
    self, PreparedProviderRuntimeProbeError, ProviderRuntimeProbeStatus,
};
use crate::provider_usability::provider_status_is_usable;
use crate::ProviderRuntimeHost;

mod effective_preference;
mod finalization;

pub use effective_preference::effective_preferred_model_id_for_workspace_runtime;
use finalization::{
    auth_config_error_provider_options, finalize_provider_options_response,
    managed_config_error_provider_options, source_config_error_provider_options,
    unusable_provider_options, ProviderOptionsErrorContext, ProviderOptionsResponseContext,
};

pub const PROVIDER_OPTIONS_CACHE_TTL: Duration = Duration::from_secs(30);
pub const PROVIDER_OPTIONS_VERIFY_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug)]
pub enum ProviderOptionsServiceError {
    ProviderLaunchConfig(ProviderLaunchConfigError),
    SelectedEndpointMissing,
}

pub struct ProviderOptionsPreflightRequest<'a> {
    pub workspace_id: WorkspaceId,
    pub provider_id: &'a str,
    pub install_target: InstallTarget,
}

pub enum ProviderOptionsPreflight {
    Cached(Value),
    NeedsWorkspace(Box<PreparedProviderOptions>),
}

pub struct PreparedProviderOptions {
    workspace_id: WorkspaceId,
    provider_id: String,
    install_target: InstallTarget,
    launch_config: ProviderLaunchConfigSnapshot,
    cache: ProviderOptionsCacheSnapshot,
    selected_endpoint: Option<HarnessEndpointRecord>,
}

pub struct ProviderOptionsWorkspaceInput<'a> {
    pub prepared: Box<PreparedProviderOptions>,
    pub workspace: &'a Workspace,
    pub preferred_model_id: Option<String>,
}

pub async fn prepare_provider_options_response<H>(
    host: &H,
    request: ProviderOptionsPreflightRequest<'_>,
) -> Result<ProviderOptionsPreflight, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost,
{
    let ProviderOptionsPreflightRequest {
        workspace_id,
        provider_id,
        install_target,
    } = request;
    let launch_config = load_provider_launch_config_snapshot(host, provider_id).await;
    let skip_cached_config_surfaces =
        launch_config.managed_config_error.is_some() || launch_config.source_config_error.is_some();
    let cache = ProviderOptionsCacheSnapshot::load(
        host.provider_runtime(),
        workspace_id,
        install_target,
        provider_id,
        skip_cached_config_surfaces,
    )
    .await;

    if let Some(out) =
        cache.fresh_authoritative_response(PROVIDER_OPTIONS_CACHE_TTL, PROVIDER_OPTIONS_VERIFY_TTL)
    {
        return Ok(ProviderOptionsPreflight::Cached(out));
    }

    launch_config
        .ensure_known_provider(host, provider_id)
        .await
        .map_err(ProviderOptionsServiceError::ProviderLaunchConfig)?;
    let selected_endpoint = launch_config.selected_endpoint_record();

    Ok(ProviderOptionsPreflight::NeedsWorkspace(Box::new(
        PreparedProviderOptions {
            workspace_id,
            provider_id: provider_id.to_string(),
            install_target,
            launch_config,
            cache,
            selected_endpoint,
        },
    )))
}

pub async fn finish_provider_options_response<H>(
    host: &H,
    input: ProviderOptionsWorkspaceInput<'_>,
) -> Result<Value, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let ProviderOptionsWorkspaceInput {
        prepared,
        workspace,
        preferred_model_id,
    } = input;
    let PreparedProviderOptions {
        workspace_id,
        provider_id,
        install_target,
        launch_config,
        cache,
        selected_endpoint,
    } = *prepared;

    if let Some(config_error) = launch_config.managed_config_error.as_ref() {
        let out = managed_config_error_provider_options(
            host,
            ProviderOptionsErrorContext {
                provider_id: &provider_id,
                workspace_id,
                cache: &cache,
                preferred_model_id: preferred_model_id.clone(),
                verify_ttl: PROVIDER_OPTIONS_VERIFY_TTL,
            },
            config_error,
            launch_config.source_config(),
        )
        .await;
        return Ok(out);
    }

    let provider_status = launch_config
        .provider_status(host, &provider_id, install_target)
        .await;

    if let Some(config_error) = launch_config.source_config_error.as_ref() {
        let out = source_config_error_provider_options(
            host,
            ProviderOptionsErrorContext {
                provider_id: &provider_id,
                workspace_id,
                cache: &cache,
                preferred_model_id: preferred_model_id.clone(),
                verify_ttl: PROVIDER_OPTIONS_VERIFY_TTL,
            },
            &provider_status,
            config_error,
            launch_config.source_config(),
        )
        .await;
        return Ok(out);
    }

    let source_config = launch_config.source_config();
    let has_active_auth =
        match provider_runtime_probe_service::provider_has_active_auth_for_workspace_runtime(
            host,
            workspace,
            &provider_id,
            source_config,
        )
        .await
        {
            Ok(value) => value,
            Err(config_error) => {
                let config_error = redaction::redact_sensitive(&config_error);
                let out = auth_config_error_provider_options(
                    host,
                    ProviderOptionsErrorContext {
                        provider_id: &provider_id,
                        workspace_id,
                        cache: &cache,
                        preferred_model_id: preferred_model_id.clone(),
                        verify_ttl: PROVIDER_OPTIONS_VERIFY_TTL,
                    },
                    &provider_status,
                    &config_error,
                )
                .await;
                return Ok(out);
            }
        };
    let auth_mode = provider_auth_mode(has_active_auth, source_config);

    if !provider_status_is_usable(&provider_status) {
        let out = unusable_provider_options(
            host,
            ProviderOptionsErrorContext {
                provider_id: &provider_id,
                workspace_id,
                cache: &cache,
                preferred_model_id: preferred_model_id.clone(),
                verify_ttl: PROVIDER_OPTIONS_VERIFY_TTL,
            },
            &provider_status,
            has_active_auth,
            auth_mode,
            source_config,
            selected_endpoint.as_ref(),
        )
        .await;
        return Ok(out);
    }

    let use_crp_probe = provider_supports_runtime_model_catalog(&provider_id);
    let probe_context = ProviderOptionsProbeContext {
        host,
        workspace,
        provider_id: &provider_id,
        workspace_id,
        install_target,
        provider_status: &provider_status,
        has_active_auth,
        auth_mode,
        source_config,
        selected_endpoint: selected_endpoint.as_ref(),
        cache: &cache,
        preferred_model_id,
        verify_ttl: PROVIDER_OPTIONS_VERIFY_TTL,
    };

    dispatch_provider_options_probe(use_crp_probe, selected_endpoint.as_ref(), probe_context).await
}

async fn dispatch_provider_options_probe<H>(
    use_crp_probe: bool,
    selected_endpoint: Option<&HarnessEndpointRecord>,
    probe_context: ProviderOptionsProbeContext<'_, H>,
) -> Result<Value, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    match provider_options_probe_plan(
        use_crp_probe,
        selected_endpoint.map(|endpoint| endpoint.id.as_str()),
    ) {
        ProviderOptionsProbePlan::EnvOnly => env_probe_provider_options(probe_context).await,
        ProviderOptionsProbePlan::SelectedEndpointRuntimeLaunch(endpoint_id) => {
            selected_endpoint_runtime_launch_provider_options(
                probe_context,
                endpoint_id.to_string(),
            )
            .await
        }
        ProviderOptionsProbePlan::RuntimeModels => {
            runtime_models_provider_options(probe_context).await
        }
    }
}

struct ProviderOptionsProbeContext<'a, H>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    host: &'a H,
    workspace: &'a Workspace,
    provider_id: &'a str,
    workspace_id: WorkspaceId,
    install_target: InstallTarget,
    provider_status: &'a ProviderStatus,
    has_active_auth: bool,
    auth_mode: &'a str,
    source_config: Option<&'a harness_sources::HarnessProviderSourceConfig>,
    selected_endpoint: Option<&'a HarnessEndpointRecord>,
    cache: &'a ProviderOptionsCacheSnapshot,
    preferred_model_id: Option<String>,
    verify_ttl: Duration,
}

impl<'a, H> ProviderOptionsProbeContext<'a, H>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    fn response_base(&self) -> ProviderOptionsResponseBase<'_> {
        ProviderOptionsResponseBase {
            provider_id: self.provider_id,
            workspace_id: self.workspace_id,
            provider_status: self.provider_status,
            has_active_auth: self.has_active_auth,
            auth_mode: self.auth_mode,
            source_config: self.source_config,
        }
    }

    fn response_context(&self) -> ProviderOptionsResponseContext<'_> {
        ProviderOptionsResponseContext {
            provider_id: self.provider_id,
            provider_status: Some(self.provider_status),
            selected_endpoint: self.selected_endpoint,
            cache: self.cache,
            preferred_model_id: self.preferred_model_id.clone(),
        }
    }
}

async fn env_probe_provider_options<H>(
    context: ProviderOptionsProbeContext<'_, H>,
) -> Result<Value, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let status = provider_runtime_probe_service::probe_provider_options_env(
        context.host,
        context.workspace,
        context.provider_id,
    )
    .await;
    let raw_resp = env_probe_provider_options_response(
        context.response_base(),
        ProviderOptionsProbeResult {
            probe_ok: status.probe_ok,
            auth_required: status.auth_required,
            probe_error: status.probe_error,
        },
    );
    let out = finalize_provider_options_response(
        context.host,
        context.response_context(),
        raw_resp,
        true,
        context.verify_ttl,
    )
    .await;
    Ok(out)
}

async fn selected_endpoint_runtime_launch_provider_options<H>(
    context: ProviderOptionsProbeContext<'_, H>,
    endpoint_id: String,
) -> Result<Value, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let endpoint = context
        .selected_endpoint
        .ok_or(ProviderOptionsServiceError::SelectedEndpointMissing)?;
    let status = provider_runtime_probe_service::probe_selected_endpoint_runtime_launch(
        context.host,
        context.workspace,
        context.provider_id,
        context.install_target,
        endpoint_id,
    )
    .await
    .unwrap_or_else(provider_runtime_probe_error_status);
    let raw_resp = selected_endpoint_runtime_launch_options_response(
        context.response_base(),
        endpoint,
        ProviderOptionsProbeResult {
            probe_ok: status.probe_ok,
            auth_required: status.auth_required,
            probe_error: status.probe_error,
        },
    );
    let out = finalize_provider_options_response(
        context.host,
        context.response_context(),
        raw_resp,
        true,
        context.verify_ttl,
    )
    .await;
    Ok(out)
}

async fn runtime_models_provider_options<H>(
    context: ProviderOptionsProbeContext<'_, H>,
) -> Result<Value, ProviderOptionsServiceError>
where
    H: ProviderRuntimeHost + ProviderProbeHost,
{
    let probe = provider_runtime_probe_service::probe_runtime_models_for_provider_options(
        context.host,
        context.workspace,
        context.provider_id,
        context.install_target,
    )
    .await;
    let raw_resp = runtime_models_provider_options_response(
        context.provider_id,
        context.workspace_id,
        context.provider_status,
        probe,
        context.has_active_auth,
        context.auth_mode,
        context.source_config,
    );
    let out = finalize_provider_options_response(
        context.host,
        context.response_context(),
        raw_resp,
        true,
        context.verify_ttl,
    )
    .await;
    Ok(out)
}

fn provider_runtime_probe_error_status(
    error: PreparedProviderRuntimeProbeError,
) -> ProviderRuntimeProbeStatus {
    match error {
        PreparedProviderRuntimeProbeError::Verify(error) => {
            let probe_error = redaction::redact_sensitive(&error);
            let (_, auth_required, _) = classify_probe_error(&probe_error);
            ProviderRuntimeProbeStatus {
                probe_ok: false,
                auth_required: auth_required.unwrap_or(false),
                probe_error: Some(probe_error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use ctx_provider_install::install_state::InstallTarget;
    use serde_json::json;

    use super::*;
    use crate::provider_cache::workspace_provider_cache_key;
    use crate::ProviderRuntime;

    struct TestRuntimeHost {
        data_root: tempfile::TempDir,
        runtime: ProviderRuntime,
    }

    impl TestRuntimeHost {
        fn new() -> Self {
            Self {
                data_root: tempfile::tempdir().expect("tempdir"),
                runtime: ProviderRuntime::new(HashMap::new()),
            }
        }
    }

    impl ProviderRuntimeHost for TestRuntimeHost {
        fn data_root(&self) -> &Path {
            self.data_root.path()
        }

        fn current_ctx_version(&self) -> Option<String> {
            Some("test".to_string())
        }

        fn provider_runtime(&self) -> &ProviderRuntime {
            &self.runtime
        }
    }

    #[tokio::test]
    async fn preflight_returns_fresh_cache_before_known_provider_checks() {
        let host = TestRuntimeHost::new();
        let workspace_id = WorkspaceId(uuid::Uuid::new_v4());
        let provider_id = "not-a-known-provider";
        let cache_key =
            workspace_provider_cache_key(workspace_id, InstallTarget::Host, provider_id);
        host.runtime
            .store_provider_options_cache_value(
                cache_key.clone(),
                json!({
                    "provider_id": provider_id,
                    "workspace_id": workspace_id.0,
                    "probe_ok": true
                }),
            )
            .await;
        host.runtime
            .store_provider_verify_cache_value(cache_key, json!({"probe_ok": true}))
            .await;

        let out = prepare_provider_options_response(
            &host,
            ProviderOptionsPreflightRequest {
                workspace_id,
                provider_id,
                install_target: InstallTarget::Host,
            },
        )
        .await
        .expect("cached preflight succeeds before known-provider validation");

        let ProviderOptionsPreflight::Cached(value) = out else {
            panic!("expected cached response");
        };
        assert_eq!(value["provider_id"], provider_id);
        assert_eq!(value["verify"]["probe_ok"], true);
    }
}
