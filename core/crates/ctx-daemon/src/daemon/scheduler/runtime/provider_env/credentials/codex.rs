use super::*;
use anyhow::anyhow;

pub(super) struct CodexRuntimeCredentialRequest<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) provider_env: &'a mut HashMap<String, String>,
    pub(super) runtime_provider_id: &'a str,
    pub(super) runtime_plan: &'a ctx_harness_runtime::HarnessExecutionPlan,
    pub(super) is_linux_sandbox: bool,
    pub(super) credential_mode: ProviderRuntimeCredentialMode,
}

pub(super) async fn prepare_codex_runtime_credentials(
    request: CodexRuntimeCredentialRequest<'_>,
) -> Result<()> {
    let CodexRuntimeCredentialRequest {
        provider_launch,
        provider_env,
        runtime_provider_id,
        runtime_plan,
        is_linux_sandbox,
        credential_mode,
    } = request;

    if is_linux_sandbox && credential_mode.is_user_managed_endpoint() {
        if let Some(root) = runtime_plan.env_overrides.get("CTX_DATA_ROOT") {
            provider_accounts::ensure_codex_endpoint_runtime_home_from_env(
                Path::new(root),
                provider_env,
            )
            .await?;
        }
    }

    if !provider_env.contains_key("CODEX_HOME") && credential_mode.is_subscription() {
        if is_linux_sandbox {
            if let Some(root) = runtime_plan.env_overrides.get("CTX_DATA_ROOT") {
                let env = provider_accounts::codex_env_for_active_account_with_runtime_root(
                    provider_launch.data_root(),
                    Path::new(root),
                )
                .await?;
                for (key, value) in env {
                    provider_env.insert(key, value);
                }
            }
        } else {
            let env = provider_accounts::codex_env_for_active_account(provider_launch.data_root())
                .await?;
            for (key, value) in env {
                provider_env.insert(key, value);
            }
        }
    }

    if credential_mode.is_ctx_managed_relay() {
        return Ok(());
    }

    let codex_home = provider_env
        .get("CODEX_HOME")
        .cloned()
        .ok_or_else(|| anyhow!("missing CODEX_HOME for {runtime_provider_id}"))?;
    provider_accounts::ensure_codex_auth_ready(Path::new(&codex_home))
        .await
        .map_err(|err| {
            if credential_mode.is_user_managed_endpoint() {
                anyhow!(
                    "Codex endpoint credentials are not configured correctly. Open Settings -> Agent Harnesses and verify the selected endpoint. Details: {err}"
                )
            } else {
                anyhow!(
                    "Codex authentication is not configured. Open Settings -> Codex and add a subscription login or API key. Details: {err}"
                )
            }
        })?;
    if is_linux_sandbox && credential_mode.is_user_managed_endpoint() {
        let openai_api_key_present = provider_env
            .get("OPENAI_API_KEY")
            .is_some_and(|value| !value.trim().is_empty());
        if !openai_api_key_present {
            anyhow::bail!(
                "codex endpoint container runtime missing OPENAI_API_KEY after endpoint resolution"
            );
        }
        if let Some(root) = runtime_plan.env_overrides.get("CTX_DATA_ROOT") {
            let expected_home = provider_accounts::codex_runtime_home(Path::new(root));
            if Path::new(&codex_home) != expected_home {
                anyhow::bail!(
                    "codex endpoint container runtime must use CODEX_HOME={} but resolved {}",
                    expected_home.display(),
                    codex_home
                );
            }
        }
    }

    Ok(())
}
