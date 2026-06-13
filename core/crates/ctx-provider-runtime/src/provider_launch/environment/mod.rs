use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

mod mcp;
mod openhands;
#[cfg(test)]
mod tests;

pub async fn apply_provider_launch_overrides(
    provider_id: &str,
    workdir: &Path,
    provider_env: &mut HashMap<String, String>,
) -> Result<()> {
    mcp::apply_provider_mcp_command_overrides(provider_id, provider_env);

    if provider_id == "openhands" {
        openhands::apply_openhands_launch_overrides(workdir, provider_env).await?;
        return Ok(());
    }

    mcp::strip_unused_daemon_auth_from_provider_env(provider_env);
    Ok(())
}
