use ctx_core::models::ExecutionEnvironment as SessionExecutionEnvironment;
use ctx_provider_install::install_state::InstallTarget;
use ctx_settings_model::{ContainerNetworkMode, ExecutionMode, ExecutionSettings, Settings};
use ctx_store::Store;
use ctx_workspace_config as workspace_config;

use crate::{load_settings, ExecutionPolicyDenied, HostExecutionPolicy};

#[derive(Debug)]
pub enum EffectiveExecutionSettingsError {
    InvalidWorkspaceOverride(anyhow::Error),
    Internal(anyhow::Error),
}

impl EffectiveExecutionSettingsError {
    pub fn into_inner(self) -> anyhow::Error {
        match self {
            Self::InvalidWorkspaceOverride(err) | Self::Internal(err) => err,
        }
    }
}

#[derive(Debug)]
pub enum WorkspaceExecutionConfigSnapshotError {
    InvalidWorkspaceConfig(anyhow::Error),
    RequestOrPolicy(anyhow::Error),
    Internal(anyhow::Error),
}

#[derive(Debug)]
pub enum WorkspaceExecutionConfigUpdateError {
    RequestOrPolicy(anyhow::Error),
    Persistence(anyhow::Error),
}

pub async fn workspace_execution_config_snapshot_for_loaded_settings(
    settings_data: &Settings,
    workspace_store: &Store,
) -> Result<workspace_config::ExecutionConfigSnapshot, WorkspaceExecutionConfigSnapshotError> {
    let mut effective = settings_data.execution.clone().unwrap_or_default();
    let mut source = "daemon_default".to_string();
    match workspace_config::load_execution_settings_override(workspace_store).await {
        Ok(Some(override_config)) => {
            apply_workspace_execution_settings_override(&mut effective, &override_config)
                .map_err(WorkspaceExecutionConfigSnapshotError::RequestOrPolicy)?;
            source = "workspace".to_string();
        }
        Ok(None) => {}
        Err(error) if workspace_config::is_workspace_runtime_settings_parse_error(&error) => {
            return Err(WorkspaceExecutionConfigSnapshotError::InvalidWorkspaceConfig(error));
        }
        Err(error) => return Err(WorkspaceExecutionConfigSnapshotError::Internal(error)),
    }
    Ok(workspace_config::project_execution_config(
        source, &effective,
    ))
}

pub async fn update_workspace_execution_config_for_loaded_settings(
    settings_data: &Settings,
    workspace_store: &Store,
    update: workspace_config::ExecutionConfigUpdateInput,
) -> Result<(), WorkspaceExecutionConfigUpdateError> {
    let effective = settings_data.execution.clone().unwrap_or_default();
    let requested_override = workspace_config::execution_settings_override_from_update(&update);
    validate_workspace_execution_settings_override(&effective, &requested_override)
        .map_err(WorkspaceExecutionConfigUpdateError::RequestOrPolicy)?;
    let update = workspace_config::execution_config_update_from_input(update);
    workspace_config::update_execution_config(workspace_store, update)
        .await
        .map_err(WorkspaceExecutionConfigUpdateError::Persistence)
}

pub async fn effective_execution_settings_classified(
    global_store: &Store,
    workspace_store: &Store,
) -> Result<ExecutionSettings, EffectiveExecutionSettingsError> {
    let settings_data = load_settings(global_store)
        .await
        .map_err(EffectiveExecutionSettingsError::Internal)?;
    effective_execution_settings_for_loaded_settings(settings_data, workspace_store).await
}

async fn effective_execution_settings_for_loaded_settings(
    settings_data: Settings,
    workspace_store: &Store,
) -> Result<ExecutionSettings, EffectiveExecutionSettingsError> {
    let mut effective = settings_data.execution.clone().unwrap_or_default();
    if let Some(ov) = workspace_config::load_execution_settings_override(workspace_store)
        .await
        .map_err(EffectiveExecutionSettingsError::InvalidWorkspaceOverride)?
    {
        apply_workspace_execution_settings_override(&mut effective, &ov)
            .map_err(EffectiveExecutionSettingsError::InvalidWorkspaceOverride)?;
    }
    HostExecutionPolicy::current()
        .and_then(|policy| policy.validate_execution_settings(&effective))
        .map_err(EffectiveExecutionSettingsError::InvalidWorkspaceOverride)?;
    Ok(effective)
}

pub async fn effective_execution_settings(
    global_store: &Store,
    workspace_store: &Store,
) -> anyhow::Result<ExecutionSettings> {
    effective_execution_settings_classified(global_store, workspace_store)
        .await
        .map_err(EffectiveExecutionSettingsError::into_inner)
}

pub fn install_target_for_settings(settings: &ExecutionSettings) -> InstallTarget {
    if matches!(settings.mode, ExecutionMode::Sandbox) {
        InstallTarget::Container
    } else {
        InstallTarget::Host
    }
}

pub async fn effective_install_target(
    global_store: &Store,
    workspace_store: &Store,
) -> anyhow::Result<InstallTarget> {
    let effective = effective_execution_settings(global_store, workspace_store).await?;
    Ok(install_target_for_settings(&effective))
}

pub fn apply_execution_environment(
    settings: &mut ExecutionSettings,
    execution_environment: SessionExecutionEnvironment,
) {
    match execution_environment {
        SessionExecutionEnvironment::Host => {
            settings.mode = ExecutionMode::Host;
        }
        SessionExecutionEnvironment::Sandbox => {
            settings.mode = ExecutionMode::Sandbox;
            settings.container.mount_mode = ctx_settings_model::ContainerMountMode::DiskIsolated;
        }
    }
}

pub fn validate_execution_environment_against_settings(
    settings: &ExecutionSettings,
    execution_environment: SessionExecutionEnvironment,
) -> anyhow::Result<()> {
    HostExecutionPolicy::current()?.validate_execution_environment(execution_environment)?;
    if matches!(settings.mode, ExecutionMode::Sandbox)
        && matches!(execution_environment, SessionExecutionEnvironment::Host)
    {
        return Err(ExecutionPolicyDenied::new(
            "session execution environment host is not allowed when effective daemon execution mode is sandbox",
        )
        .into());
    }
    Ok(())
}

pub async fn effective_execution_settings_for_environment(
    global_store: &Store,
    workspace_store: &Store,
    execution_environment: SessionExecutionEnvironment,
) -> anyhow::Result<ExecutionSettings> {
    let mut effective = effective_execution_settings(global_store, workspace_store).await?;
    validate_execution_environment_against_settings(&effective, execution_environment)?;
    apply_execution_environment(&mut effective, execution_environment);
    Ok(effective)
}

pub async fn effective_install_target_for_environment(
    global_store: &Store,
    workspace_store: &Store,
    execution_environment: SessionExecutionEnvironment,
) -> anyhow::Result<InstallTarget> {
    let effective = effective_execution_settings_for_environment(
        global_store,
        workspace_store,
        execution_environment,
    )
    .await?;
    Ok(install_target_for_settings(&effective))
}

pub fn apply_workspace_execution_settings_override(
    settings: &mut ExecutionSettings,
    ov: &workspace_config::ExecutionSettingsOverride,
) -> anyhow::Result<()> {
    let ov = normalize_persisted_workspace_execution_settings_override(ov)?;
    validate_workspace_execution_settings_override(settings, &ov)?;
    workspace_config::apply_execution_settings_override(settings, &ov);
    Ok(())
}

fn normalize_persisted_workspace_execution_settings_override(
    ov: &workspace_config::ExecutionSettingsOverride,
) -> anyhow::Result<workspace_config::ExecutionSettingsOverride> {
    let mut normalized = ov.clone();
    // Persisted host overrides predate the daemon-owned sandbox-only gate. Treat them as stale
    // reads; new host writes still go through strict validation before persistence.
    if matches!(
        HostExecutionPolicy::current()?,
        HostExecutionPolicy::SandboxOnly
    ) && matches!(normalized.mode, Some(ExecutionMode::Host))
    {
        normalized.mode = Some(ExecutionMode::Sandbox);
        normalized.container = workspace_config::ContainerExecutionSettingsOverride::default();
    }
    Ok(normalized)
}

pub fn validate_workspace_execution_settings_override(
    settings: &ExecutionSettings,
    ov: &workspace_config::ExecutionSettingsOverride,
) -> anyhow::Result<()> {
    if matches!(settings.mode, ExecutionMode::Sandbox) {
        if matches!(ov.mode, Some(ExecutionMode::Host)) {
            return Err(ExecutionPolicyDenied::new(
                "workspace execution override cannot select host when daemon execution mode is sandbox",
            )
            .into());
        }
        validate_sandbox_network_override(settings, ov)?;
    }
    let mut effective = settings.clone();
    workspace_config::apply_execution_settings_override(&mut effective, ov);
    HostExecutionPolicy::current()?.validate_execution_settings(&effective)?;
    Ok(())
}

fn validate_sandbox_network_override(
    settings: &ExecutionSettings,
    ov: &workspace_config::ExecutionSettingsOverride,
) -> anyhow::Result<()> {
    let requested_network = ov
        .container
        .network_mode
        .as_ref()
        .unwrap_or(&settings.container.network_mode);
    match (&settings.container.network_mode, requested_network) {
        (ContainerNetworkMode::LlmOnly, ContainerNetworkMode::LlmOnly) => Ok(()),
        (ContainerNetworkMode::LlmOnly, requested) => Err(ExecutionPolicyDenied::new(format!(
            "workspace execution override cannot broaden sandbox network mode from llm_only to {}",
            network_mode_label(requested)
        ))
        .into()),
        (ContainerNetworkMode::Allowlist, ContainerNetworkMode::All) => Err(
            ExecutionPolicyDenied::new(
                "workspace execution override cannot broaden sandbox network mode from allowlist to all",
            )
            .into(),
        ),
        (ContainerNetworkMode::Allowlist, ContainerNetworkMode::Allowlist) => {
            validate_allowlist_subset(&settings.container.allowlist, &ov.container.allowlist)
        }
        (ContainerNetworkMode::Allowlist, ContainerNetworkMode::LlmOnly) => Ok(()),
        (ContainerNetworkMode::All, _) => Ok(()),
    }
}

fn validate_allowlist_subset(
    daemon_allowlist: &[String],
    workspace_allowlist: &Option<Vec<String>>,
) -> anyhow::Result<()> {
    let Some(workspace_allowlist) = workspace_allowlist else {
        return Ok(());
    };
    let allowed = daemon_allowlist
        .iter()
        .filter_map(|value| trimmed_nonempty(value))
        .collect::<std::collections::BTreeSet<_>>();
    for entry in workspace_allowlist
        .iter()
        .filter_map(|value| trimmed_nonempty(value))
    {
        if !allowed.contains(&entry) {
            return Err(ExecutionPolicyDenied::new(format!(
                "workspace execution allowlist entry `{entry}` is not allowed by daemon sandbox allowlist"
            ))
            .into());
        }
    }
    Ok(())
}

fn trimmed_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn network_mode_label(mode: &ContainerNetworkMode) -> &'static str {
    match mode {
        ContainerNetworkMode::LlmOnly => "llm_only",
        ContainerNetworkMode::Allowlist => "allowlist",
        ContainerNetworkMode::All => "all",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ctx_settings_model::ContainerExecutionSettings;
    use ctx_workspace_config::ExecutionSettingsOverride;

    #[test]
    fn install_target_tracks_execution_mode() {
        let host = ExecutionSettings {
            mode: ExecutionMode::Host,
            ..ExecutionSettings::default()
        };
        let sandbox = ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        };

        assert_eq!(install_target_for_settings(&host), InstallTarget::Host);
        assert_eq!(
            install_target_for_settings(&sandbox),
            InstallTarget::Container
        );
    }

    #[test]
    fn sandbox_override_cannot_broaden_network_allowlist() {
        let settings = ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                network_mode: ContainerNetworkMode::Allowlist,
                allowlist: vec!["api.openai.com".to_string()],
                ..ContainerExecutionSettings::default()
            },
        };
        let override_config = ExecutionSettingsOverride {
            container: ctx_workspace_config::ContainerExecutionSettingsOverride {
                network_mode: Some(ContainerNetworkMode::All),
                ..ctx_workspace_config::ContainerExecutionSettingsOverride::default()
            },
            ..ExecutionSettingsOverride::default()
        };

        let err = validate_workspace_execution_settings_override(&settings, &override_config)
            .expect_err("allowlist override must not broaden to all");

        assert!(format!("{err:#}").contains("cannot broaden sandbox network mode"));
    }
}
