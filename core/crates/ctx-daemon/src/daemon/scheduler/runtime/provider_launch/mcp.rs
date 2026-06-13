use super::*;

pub(super) async fn issue_mcp_token_if_enabled(
    provider_launch: &ProviderTurnLaunchHost,
    session: &Session,
    provider_env: &mut HashMap<String, String>,
    mcp_disabled: bool,
) -> Option<String> {
    if mcp_disabled {
        return None;
    }
    provider_launch
        .issue_turn_mcp_token(session, provider_env)
        .await
}
