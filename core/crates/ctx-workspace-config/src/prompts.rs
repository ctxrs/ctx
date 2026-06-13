use super::*;

#[derive(Debug, Clone)]
pub struct AgentSystemPromptAppendConfig {
    pub configured_append: Option<String>,
    pub default_append: String,
}

impl AgentSystemPromptAppendConfig {
    pub fn new_default() -> Self {
        Self {
            configured_append: None,
            default_append: DEFAULT_SYSTEM_PROMPT_APPEND.to_string(),
        }
    }

    pub fn effective_append(&self) -> Option<String> {
        match self.configured_append.as_deref() {
            Some(value) => trimmed_nonempty(value),
            None => trimmed_nonempty(&self.default_append),
        }
    }

    pub fn source(&self) -> AgentSystemPromptAppendSource {
        match self.configured_append.as_deref() {
            Some(value) => {
                if value.trim().is_empty() {
                    AgentSystemPromptAppendSource::Disabled
                } else {
                    AgentSystemPromptAppendSource::Config
                }
            }
            None => {
                if self.default_append.trim().is_empty() {
                    AgentSystemPromptAppendSource::Disabled
                } else {
                    AgentSystemPromptAppendSource::Default
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubagentSystemPromptAppendConfig {
    pub configured_append: Option<String>,
    pub default_append: String,
}

impl SubagentSystemPromptAppendConfig {
    pub fn new_default() -> Self {
        Self {
            configured_append: None,
            default_append: DEFAULT_SUBAGENT_SYSTEM_PROMPT_APPEND.to_string(),
        }
    }

    pub fn effective_append(&self) -> Option<String> {
        match self.configured_append.as_deref() {
            Some(value) => trimmed_nonempty(value),
            None => trimmed_nonempty(&self.default_append),
        }
    }

    pub fn source(&self) -> AgentSystemPromptAppendSource {
        match self.configured_append.as_deref() {
            Some(value) => {
                if value.trim().is_empty() {
                    AgentSystemPromptAppendSource::Disabled
                } else {
                    AgentSystemPromptAppendSource::Config
                }
            }
            None => {
                if self.default_append.trim().is_empty() {
                    AgentSystemPromptAppendSource::Disabled
                } else {
                    AgentSystemPromptAppendSource::Default
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSystemPromptAppendSource {
    Default,
    Config,
    Disabled,
}

pub async fn load_agent_system_prompt_append(
    store: &Store,
) -> Result<AgentSystemPromptAppendConfig> {
    let cfg = load_workspace_settings_doc(store).await?;
    let configured_append = cfg.agents.and_then(|agents| agents.system_prompt_append);

    Ok(AgentSystemPromptAppendConfig {
        configured_append,
        default_append: DEFAULT_SYSTEM_PROMPT_APPEND.to_string(),
    })
}

pub async fn load_subagent_system_prompt_append(
    store: &Store,
) -> Result<SubagentSystemPromptAppendConfig> {
    let cfg = load_workspace_settings_doc(store).await?;
    let configured_append = cfg
        .subagents
        .and_then(|subagents| subagents.system_prompt_append);

    Ok(SubagentSystemPromptAppendConfig {
        configured_append,
        default_append: DEFAULT_SUBAGENT_SYSTEM_PROMPT_APPEND.to_string(),
    })
}

pub async fn update_agent_system_prompt_append(
    store: &Store,
    system_prompt_append: Option<String>,
) -> Result<()> {
    mutate_workspace_settings_doc(store, "agents", move |cfg| {
        match system_prompt_append {
            Some(value) => {
                let trimmed = value.trim().to_string();
                cfg.agents = Some(WorkspaceAgentsConfig {
                    system_prompt_append: Some(trimmed),
                });
            }
            None => cfg.agents = None,
        }
        Ok(())
    })
    .await
}

pub async fn update_and_load_agent_system_prompt_append(
    store: &Store,
    system_prompt_append: Option<String>,
) -> Result<AgentSystemPromptAppendConfig> {
    update_agent_system_prompt_append(store, system_prompt_append).await?;
    load_agent_system_prompt_append(store).await
}

pub async fn update_subagent_system_prompt_append(
    store: &Store,
    system_prompt_append: Option<String>,
) -> Result<()> {
    mutate_workspace_settings_doc(store, "subagents", move |cfg| {
        match system_prompt_append {
            Some(value) => {
                let trimmed = value.trim().to_string();
                cfg.subagents = Some(WorkspaceSubagentsConfig {
                    system_prompt_append: Some(trimmed),
                });
            }
            None => cfg.subagents = None,
        }
        Ok(())
    })
    .await
}

pub async fn update_and_load_subagent_system_prompt_append(
    store: &Store,
    system_prompt_append: Option<String>,
) -> Result<SubagentSystemPromptAppendConfig> {
    update_subagent_system_prompt_append(store, system_prompt_append).await?;
    load_subagent_system_prompt_append(store).await
}
