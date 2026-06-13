use super::*;

pub(super) struct SubscriptionRuntimeCredentialRequest<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) provider_env: &'a mut HashMap<String, String>,
    pub(super) runtime_provider_id: &'a str,
    pub(super) runtime_plan: &'a ctx_harness_runtime::HarnessExecutionPlan,
    pub(super) is_linux_sandbox: bool,
}

pub(super) async fn prepare_subscription_runtime_credentials(
    request: SubscriptionRuntimeCredentialRequest<'_>,
) -> Result<()> {
    let SubscriptionRuntimeCredentialRequest {
        provider_launch,
        provider_env,
        runtime_provider_id,
        runtime_plan,
        is_linux_sandbox,
    } = request;
    let env = if is_linux_sandbox {
        if let Some(root) = runtime_plan.env_overrides.get("CTX_DATA_ROOT") {
            provider_accounts::subscription_env_for_active_account_with_runtime_root(
                provider_launch.data_root(),
                Path::new(root),
                runtime_provider_id,
            )
            .await?
        } else {
            provider_accounts::subscription_env_for_active_account(
                provider_launch.data_root(),
                runtime_provider_id,
            )
            .await?
        }
    } else {
        provider_accounts::subscription_env_for_active_account(
            provider_launch.data_root(),
            runtime_provider_id,
        )
        .await?
    };
    for (key, value) in env {
        provider_env.insert(key, value);
    }

    Ok(())
}
