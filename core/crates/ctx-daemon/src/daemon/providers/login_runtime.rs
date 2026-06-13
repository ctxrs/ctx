use std::path::Path;

use ctx_provider_runtime::provider_login_runtime::{
    resolve_claude_login_runtime_from_config, resolve_cursor_login_runtime_from_config,
    ProviderLoginRuntimeCommand,
};

pub async fn resolve_cursor_login_runtime(
    data_root: &Path,
) -> anyhow::Result<ProviderLoginRuntimeCommand> {
    resolve_cursor_login_runtime_from_config(data_root).await
}

pub async fn resolve_claude_login_runtime(
    data_root: &Path,
) -> anyhow::Result<ProviderLoginRuntimeCommand> {
    resolve_claude_login_runtime_from_config(data_root).await
}
