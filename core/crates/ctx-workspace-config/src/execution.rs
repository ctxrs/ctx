use super::*;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ExecutionConfigSnapshot {
    pub source: String,
    pub environment: String,
    pub network_mode: Option<String>,
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionConfigUpdateInput {
    pub environment: ExecutionEnvironment,
    pub network_mode: Option<ContainerNetworkMode>,
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionConfigInputError {
    InvalidEnvironment,
    InvalidNetworkMode,
    SandboxRuntimeUnavailable,
}

impl std::fmt::Display for ExecutionConfigInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEnvironment => {
                write!(f, "invalid environment (expected host|sandbox)")
            }
            Self::InvalidNetworkMode => {
                write!(f, "invalid network_mode (expected llm_only|allowlist|all)")
            }
            Self::SandboxRuntimeUnavailable => write!(
                f,
                "AVF sandbox is unavailable on this macOS host. Install or launch through the desktop app so the AVF helper/runtime is present, then try again."
            ),
        }
    }
}

impl std::error::Error for ExecutionConfigInputError {}

#[derive(Debug, Clone, Default)]
pub struct ExecutionSettingsOverride {
    pub mode: Option<ExecutionMode>,
    pub container: ContainerExecutionSettingsOverride,
}

#[derive(Debug, Clone, Default)]
pub struct ContainerExecutionSettingsOverride {
    pub network_mode: Option<ContainerNetworkMode>,
    pub allowlist: Option<Vec<String>>,
    pub image: Option<String>,
}

pub async fn load_execution_settings_override(
    store: &Store,
) -> Result<Option<ExecutionSettingsOverride>> {
    let cfg = load_workspace_settings_doc(store).await?;
    let Some(exec) = cfg.execution else {
        return Ok(None);
    };

    let mut ov = ExecutionSettingsOverride::default();
    let environment = exec.environment;
    if let Some(environment) = environment {
        match environment {
            ExecutionEnvironment::Host => {
                ov.mode = Some(ExecutionMode::Host);
            }
            ExecutionEnvironment::Sandbox => {
                ov.mode = Some(ExecutionMode::Sandbox);
            }
        }
    }

    if let Some(c) = exec.container {
        ov.container.network_mode = c.network_mode;
        ov.container.allowlist = c.allowlist.map(|v| {
            v.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });
        ov.container.image = c
            .image
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
    }

    Ok(Some(ov))
}

pub fn apply_execution_settings_override(
    settings: &mut ExecutionSettings,
    ov: &ExecutionSettingsOverride,
) {
    if let Some(mode) = ov.mode.clone() {
        settings.mode = mode;
    }
    if let Some(network_mode) = ov.container.network_mode.clone() {
        settings.container.network_mode = network_mode;
    }
    if let Some(allowlist) = ov.container.allowlist.clone() {
        settings.container.allowlist = allowlist;
    }
    if let Some(image) = ov.container.image.clone() {
        settings.container.image = Some(image);
    }
}

pub fn project_execution_config(
    source: impl Into<String>,
    effective: &ExecutionSettings,
) -> ExecutionConfigSnapshot {
    let environment = match effective.mode {
        ExecutionMode::Host => "host",
        ExecutionMode::Sandbox => "sandbox",
    }
    .to_string();
    let network_mode = match effective.container.network_mode {
        ContainerNetworkMode::LlmOnly => "llm_only",
        ContainerNetworkMode::Allowlist => "allowlist",
        ContainerNetworkMode::All => "all",
    }
    .to_string();

    ExecutionConfigSnapshot {
        source: source.into(),
        environment,
        network_mode: Some(network_mode),
        allowlist: Some(effective.container.allowlist.clone()),
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionConfigUpdate {
    pub environment: ExecutionEnvironment,
    pub network_mode: Option<ContainerNetworkMode>,
    pub allowlist: Option<Vec<String>>,
    pub image: Option<String>,
}

pub fn parse_execution_config_update_input(
    environment: &str,
    network_mode: Option<&str>,
    allowlist: Option<Vec<String>>,
    sandbox_runtime_available: bool,
) -> Result<ExecutionConfigUpdateInput, ExecutionConfigInputError> {
    let environment = match environment {
        "host" => ExecutionEnvironment::Host,
        "sandbox" => {
            if !sandbox_runtime_available {
                return Err(ExecutionConfigInputError::SandboxRuntimeUnavailable);
            }
            ExecutionEnvironment::Sandbox
        }
        _ => return Err(ExecutionConfigInputError::InvalidEnvironment),
    };
    let network_mode = parse_execution_network_mode_input(network_mode)?;
    let allowlist = allowlist.map(normalize_execution_allowlist);
    Ok(ExecutionConfigUpdateInput {
        environment,
        network_mode,
        allowlist,
    })
}

pub fn parse_execution_network_mode_input(
    network_mode: Option<&str>,
) -> Result<Option<ContainerNetworkMode>, ExecutionConfigInputError> {
    match network_mode.map(str::trim) {
        None | Some("") => Ok(None),
        Some("llm_only") => Ok(Some(ContainerNetworkMode::LlmOnly)),
        Some("allowlist") => Ok(Some(ContainerNetworkMode::Allowlist)),
        Some("all") => Ok(Some(ContainerNetworkMode::All)),
        _ => Err(ExecutionConfigInputError::InvalidNetworkMode),
    }
}

pub fn normalize_execution_allowlist(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

pub fn execution_settings_override_from_update(
    update: &ExecutionConfigUpdateInput,
) -> ExecutionSettingsOverride {
    ExecutionSettingsOverride {
        mode: Some(match update.environment {
            ExecutionEnvironment::Host => ExecutionMode::Host,
            ExecutionEnvironment::Sandbox => ExecutionMode::Sandbox,
        }),
        container: ContainerExecutionSettingsOverride {
            network_mode: update.network_mode.clone(),
            allowlist: update.allowlist.clone(),
            image: None,
        },
    }
}

pub fn execution_config_update_from_input(
    input: ExecutionConfigUpdateInput,
) -> ExecutionConfigUpdate {
    ExecutionConfigUpdate {
        environment: input.environment,
        network_mode: input.network_mode,
        allowlist: input.allowlist,
        image: None,
    }
}

pub async fn update_execution_config(store: &Store, update: ExecutionConfigUpdate) -> Result<()> {
    let container = if matches!(update.environment, ExecutionEnvironment::Sandbox) {
        Some(WorkspaceContainerExecutionConfig {
            network_mode: update.network_mode,
            allowlist: update.allowlist.map(|v| {
                v.into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            }),
            image: update
                .image
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        })
    } else {
        None
    };
    mutate_workspace_settings_doc(store, "execution", move |cfg| {
        cfg.execution = Some(WorkspaceExecutionConfig {
            environment: Some(update.environment),
            container,
        });
        Ok(())
    })
    .await
}
