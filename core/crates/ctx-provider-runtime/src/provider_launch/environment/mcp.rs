use std::collections::HashMap;

pub(super) fn apply_provider_mcp_command_overrides(
    provider_id: &str,
    provider_env: &mut HashMap<String, String>,
) {
    if matches!(provider_id, "fake" | "broken" | "opencode" | "kimi") {
        // These providers currently behave truthfully without the daemon MCP runtime.
        provider_env.insert("CTX_MCP_DISABLED".to_string(), "1".to_string());
    }
}

pub(super) fn strip_unused_daemon_auth_from_provider_env(
    provider_env: &mut HashMap<String, String>,
) {
    let mcp_disabled = provider_env
        .get("CTX_MCP_DISABLED")
        .and_then(|value| ctx_core::boolish::parse_boolish(value))
        .unwrap_or(false);
    if !mcp_disabled {
        return;
    }
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        provider_env.remove(*key);
    }
}
