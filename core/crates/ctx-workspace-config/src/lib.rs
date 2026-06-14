use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
pub use ctx_core::models::ExecutionEnvironment;
use ctx_sandbox_contract::{ContainerNetworkMode, ExecutionMode, ExecutionSettings};
use ctx_store::Store;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Mutex as AsyncMutex;

mod execution;
mod merge_queue;
mod new_session;
mod prompts;
mod vcs;
mod worktree_bootstrap;

pub use execution::{
    apply_execution_settings_override, execution_config_update_from_input,
    execution_settings_override_from_update, load_execution_settings_override,
    normalize_execution_allowlist, parse_execution_config_update_input,
    parse_execution_network_mode_input, project_execution_config, update_execution_config,
    ContainerExecutionSettingsOverride, ExecutionConfigInputError, ExecutionConfigSnapshot,
    ExecutionConfigUpdate, ExecutionConfigUpdateInput, ExecutionSettingsOverride,
};
pub use merge_queue::{
    load_merge_queue_config, load_merge_queue_target_branch_override, update_merge_queue_config,
    update_merge_queue_config_with_transition, MergeQueueCanonicalSync, MergeQueueConfig,
    MergeQueueConfigTransition, MergeQueueConfigUpdate,
};
pub use new_session::{
    load_preferred_new_session_model_id, load_preferred_new_session_models,
    update_preferred_new_session_model_id,
};
pub use prompts::{
    load_agent_system_prompt_append, load_subagent_system_prompt_append,
    update_agent_system_prompt_append, update_and_load_agent_system_prompt_append,
    update_and_load_subagent_system_prompt_append, update_subagent_system_prompt_append,
    AgentSystemPromptAppendConfig, AgentSystemPromptAppendSource, SubagentSystemPromptAppendConfig,
};
pub use vcs::{load_primary_branch, update_and_load_primary_branch, update_primary_branch};
pub use worktree_bootstrap::{
    load_worktree_bootstrap_config, update_worktree_bootstrap_config, WorktreeBootstrapConfig,
    WorktreeBootstrapConfigUpdate,
};

const WORKSPACE_SETTINGS_SCHEMA_VERSION: i64 = 1;
const WORKSPACE_RUNTIME_SETTINGS_PARSE_CONTEXT: &str =
    "parsing workspace runtime settings document";

pub fn is_workspace_runtime_settings_parse_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string() == WORKSPACE_RUNTIME_SETTINGS_PARSE_CONTEXT)
}

pub const DEFAULT_SYSTEM_PROMPT_APPEND: &str = "You are working inside ctx, an agent development environment. Use ctx MCP tools to attach photos/videos as artifacts, start persistent web sessions (Playwright REPL/scripts), and run sub-agents for research or well-scoped implementations. Check `.ctx/attachments/refs/` and `.ctx/attachments/docs/` for extra reference repos and docs.";
pub const DEFAULT_SUBAGENT_SYSTEM_PROMPT_APPEND: &str = "You are a subagent. The user messaging you is the primary agent who will provide your instructions.";

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceRuntimeSettingsDoc {
    #[serde(default)]
    agents: Option<WorkspaceAgentsConfig>,
    #[serde(default)]
    subagents: Option<WorkspaceSubagentsConfig>,
    #[serde(default)]
    new_session: Option<WorkspaceNewSessionConfig>,
    #[serde(default)]
    vcs: Option<WorkspaceVcsConfig>,
    #[serde(default)]
    merge_queue: Option<WorkspaceMergeQueueConfig>,
    #[serde(default)]
    execution: Option<WorkspaceExecutionConfig>,
    #[serde(default)]
    worktree_bootstrap: Option<WorkspaceWorktreeBootstrapConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct WorkspaceAgentsConfig {
    #[serde(default)]
    system_prompt_append: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct WorkspaceSubagentsConfig {
    #[serde(default)]
    system_prompt_append: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceNewSessionConfig {
    #[serde(default, deserialize_with = "deserialize_optional_string_map")]
    preferred_model_by_provider: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceVcsConfig {
    #[serde(default)]
    primary_branch: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceMergeQueueConfig {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    target_branch: Option<String>,
    #[serde(default)]
    verify_commands: Option<Vec<String>>,
    #[serde(default)]
    push_on_success: Option<bool>,
    #[serde(default)]
    push_remote: Option<String>,
    #[serde(default)]
    push_branch: Option<String>,
    #[serde(default)]
    canonical_sync: Option<MergeQueueCanonicalSync>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceExecutionConfig {
    #[serde(default)]
    environment: Option<ExecutionEnvironment>,
    #[serde(default)]
    container: Option<WorkspaceContainerExecutionConfig>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceContainerExecutionConfig {
    #[serde(default)]
    network_mode: Option<ContainerNetworkMode>,
    #[serde(default)]
    allowlist: Option<Vec<String>>,
    #[serde(default)]
    image: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct WorkspaceWorktreeBootstrapConfig {
    #[serde(default)]
    setup_command: Option<String>,
    #[serde(default)]
    timeout_sec: Option<u64>,
    #[serde(default)]
    wait_for_completion: Option<bool>,
    #[serde(default)]
    cleanup_command: Option<String>,
    #[serde(default)]
    cleanup_timeout_sec: Option<u64>,
}

async fn load_workspace_settings_doc(store: &Store) -> Result<WorkspaceRuntimeSettingsDoc> {
    let Some(doc) = store.get_runtime_settings_document().await? else {
        return Ok(WorkspaceRuntimeSettingsDoc::default());
    };

    let parsed = serde_json::from_str::<WorkspaceRuntimeSettingsDoc>(&doc.settings_json)
        .context(WORKSPACE_RUNTIME_SETTINGS_PARSE_CONTEXT)?;
    Ok(parsed)
}

fn workspace_settings_write_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

async fn mutate_workspace_settings_doc(
    store: &Store,
    _operation: &'static str,
    mutate: impl FnOnce(&mut WorkspaceRuntimeSettingsDoc) -> Result<()>,
) -> Result<()> {
    let _guard = workspace_settings_write_lock().lock().await;
    let mut cfg = load_workspace_settings_doc(store).await?;
    #[cfg(test)]
    pause_after_workspace_settings_load_for_tests(_operation).await;
    mutate(&mut cfg)?;
    save_workspace_settings_doc(store, &cfg).await
}

async fn save_workspace_settings_doc(
    store: &Store,
    cfg: &WorkspaceRuntimeSettingsDoc,
) -> Result<()> {
    let settings_json = serde_json::to_string_pretty(cfg)?;
    store
        .upsert_runtime_settings_document(WORKSPACE_SETTINGS_SCHEMA_VERSION, &settings_json)
        .await?;
    Ok(())
}

fn normalize_provider_preference_key(value: &str) -> Option<String> {
    trimmed_nonempty(value)
}

fn deserialize_optional_string_map<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<HashMap<String, serde_json::Value>>::deserialize(deserializer)?;
    Ok(raw.map(|entries| {
        entries
            .into_iter()
            .filter_map(|(key, value)| value.as_str().map(|model_id| (key, model_id.to_string())))
            .collect()
    }))
}

fn trimmed_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
type WorkspaceSettingsLoadedSignal = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[cfg(test)]
type WorkspaceSettingsTestPauseHook =
    AsyncMutex<HashMap<&'static str, WorkspaceSettingsLoadedSignal>>;

#[cfg(test)]
fn workspace_settings_test_pause_hook() -> &'static WorkspaceSettingsTestPauseHook {
    static HOOK: OnceLock<WorkspaceSettingsTestPauseHook> = OnceLock::new();
    HOOK.get_or_init(|| AsyncMutex::new(HashMap::new()))
}

#[cfg(test)]
async fn pause_after_workspace_settings_load_for_tests(operation: &'static str) {
    let hook = workspace_settings_test_pause_hook()
        .lock()
        .await
        .remove(operation);
    if let Some((loaded_tx, resume_rx)) = hook {
        let _ = loaded_tx.send(());
        let _ = resume_rx.await;
    }
}

#[cfg(test)]
mod tests;
