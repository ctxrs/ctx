use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use ctx_harness_sources::{HarnessRuntimeSourceMode, HarnessSourceKind, ResolvedHarnessSource};

pub(in crate::daemon::scheduler::runtime) struct ProviderSourceEnvResolution {
    pub(in crate::daemon::scheduler::runtime) resolved_source: ResolvedHarnessSource,
    pub(in crate::daemon::scheduler::runtime) runtime_source_mode: HarnessRuntimeSourceMode,
    pub(in crate::daemon::scheduler::runtime) using_endpoint_source: bool,
}

pub(in crate::daemon::scheduler::runtime) async fn apply_runtime_source_env(
    data_root: &Path,
    provider_id: &str,
    runtime_plan: &ctx_harness_runtime::HarnessExecutionPlan,
    provider_env: &mut HashMap<String, String>,
) -> Result<ProviderSourceEnvResolution> {
    for (key, value) in runtime_plan.env_overrides.iter() {
        provider_env.insert(key.clone(), value.clone());
    }
    ctx_mcp_command::configure_runtime_mcp_command(provider_id, provider_env, data_root)?;

    let runtime_data_root = runtime_plan.runtime_data_root();
    let resolved_source = ctx_harness_sources::resolve_provider_source_for_run_with_runtime_root(
        data_root,
        provider_id,
        runtime_data_root,
    )
    .await
    .map_err(|err| anyhow!("provider source resolution failed for {provider_id}: {err}"))?;
    let runtime_source_mode = resolved_source.runtime_source_mode();
    let using_endpoint_source = runtime_source_mode.source_kind() == HarnessSourceKind::Endpoint;
    provider_env.insert(
        "CTX_PROVIDER_SOURCE_KIND".to_string(),
        match resolved_source.source_kind {
            HarnessSourceKind::Subscription => "subscription".to_string(),
            HarnessSourceKind::Endpoint => "endpoint".to_string(),
        },
    );
    if let Some(endpoint) = resolved_source.endpoint.as_ref() {
        provider_env.insert("CTX_PROVIDER_ENDPOINT_ID".to_string(), endpoint.id.clone());
        provider_env.insert(
            "CTX_PROVIDER_ENDPOINT_SHAPE".to_string(),
            endpoint.api_shape.as_str().to_string(),
        );
    }
    for (key, value) in resolved_source.env.iter() {
        provider_env.insert(key.clone(), value.clone());
    }

    Ok(ProviderSourceEnvResolution {
        resolved_source,
        runtime_source_mode,
        using_endpoint_source,
    })
}
