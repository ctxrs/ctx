use super::*;
mod dependencies;
mod flow;
mod install_kind;
mod registry;

use self::dependencies::{
    install_provider_blocking_dependencies, wait_for_provider_readiness_dependencies,
};
pub(crate) use self::flow::install_provider_impl;
pub(crate) use self::registry::repair_install_dir;
use self::registry::update_registry_last_error;

fn apply_managed_provider_install_to_cfg(
    cfg: &mut AgentServerConfigFile,
    provider_id: &str,
    target: InstallTarget,
    managed: &ManagedProviderInstall,
    dependency_ids: &[String],
    implicit_managed_dependencies: &[(String, ManagedInstallMetadata)],
) {
    for (dependency_id, metadata) in implicit_managed_dependencies {
        cfg.managed_installs
            .insert(dependency_id.clone(), metadata.clone());
    }
    cfg.managed_install_targets
        .entry(provider_id.to_string())
        .or_default()
        .insert(target.as_str().to_string(), managed.meta.clone());
    cfg.managed_provider_targets
        .entry(provider_id.to_string())
        .or_default()
        .insert(
            target.as_str().to_string(),
            AgentServerCommand {
                command: managed.command.clone(),
                args: managed.args.clone(),
                dependencies: dependency_ids.to_vec(),
                managed: Some(managed.meta.clone()),
            },
        );
    cfg.providers.remove(provider_id);
}

async fn wait_for_tracked_install(
    state: &ManagedInstallHostObject,
    install_id: InstallId,
    provider_id: &str,
    target: InstallTarget,
    parent_install_id: Option<InstallId>,
) -> Result<()> {
    loop {
        ensure_install_not_cancelled(state, parent_install_id).await?;
        let info = state.get_install_info(install_id).await.ok_or_else(|| {
            anyhow::anyhow!(
                "tracked install {} for provider '{}' target '{}' is missing",
                install_id,
                provider_id,
                target.as_str()
            )
        })?;
        match info.state {
            InstallStateKind::Running => {
                tokio::time::sleep(INSTALL_REGISTRY_POLL_INTERVAL).await;
            }
            InstallStateKind::Succeeded => return Ok(()),
            InstallStateKind::Failed | InstallStateKind::Cancelled => {
                anyhow::bail!(
                    "tracked install {} for provider '{}' target '{}' {}: {}",
                    install_id,
                    provider_id,
                    target.as_str(),
                    match info.state {
                        InstallStateKind::Failed => "failed",
                        InstallStateKind::Cancelled => "was cancelled",
                        InstallStateKind::Running | InstallStateKind::Succeeded => unreachable!(),
                    },
                    info.error
                        .unwrap_or_else(|| "unknown install failure".to_string())
                );
            }
        }
    }
}

pub(super) async fn run_tracked_provider_install(
    state: &ManagedInstallHostObject,
    install_id: InstallId,
    provider_id: &str,
    target: InstallTarget,
) -> Result<()> {
    state.invalidate_provider_matrix_cache().await;
    let res = Box::pin(install_provider_impl(
        state,
        provider_id,
        target,
        Some(install_id),
    ))
    .await;
    match &res {
        Ok(()) => state.finish_install(install_id, true, None, None).await,
        Err(e) => {
            let code = classify_install_error("provider_install", e);
            state
                .finish_install(
                    install_id,
                    false,
                    Some(truncate_for_storage(&format!("{e:#}"), 12_000)),
                    Some(code),
                )
                .await
        }
    }
    state.invalidate_provider_matrix_cache().await;
    res
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_install<H: InstallProgressHost + ?Sized>(
    state: &H,
    install_id: Option<InstallId>,
    provider_id: &str,
    level: InstallEventLevel,
    stage: &str,
    message: String,
    bytes: Option<u64>,
    total_bytes: Option<u64>,
    attempt: Option<u32>,
) {
    emit_install_with_code(
        state,
        install_id,
        provider_id,
        level,
        stage,
        message,
        bytes,
        total_bytes,
        attempt,
        None,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_install_with_code<H: InstallProgressHost + ?Sized>(
    state: &H,
    install_id: Option<InstallId>,
    provider_id: &str,
    level: InstallEventLevel,
    stage: &str,
    message: String,
    bytes: Option<u64>,
    total_bytes: Option<u64>,
    attempt: Option<u32>,
    error_code: Option<InstallErrorCode>,
) {
    let Some(install_id) = install_id else {
        return;
    };
    let target = state
        .get_install_info(install_id)
        .await
        .and_then(|info| info.target);
    state
        .emit_install_event(
            install_id,
            InstallProgressEvent {
                install_id,
                provider_id: provider_id.to_string(),
                target,
                at: Utc::now(),
                stage: stage.to_string(),
                message,
                level,
                bytes,
                total_bytes,
                attempt,
                error_code,
            },
        )
        .await;
}

pub(super) async fn ensure_install_not_cancelled<H: InstallProgressHost + ?Sized>(
    state: &H,
    install_id: Option<InstallId>,
) -> Result<()> {
    let Some(install_id) = install_id else {
        return Ok(());
    };
    if state.is_install_cancelled(install_id).await {
        anyhow::bail!("install canceled by user");
    }
    Ok(())
}

pub(super) fn classify_install_error(stage: &str, err: &anyhow::Error) -> InstallErrorCode {
    let text = format!("{err:#}").to_ascii_lowercase();
    if text.contains("install canceled by user") {
        return InstallErrorCode::Cancelled;
    }
    if text.contains("invalid install target") {
        return InstallErrorCode::InvalidTarget;
    }
    if text.contains("unsupported provider target")
        || text.contains("unsupported dependency target")
        || text.contains("is not supported for")
    {
        return InstallErrorCode::UnsupportedTarget;
    }
    if text.contains("checksum mismatch") {
        return InstallErrorCode::ChecksumMismatch;
    }
    if text.contains("timed out") {
        return InstallErrorCode::Timeout;
    }
    if stage == "refresh" || text.contains("not healthy") {
        return InstallErrorCode::HealthCheckFailed;
    }
    if stage == "registry" || text.contains("managed install registry") {
        return InstallErrorCode::RegistryWriteFailed;
    }
    if text.contains("matrix version mismatch") || text.contains("no compatible release") {
        return InstallErrorCode::MatrixMismatch;
    }
    if text.contains("download")
        || text.contains("http error")
        || text.contains("sending request")
        || text.contains("streaming download")
    {
        return InstallErrorCode::DownloadFailed;
    }
    if text.contains("install failed")
        || text.contains("process")
        || text.contains("command")
        || text.contains("pip")
        || text.contains("npm")
    {
        return InstallErrorCode::CommandFailed;
    }
    InstallErrorCode::Unknown
}

pub async fn refresh_provider_statuses(state: &ManagedInstallHostObject) -> Result<()> {
    let cfg = load_agent_server_config(state.data_root())
        .await
        .context("loading managed install registry for provider status refresh")?;
    refresh_provider_statuses_with_cfg(state, cfg).await
}

async fn refresh_provider_statuses_with_cfg(
    state: &ManagedInstallHostObject,
    cfg: AgentServerConfigFile,
) -> Result<()> {
    let matrix = state.load_provider_matrix().await;
    let current_ctx_version = state.current_ctx_version();

    let mut statuses = HashMap::new();
    for (id, inspected_status) in state.inspect_provider_adapters().await {
        match inspected_status {
            Ok(mut status) => {
                apply_managed_install_details(&mut status, &cfg);
                if let Some(entry) = provider_matrix::get_entry(&matrix, &id) {
                    crate::provider_status_matrix::apply_matrix_to_status(
                        state.data_root(),
                        &cfg,
                        entry,
                        &mut status,
                        current_ctx_version.as_deref(),
                    )
                    .await;
                }
                statuses.insert(id.clone(), status);
            }
            Err(e) => {
                statuses.insert(
                    id.clone(),
                    ctx_providers::adapters::ProviderStatus {
                        provider_id: id.clone(),
                        installed: false,
                        detected_path: None,
                        version: None,
                        capabilities: None,
                        health: ctx_providers::adapters::ProviderHealth::Error,
                        diagnostics: vec![e.to_string()],
                        details: HashMap::new(),
                        usability: ctx_providers::adapters::ProviderUsability::default(),
                    },
                );
            }
        }
    }
    state.replace_provider_statuses(statuses).await;
    Ok(())
}
