use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use ctx_core::models::Session;

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;

use super::turn_start::turn_start_deadline;

mod mcp;

pub(super) struct PreparedProviderLaunchEnvironment {
    pub(super) mcp_token: Option<String>,
    pub(super) codex_home: Option<PathBuf>,
    pub(super) start_deadline_duration: Duration,
}

pub(super) async fn prepare_provider_launch_environment(
    provider_launch: &ProviderTurnLaunchHost,
    session: &Session,
    runtime_provider_id: &str,
    workdir: &Path,
    provider_env: &mut HashMap<String, String>,
) -> Result<PreparedProviderLaunchEnvironment> {
    ctx_provider_runtime::provider_launch::environment::apply_provider_launch_overrides(
        runtime_provider_id,
        workdir,
        provider_env,
    )
    .await?;
    let mcp_disabled = provider_env
        .get("CTX_MCP_DISABLED")
        .and_then(|value| ctx_core::boolish::parse_boolish(value))
        .unwrap_or(false);
    let mcp_token =
        mcp::issue_mcp_token_if_enabled(provider_launch, session, provider_env, mcp_disabled).await;
    let codex_home = provider_env
        .get("CODEX_HOME")
        .map(|value| PathBuf::from(value.as_str()));
    let start_deadline_duration = turn_start_deadline(provider_env);

    Ok(PreparedProviderLaunchEnvironment {
        mcp_token,
        codex_home,
        start_deadline_duration,
    })
}
