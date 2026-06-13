use anyhow::{Context, Result};

use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_workspace_config as workspace_config;

pub(in crate::daemon::scheduler::runtime) fn normalize_session_model_id(
    model_id: &str,
) -> Option<String> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(in crate::daemon::scheduler::runtime) fn provider_supports_system_prompt_append(
    provider_id: &str,
) -> bool {
    matches!(provider_id, "claude-crp" | CODEX_PROVIDER_ID)
}

pub(in crate::daemon::scheduler::runtime) fn runtime_provider_id_for_session_provider<'a>(
    session_provider_id: &'a str,
    _resolved_source: &ctx_harness_sources::ResolvedHarnessSource,
) -> &'a str {
    session_provider_id
}

pub(in crate::daemon::scheduler::runtime) async fn load_system_prompt_append_for_relationship(
    store: &ctx_store::Store,
    relationship: Option<&str>,
) -> Result<Option<String>> {
    let prompt_config = workspace_config::load_agent_system_prompt_append(store)
        .await
        .context("loading agent system prompt append config")?;
    let mut system_prompt_append = prompt_config.effective_append();

    if relationship == Some("sub_agent") {
        let subagent_config = workspace_config::load_subagent_system_prompt_append(store)
            .await
            .context("loading subagent system prompt append config")?;
        if let Some(subagent_append) = subagent_config.effective_append() {
            system_prompt_append = Some(match system_prompt_append {
                Some(mut append) => {
                    append.push_str("\n\n");
                    append.push_str(&subagent_append);
                    append
                }
                None => subagent_append,
            });
        }
    }

    Ok(system_prompt_append)
}
