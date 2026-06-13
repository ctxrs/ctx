use super::*;
use ctx_provider_runtime::provider_launch::options::runtime_probe_models_payload;
use ctx_provider_runtime::provider_launch::probe::provider_probe_context_for_workspace_runtime;

pub(super) async fn load_runtime_model_catalog(
    host: &impl ModelCatalogHost,
    workspace: &Workspace,
    provider_id: &str,
    install_target: ctx_provider_install::install_state::InstallTarget,
    cache_key: String,
    pinned_catalog: Option<ModelCatalog>,
) -> Result<Option<ModelCatalog>, String> {
    let (cfg, config_error) =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_with_error(
            ProviderRuntimeHost::data_root(host),
        )
        .await;
    if let Some(config_error) = config_error {
        return Err(config_error);
    }
    let runtime_command = match installer::resolve_runtime_provider_command_for_target(
        &cfg,
        provider_id,
        Some(install_target),
    ) {
        Ok(Some(command)) => command,
        Ok(None) => return Ok(pinned_catalog),
        Err(err) => {
            tracing::warn!(
                provider_id = provider_id,
                "provider runtime command resolution failed: {}",
                logs::redact_sensitive(&err.to_string())
            );
            return Ok(pinned_catalog);
        }
    };
    let command = runtime_command.command_abs_path;
    let args = runtime_command.args;

    let probe_context =
        match provider_probe_context_for_workspace_runtime(host, workspace, provider_id).await {
            Ok(context) => context,
            Err(err) => {
                tracing::warn!(
                    provider_id = provider_id,
                    "provider probe runtime context failed: {}",
                    logs::redact_sensitive(&err)
                );
                return Ok(pinned_catalog);
            }
        };
    let mut env = probe_context.env;
    installer::prepend_runtime_bin_dirs_to_provider_path_for_target(
        &mut env,
        &cfg,
        provider_id,
        ProviderRuntimeHost::data_root(host),
        Some(install_target),
    );
    if let Err(err) = installer::ensure_codex_cli_command_env_for_target(
        &mut env,
        &cfg,
        provider_id,
        Some(install_target),
    ) {
        tracing::warn!(
            provider_id = provider_id,
            "provider codex-cli runtime path resolution failed: {}",
            logs::redact_sensitive(&err.to_string())
        );
        return Ok(pinned_catalog);
    }

    let probe = match probe_crp_models(provider_id, command, args, probe_context.cwd, env).await {
        Ok(probe) => probe,
        Err(e) => {
            tracing::warn!(
                provider_id = provider_id,
                "provider options probe failed: {}",
                logs::redact_sensitive(&e.to_string())
            );
            return Ok(pinned_catalog);
        }
    };

    let fallback_current_model_id = pinned_catalog
        .as_ref()
        .and_then(ModelCatalog::default_model_id);
    let Some(models_value) =
        runtime_probe_models_payload(provider_id, &probe, fallback_current_model_id)
    else {
        return Ok(pinned_catalog);
    };
    let Some(models) = build_model_catalog(&models_value) else {
        return Ok(pinned_catalog);
    };

    let mut value = serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace.id.0,
        "installed": true,
        "probe_ok": true,
        "supports_load": false,
        "auth_required": false,
        "models": models_value,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    value = redact_json_value(value);
    host.provider_runtime()
        .store_provider_options_cache_value(cache_key, value)
        .await;
    Ok(Some(models))
}
