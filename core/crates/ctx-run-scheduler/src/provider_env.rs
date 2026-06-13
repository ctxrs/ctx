use std::collections::HashMap;
use std::path::Path;

use ctx_core::models::Session;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_settings_model::ProviderControlMode;

pub struct BaseProviderEnvRequest<'a> {
    pub daemon_url: &'a str,
    pub data_root: &'a Path,
    pub session: &'a Session,
    pub full_model_id: &'a str,
    pub provider_control_mode: &'a ProviderControlMode,
}

pub fn build_base_provider_env(request: BaseProviderEnvRequest<'_>) -> HashMap<String, String> {
    let BaseProviderEnvRequest {
        daemon_url,
        data_root,
        session,
        full_model_id,
        provider_control_mode,
    } = request;
    let mut provider_env = HashMap::new();
    provider_env.insert("CTX_DAEMON_URL".to_string(), daemon_url.to_string());
    provider_env.insert(
        "CTX_DATA_ROOT".to_string(),
        data_root.to_string_lossy().to_string(),
    );
    provider_env.insert("CTX_PROVIDER_ID".to_string(), session.provider_id.clone());
    provider_env.insert(
        "CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL".to_string(),
        "1".to_string(),
    );
    if let Some(provider_ref) = session.provider_session_ref.clone() {
        provider_env.insert("CTX_PROVIDER_SESSION_REF".to_string(), provider_ref);
    }
    provider_env.insert("CTX_SESSION_ID".to_string(), session.id.0.to_string());
    provider_env.insert(
        "CTX_WORKTREE_ID".to_string(),
        session.worktree_id.0.to_string(),
    );
    provider_env.insert("CTX_MODEL_ID".to_string(), full_model_id.to_string());
    if let Some(mode_id) = provider_mode_id_for(&session.provider_id, provider_control_mode) {
        provider_env.insert("CTX_PROVIDER_MODE".to_string(), mode_id.to_string());
    }
    if let Ok(v) = std::env::var("CTX_MCP_COMMAND") {
        provider_env.insert("CTX_MCP_COMMAND".to_string(), v);
    }
    if let Ok(v) = std::env::var("CTX_MCP_DISABLED") {
        provider_env.insert("CTX_MCP_DISABLED".to_string(), v);
    }
    provider_env
}

pub fn provider_mode_id_for(
    provider_id: &str,
    control_mode: &ProviderControlMode,
) -> Option<&'static str> {
    match control_mode {
        ProviderControlMode::Full => match provider_id {
            CODEX_PROVIDER_ID => Some("full-access"),
            "claude-crp" => Some("bypassPermissions"),
            "droid" => Some("auto_high"),
            _ => None,
        },
        ProviderControlMode::HarnessNative | ProviderControlMode::CtxEnforced => None,
    }
}
