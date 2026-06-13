use std::path::Path;

use anyhow::Result;
use ctx_core::redaction;
use ctx_harness_sources as harness_sources;
use ctx_managed_installs::AgentServerConfigFile;

pub async fn load_provider_source_config_with_error(
    data_root: &Path,
    provider_id: &str,
) -> (
    Option<harness_sources::HarnessProviderSourceConfig>,
    Option<String>,
) {
    if !harness_sources::supports_harness_endpoint(provider_id) {
        return (None, None);
    }

    match harness_sources::get_provider_source_config(data_root, provider_id).await {
        Ok(config) => (Some(config), None),
        Err(err) => (None, Some(redaction::redact_sensitive(&err.to_string()))),
    }
}

pub async fn load_managed_agent_server_config_or_err(
    data_root: &Path,
) -> Result<AgentServerConfigFile> {
    ctx_managed_installs::load_agent_server_config(data_root)
        .await
        .map_err(|err| anyhow::anyhow!(redaction::redact_sensitive(&err.to_string())))
}

pub async fn load_managed_agent_server_config_with_error(
    data_root: &Path,
) -> (AgentServerConfigFile, Option<String>) {
    match load_managed_agent_server_config_or_err(data_root).await {
        Ok(config) => (config, None),
        Err(err) => (AgentServerConfigFile::default(), Some(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_provider_source_config_short_circuits_without_error() {
        let temp = tempfile::tempdir().expect("tempdir");

        let (config, error) = load_provider_source_config_with_error(temp.path(), "cursor").await;

        assert!(config.is_none());
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn managed_config_error_returns_default_config_and_message() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = ctx_managed_installs::agent_server_config_path(temp.path());
        tokio::fs::create_dir_all(path.parent().expect("config parent"))
            .await
            .expect("create config parent");
        tokio::fs::write(&path, b"{ invalid json")
            .await
            .expect("write invalid config");

        let (config, error) = load_managed_agent_server_config_with_error(temp.path()).await;

        assert!(config.providers.is_empty());
        let error = error.expect("config error");
        assert!(
            error.contains("parsing agent server config"),
            "unexpected error: {error}"
        );
    }
}
