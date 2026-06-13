use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use ctx_harness_sources::HarnessSourceKind;
use ctx_managed_installs::AgentServerConfigFile;
use ctx_provider_install::install_state::InstallTarget;

use super::probe::WorkspaceRuntimeProbeContext;
use super::resolver::{is_acp_provider_id, runtime_probe_command_as_agent_command_for_target};

#[derive(Debug, Clone)]
pub struct PreparedProviderRuntimeProbe {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
    pub selected_endpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareProviderRuntimeProbeError {
    message: String,
}

impl PrepareProviderRuntimeProbeError {
    pub fn into_message(self) -> String {
        self.message
    }
}

impl fmt::Display for PrepareProviderRuntimeProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrepareProviderRuntimeProbeError {}

pub fn prepare_provider_runtime_probe_launch(
    data_root: &Path,
    cfg: &AgentServerConfigFile,
    provider_id: &str,
    install_target: InstallTarget,
    probe_context: WorkspaceRuntimeProbeContext,
    selected_endpoint_id: Option<String>,
) -> Result<PreparedProviderRuntimeProbe, PrepareProviderRuntimeProbeError> {
    let runtime_command = runtime_probe_command_as_agent_command_for_target(
        data_root,
        cfg,
        provider_id,
        Some(install_target),
    )
    .map_err(|error| {
        verify_error(format!(
            "runtime_command_invalid: provider={provider_id} error={error}"
        ))
    })?
    .ok_or_else(|| {
        verify_error(format!(
            "runtime_command_missing: provider={provider_id} (configure an absolute runtime command)"
        ))
    })?;
    let command = runtime_command.command;
    let args = runtime_command.args;

    let source = probe_context.source;
    let mut env = probe_context.env;
    ctx_managed_installs::prepend_runtime_bin_dirs_to_provider_path_for_target(
        &mut env,
        cfg,
        provider_id,
        data_root,
        Some(install_target),
    );
    if is_acp_provider_id(provider_id) {
        ctx_managed_installs::prepend_runtime_bin_dirs_to_provider_path_for_target(
            &mut env,
            cfg,
            "acp-crp-bridge",
            data_root,
            Some(install_target),
        );
    }
    ctx_managed_installs::ensure_codex_cli_command_env_for_target(
        &mut env,
        cfg,
        provider_id,
        Some(install_target),
    )
    .map_err(|error| {
        verify_error(format!(
            "codex_cli_command_invalid: provider={provider_id} error={error}"
        ))
    })?;

    let selected_endpoint_id = if source.source_kind == HarnessSourceKind::Endpoint {
        source
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.id.clone())
            .or(selected_endpoint_id)
    } else {
        None
    };

    Ok(PreparedProviderRuntimeProbe {
        command,
        args,
        env,
        cwd: probe_context.cwd,
        selected_endpoint_id,
    })
}

fn verify_error(message: String) -> PrepareProviderRuntimeProbeError {
    PrepareProviderRuntimeProbeError { message }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use ctx_harness_sources::{
        EndpointModelCatalogStatus, HarnessApiShape, HarnessEndpointRecord,
        HarnessEndpointVerificationStatus, ResolvedHarnessSource,
    };

    fn empty_config() -> AgentServerConfigFile {
        AgentServerConfigFile::default()
    }

    fn subscription_context(cwd: PathBuf) -> WorkspaceRuntimeProbeContext {
        WorkspaceRuntimeProbeContext {
            source: ResolvedHarnessSource {
                source_kind: HarnessSourceKind::Subscription,
                endpoint: None,
                env: HashMap::new(),
            },
            env: HashMap::from([("CUSTOM_ENV".to_string(), "kept".to_string())]),
            cwd,
        }
    }

    fn endpoint_record(endpoint_id: &str) -> HarnessEndpointRecord {
        HarnessEndpointRecord {
            id: endpoint_id.to_string(),
            provider_id: "codex".to_string(),
            name: "OpenAI".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: HarnessApiShape::OpenaiResponses,
            auth_type: "api_key".to_string(),
            model_override: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_verification_status: HarnessEndpointVerificationStatus::Valid,
            last_verification_at: None,
            last_error: None,
            has_api_key: true,
            model_catalog_status: EndpointModelCatalogStatus::Unknown,
            model_catalog_fetched_at: None,
            model_catalog_error: None,
            model_catalog_models: Vec::new(),
            manual_model_ids: Vec::new(),
            model_catalog_source: None,
        }
    }

    #[test]
    fn runtime_probe_plan_reports_missing_runtime_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = prepare_provider_runtime_probe_launch(
            temp.path(),
            &empty_config(),
            "claude-cli",
            InstallTarget::Host,
            subscription_context(temp.path().to_path_buf()),
            None,
        )
        .expect_err("missing runtime command");

        assert_eq!(
            err.to_string(),
            "runtime_command_missing: provider=claude-cli (configure an absolute runtime command)"
        );
    }

    #[test]
    fn runtime_probe_plan_preserves_probe_context_and_endpoint_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_cmd = temp.path().join("codex");
        let codex_cli_cmd = temp.path().join("codex-cli");
        std::fs::write(&codex_cmd, b"codex").expect("codex command");
        std::fs::write(&codex_cli_cmd, b"codex cli command").expect("codex cli command");
        let expected_codex_cmd = std::fs::canonicalize(&codex_cmd).expect("canonical codex path");
        let expected_codex_cli_cmd =
            std::fs::canonicalize(&codex_cli_cmd).expect("canonical codex cli path");
        let expected_temp_path = std::fs::canonicalize(temp.path()).expect("canonical temp path");
        let cfg = AgentServerConfigFile {
            providers: HashMap::from([
                (
                    "codex".to_string(),
                    ctx_managed_installs::AgentServerCommand {
                        command: codex_cmd.to_string_lossy().to_string(),
                        args: vec!["--serve".to_string()],
                        dependencies: Vec::new(),
                        managed: None,
                    },
                ),
                (
                    "codex-cli".to_string(),
                    ctx_managed_installs::AgentServerCommand {
                        command: codex_cli_cmd.to_string_lossy().to_string(),
                        args: Vec::new(),
                        dependencies: Vec::new(),
                        managed: None,
                    },
                ),
            ]),
            ..AgentServerConfigFile::default()
        };
        let cwd = temp.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("workspace");
        let context = WorkspaceRuntimeProbeContext {
            source: ResolvedHarnessSource {
                source_kind: HarnessSourceKind::Endpoint,
                endpoint: Some(endpoint_record("resolved-endpoint")),
                env: HashMap::new(),
            },
            env: HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            cwd: cwd.clone(),
        };

        let prepared = prepare_provider_runtime_probe_launch(
            temp.path(),
            &cfg,
            "codex",
            InstallTarget::Host,
            context,
            Some("requested-endpoint".to_string()),
        )
        .expect("prepared runtime probe");

        assert_eq!(prepared.command, expected_codex_cmd.to_string_lossy());
        assert_eq!(prepared.args, vec!["--serve"]);
        assert_eq!(prepared.cwd, cwd);
        assert_eq!(
            prepared.selected_endpoint_id.as_deref(),
            Some("resolved-endpoint")
        );
        assert_eq!(
            prepared.env.get("CTX_CODEX_BIN_PATH").map(String::as_str),
            Some(expected_codex_cli_cmd.to_str().expect("codex cli path"))
        );
        let path = prepared.env.get("PATH").expect("PATH");
        assert!(
            path.contains(expected_temp_path.to_str().expect("temp path")),
            "runtime command bin dir should be prepended to PATH: {path}"
        );
    }
}
