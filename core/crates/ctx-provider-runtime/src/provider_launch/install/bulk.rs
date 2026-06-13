use std::sync::Arc;
use std::time::Duration;

use ctx_managed_installs as installer;
use ctx_managed_installs::provider_install_contract;
use ctx_provider_install::install_state::{InstallId, InstallStateKind, InstallTarget};
use ctx_provider_matrix as provider_matrix;

use crate::provider_launch::resolver::is_acp_provider_id;
use crate::provider_launch::status::provider_status_for_target;

use super::dependencies::{
    seed_running_prerequisite_progress, start_contract_readiness_dependencies,
};
use super::{
    validate_install_target_allowed, validate_provider_contract_targets, ProviderInstallHost,
    StartProviderInstallError,
};

struct DeferredBulkProviderInstall {
    provider_id: String,
    install_id: InstallId,
}

pub async fn start_all_provider_installs<H>(
    state: &Arc<H>,
    target: InstallTarget,
) -> Result<Vec<(String, InstallId)>, StartProviderInstallError>
where
    H: ProviderInstallHost,
{
    validate_install_target_allowed(state, target)?;
    let mut out = Vec::new();
    let matrix = installer::ManagedInstallHost::load_provider_matrix(state.as_ref()).await;
    let current_ctx_version = installer::ManagedInstallHost::current_ctx_version(state.as_ref());
    let managed = installer::load_agent_server_config(installer::ManagedInstallHost::data_root(
        state.as_ref(),
    ))
    .await
    .map_err(|err| StartProviderInstallError {
        message: err.to_string(),
        code: Some("agent_server_config_invalid".to_string()),
    })?;
    let mut deferred_acp_repairs = Vec::new();
    for entry in &matrix.providers {
        if entry.kind != provider_matrix::ProviderMatrixEntryKind::Harness {
            continue;
        }
        if !installer::is_supported_managed_provider_for_target(&matrix, &entry.id, target) {
            continue;
        }
        let id = entry.id.as_str();
        if should_defer_acp_provider_until_stale_bridge_repair(
            &managed,
            &matrix,
            id,
            target,
            current_ctx_version.as_deref(),
        ) {
            deferred_acp_repairs.push(id.to_string());
            continue;
        }
        let issue = provider_install_contract::provider_install_viability_issue(
            installer::ManagedInstallHost::data_root(state.as_ref()),
            &managed,
            &matrix,
            id,
            target,
            current_ctx_version.as_deref(),
        );
        if let Some(issue) = issue {
            if should_defer_acp_provider_until_bridge_repair(
                &matrix,
                id,
                target,
                current_ctx_version.as_deref(),
                &issue,
            ) {
                deferred_acp_repairs.push(id.to_string());
            }
            continue;
        }
        if let Some(install_id) =
            start_bulk_provider_install_if_needed(state, &managed, &matrix, id, target).await
        {
            out.push((id.to_string(), install_id));
        }
    }

    if deferred_acp_repairs.is_empty() {
        return Ok(out);
    }

    let mut deferred_bridge_install_id = state
        .find_running_install("acp-crp-bridge", Some(target))
        .await;
    if deferred_bridge_install_id.is_none() {
        if let Some(bridge_install_id) = start_bulk_provider_install_if_needed(
            state,
            &managed,
            &matrix,
            "acp-crp-bridge",
            target,
        )
        .await
        {
            deferred_bridge_install_id = Some(bridge_install_id);
            out.push(("acp-crp-bridge".to_string(), bridge_install_id));
        }
    }

    let refreshed_managed = installer::load_agent_server_config(
        installer::ManagedInstallHost::data_root(state.as_ref()),
    )
    .await
    .map_err(|err| StartProviderInstallError {
        message: err.to_string(),
        code: Some("agent_server_config_invalid".to_string()),
    })?;
    let mut deferred_queue = Vec::new();
    for provider_id in deferred_acp_repairs {
        let issue = provider_install_contract::provider_install_viability_issue(
            installer::ManagedInstallHost::data_root(state.as_ref()),
            &refreshed_managed,
            &matrix,
            &provider_id,
            target,
            current_ctx_version.as_deref(),
        );
        let should_queue_after_repair = match issue {
            None => true,
            Some(ref issue)
                if should_defer_acp_provider_until_bridge_repair(
                    &matrix,
                    &provider_id,
                    target,
                    current_ctx_version.as_deref(),
                    issue,
                ) =>
            {
                deferred_bridge_install_id.is_some()
            }
            Some(_) => false,
        };
        if !should_queue_after_repair {
            continue;
        }

        let install_id = queue_deferred_bulk_provider_install(
            state,
            &provider_id,
            target,
            deferred_bridge_install_id,
            &mut deferred_queue,
        )
        .await;
        out.push((provider_id, install_id));
    }
    spawn_deferred_bulk_provider_installs(
        state.clone(),
        deferred_bridge_install_id,
        deferred_queue,
        target,
    );
    Ok(out)
}

fn has_provider_update_available(status: &ctx_providers::adapters::ProviderStatus) -> bool {
    let matrix_update = status
        .detail_flag("matrix_update_available")
        .unwrap_or(false);
    let dependency_update = status
        .detail_flag("managed_dependency_update_available")
        .unwrap_or(false);
    matrix_update || dependency_update
}

pub fn should_skip_install_for_healthy_provider(
    status: &ctx_providers::adapters::ProviderStatus,
) -> bool {
    status.installed
        && matches!(status.health, ctx_providers::adapters::ProviderHealth::Ok)
        && status.details.get("ready_for_use").map(String::as_str) != Some("false")
        && !has_provider_update_available(status)
}

async fn start_bulk_provider_install_if_needed<H>(
    state: &Arc<H>,
    managed: &installer::AgentServerConfigFile,
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
) -> Option<InstallId>
where
    H: ProviderInstallHost,
{
    if let Some(install_id) = state.find_running_install(provider_id, Some(target)).await {
        return Some(install_id);
    }

    let status =
        provider_status_for_target(state.as_ref(), managed, matrix, provider_id, target).await;
    if should_skip_install_for_healthy_provider(&status) {
        return None;
    }
    let current_ctx_version = installer::ManagedInstallHost::current_ctx_version(state.as_ref());
    match provider_install_contract::resolve_provider_install_contract(
        installer::ManagedInstallHost::data_root(state.as_ref()),
        managed,
        matrix,
        provider_id,
        target,
        current_ctx_version.as_deref(),
    )
    .map_err(|error| StartProviderInstallError {
        message: error.to_string(),
        code: Some("install_contract_invalid".to_string()),
    })
    .and_then(|contract| validate_provider_contract_targets(state, &contract))
    {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!(
                provider_id,
                target = target.as_str(),
                code = error.code.as_deref(),
                "skipping managed provider install because install target policy rejected it: {}",
                error.message
            );
            return None;
        }
    }

    let (install_id, started_new) = state
        .start_install(provider_id.to_string(), Some(target))
        .await;
    if started_new {
        seed_running_prerequisite_progress(state, managed, matrix, provider_id, target, install_id)
            .await;
        start_contract_readiness_dependencies(
            state,
            managed,
            matrix,
            provider_id,
            target,
            install_id,
        )
        .await;
        let state2 = state.clone();
        let provider_id = provider_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = installer::install_provider_with_progress(
                state2.clone(),
                install_id,
                provider_id.clone(),
                target,
            )
            .await
            {
                tracing::error!("provider install failed ({provider_id}): {e:#}");
            }
        });
    }

    Some(install_id)
}

fn should_defer_acp_provider_until_bridge_repair(
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
    issue: &provider_install_contract::ProviderInstallViabilityIssue,
) -> bool {
    issue.code == "acp_bridge_invalid"
        && is_acp_provider_id(provider_id)
        && installer::is_compatible_managed_provider_for_target(
            matrix,
            "acp-crp-bridge",
            target,
            current_ctx_version,
        )
}

fn should_defer_acp_provider_until_stale_bridge_repair(
    managed: &installer::AgentServerConfigFile,
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) -> bool {
    if !is_acp_provider_id(provider_id)
        || !installer::is_compatible_managed_provider_for_target(
            matrix,
            "acp-crp-bridge",
            target,
            current_ctx_version,
        )
    {
        return false;
    }
    match installer::resolve_runtime_provider_command_for_target(
        managed,
        "acp-crp-bridge",
        Some(target),
    ) {
        Ok(_) => false,
        Err(_) => matches!(
            installer::resolve_runtime_provider_command_for_target_repairable_managed(
                managed,
                "acp-crp-bridge",
                Some(target),
            ),
            Ok(None)
        ),
    }
}

async fn queue_deferred_bulk_provider_install<H>(
    state: &Arc<H>,
    provider_id: &str,
    target: InstallTarget,
    bridge_install_id: Option<InstallId>,
    queue: &mut Vec<DeferredBulkProviderInstall>,
) -> InstallId
where
    H: ProviderInstallHost,
{
    let (install_id, started_new) = state
        .start_install(provider_id.to_string(), Some(target))
        .await;
    if started_new {
        if let Some(bridge_install_id) = bridge_install_id {
            let _ = state
                .register_install_progress_mirror(bridge_install_id, install_id)
                .await;
        }
        queue.push(DeferredBulkProviderInstall {
            provider_id: provider_id.to_string(),
            install_id,
        });
    }
    install_id
}

fn spawn_deferred_bulk_provider_installs<H>(
    state: Arc<H>,
    bridge_install_id: Option<InstallId>,
    deferred_installs: Vec<DeferredBulkProviderInstall>,
    target: InstallTarget,
) where
    H: ProviderInstallHost,
{
    if deferred_installs.is_empty() {
        return;
    }
    tokio::spawn(async move {
        if let Some(bridge_install_id) = bridge_install_id {
            wait_for_install_to_finish(&state, bridge_install_id).await;
        }
        for deferred_install in deferred_installs {
            if let Err(e) = installer::install_provider_with_progress(
                state.clone(),
                deferred_install.install_id,
                deferred_install.provider_id.clone(),
                target,
            )
            .await
            {
                tracing::error!(
                    "provider install failed ({}): {e:#}",
                    deferred_install.provider_id
                );
            }
        }
    });
}

async fn wait_for_install_to_finish<H>(state: &Arc<H>, install_id: InstallId)
where
    H: ProviderInstallHost,
{
    loop {
        let Some(info) = state.get_install_info(install_id).await else {
            return;
        };
        if !matches!(info.state, InstallStateKind::Running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
