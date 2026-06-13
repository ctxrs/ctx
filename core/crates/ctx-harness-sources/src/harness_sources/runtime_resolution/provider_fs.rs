use super::*;

pub(crate) fn droid_cli_model_id_for_endpoint_model(
    model_id: Option<&str>,
    base_url: Option<&str>,
) -> Option<String> {
    let model_id = model_id?;
    let base_url = base_url?;
    let display_name = droid_custom_model_display_name(model_id, base_url)?;
    droid_cli_model_id_from_display_name(&display_name)
}

pub(crate) fn codex_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("codex")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn legacy_codex_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("codex-crp")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn cline_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("cline")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn openhands_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("openhands")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn qwen_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("qwen")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn kimi_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("kimi")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn gemini_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("gemini")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn droid_endpoint_home(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("droid")
        .join("endpoint-homes")
        .join(endpoint_id)
}

pub(crate) fn amp_subscription_home(data_root: &Path, runtime_data_root: Option<&Path>) -> PathBuf {
    runtime_data_root
        .unwrap_or(data_root)
        .join("providers")
        .join("amp")
        .join("home")
}

pub(crate) fn goose_subscription_path_root(
    data_root: &Path,
    runtime_data_root: Option<&Path>,
) -> PathBuf {
    runtime_data_root
        .unwrap_or(data_root)
        .join("providers")
        .join("goose")
        .join("path-root")
}

pub(crate) fn goose_endpoint_path_root(data_root: &Path, endpoint_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("goose")
        .join("endpoint-path-roots")
        .join(endpoint_id)
}

pub(crate) async fn prepare_goose_endpoint_path_root(path_root: &Path) -> Result<PathBuf> {
    let config_dir = path_root.join("config");
    ctx_fs::permissions::ensure_private_dir(path_root).await?;
    ctx_fs::permissions::ensure_private_dir(&config_dir).await?;
    let config_path = config_dir.join("config.yaml");
    let config = r#"extensions:
  analyze:
    enabled: false
  chatrecall:
    enabled: false
  extensionmanager:
    enabled: false
  todo:
    enabled: false
  developer:
    enabled: true
  summon:
    enabled: false
  code_execution:
    enabled: false
  tom:
    enabled: false
  apps:
    enabled: false
"#;
    ctx_fs::permissions::write_private_file_atomic(&config_path, config.as_bytes()).await?;
    Ok(path_root.to_path_buf())
}

pub(crate) async fn prepare_codex_home_with_api_key(
    codex_home: &Path,
    api_key: &str,
    base_url: &str,
) -> Result<()> {
    ctx_fs::permissions::ensure_private_dir(codex_home).await?;
    let auth_path = codex_home.join("auth.json");
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "OPENAI_API_KEY": api_key,
        "OPENAI_BASE_URL": base_url,
    }))?;
    ctx_fs::permissions::write_private_file_atomic(&auth_path, &payload).await?;
    Ok(())
}

pub(crate) async fn prepare_cline_home_with_endpoint_settings(
    cline_home: &Path,
    api_key: &str,
    model_id: &str,
    base_url: &str,
) -> Result<PathBuf> {
    let data_dir = cline_home.join("data");
    let settings_dir = data_dir.join("settings");
    ctx_fs::permissions::ensure_private_dir(cline_home).await?;
    ctx_fs::permissions::ensure_private_dir(&data_dir).await?;
    ctx_fs::permissions::ensure_private_dir(&settings_dir).await?;

    let provider_namespace = model_catalog::infer_endpoint_model_provider_namespace(base_url)
        .unwrap_or_else(|| "endpoint".to_string());
    let global_state_path = data_dir.join("globalState.json");
    let global_state = if provider_namespace == "openrouter" {
        serde_json::to_vec_pretty(&serde_json::json!({
            "actModeApiProvider": "openrouter",
            "planModeApiProvider": "openrouter",
            "actModeOpenRouterModelId": model_id,
            "planModeOpenRouterModelId": model_id,
            "welcomeViewCompleted": true,
        }))?
    } else {
        serde_json::to_vec_pretty(&serde_json::json!({
            "actModeApiProvider": "openai-native",
            "planModeApiProvider": "openai-native",
            "actModeApiModelId": model_id,
            "planModeApiModelId": model_id,
            "openAiBaseUrl": base_url,
            "welcomeViewCompleted": true,
        }))?
    };
    ctx_fs::permissions::write_private_file_atomic(&global_state_path, &global_state).await?;

    let secrets_path = data_dir.join("secrets.json");
    let secrets = if provider_namespace == "openrouter" {
        serde_json::to_vec_pretty(&serde_json::json!({
            "openRouterApiKey": api_key,
        }))?
    } else {
        serde_json::to_vec_pretty(&serde_json::json!({
            "openAiNativeApiKey": api_key,
        }))?
    };
    ctx_fs::permissions::write_private_file_atomic(&secrets_path, &secrets).await?;

    let mcp_settings_path = settings_dir.join("cline_mcp_settings.json");
    let mcp_settings = serde_json::to_vec_pretty(&serde_json::json!({
        "mcpServers": {},
    }))?;
    ctx_fs::permissions::write_private_file_atomic(&mcp_settings_path, &mcp_settings).await?;

    Ok(cline_home.to_path_buf())
}

pub(crate) fn normalize_openhands_endpoint_model_id(base_url: &str, model_id: &str) -> String {
    if model_catalog::infer_endpoint_model_provider_namespace(base_url).as_deref()
        == Some("openrouter")
        && !model_id.starts_with("openrouter/")
    {
        return format!("openrouter/{model_id}");
    }
    model_id.to_string()
}

pub(crate) async fn prepare_openhands_persistence_dir(
    persistence_dir: &Path,
    api_key: &str,
    model_id: &str,
    base_url: &str,
) -> Result<PathBuf> {
    ctx_fs::permissions::ensure_private_dir(persistence_dir).await?;
    let agent_settings_path = persistence_dir.join("agent_settings.json");
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "llm": {
            "model": model_id,
            "api_key": api_key,
            "base_url": base_url,
            "usage_id": "agent",
        },
        "tools": [
            { "name": "file_editor", "params": {} },
        ],
        "mcp_config": {},
        "kind": "Agent",
    }))?;
    ctx_fs::permissions::write_private_file_atomic(&agent_settings_path, &payload).await?;
    Ok(persistence_dir.to_path_buf())
}

pub(crate) async fn prepare_openhands_python_hook_dir(persistence_dir: &Path) -> Result<PathBuf> {
    let hook_dir = persistence_dir.join("pyhooks");
    ctx_fs::permissions::ensure_private_dir(&hook_dir).await?;
    let hook_path = hook_dir.join("sitecustomize.py");
    let hook = r#"import os

if os.environ.get("OPENHANDS_PERSISTENCE_DIR"):
    from openhands_cli.stores.agent_store import (
        AgentStore,
        get_default_cli_tools,
        get_persisted_conversation_tools,
    )

    def _ctx_resolve_tools(self, session_id):
        tools = get_persisted_conversation_tools(session_id) if session_id else None
        if tools:
            return tools
        agent = self.load_from_disk()
        if agent is not None and agent.tools:
            return list(agent.tools)
        return get_default_cli_tools()

    AgentStore._resolve_tools = _ctx_resolve_tools
"#;
    ctx_fs::permissions::write_private_file_atomic(&hook_path, hook.as_bytes()).await?;
    Ok(hook_dir)
}

pub(crate) fn prepend_pythonpath(path: &Path) -> Result<std::ffi::OsString> {
    let mut parts = vec![path.as_os_str().to_os_string()];
    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        parts.extend(std::env::split_paths(&existing).map(|part| part.into_os_string()));
    }
    std::env::join_paths(parts).map_err(|err| anyhow::anyhow!("joining PYTHONPATH: {err}"))
}

pub(crate) async fn prepare_qwen_home_with_openai_settings(qwen_home: &Path) -> Result<()> {
    let qwen_config = qwen_home.join(".qwen");
    ctx_fs::permissions::ensure_private_dir(qwen_home).await?;
    ctx_fs::permissions::ensure_private_dir(&qwen_config).await?;
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "$version": 2,
        "security": {
            "auth": {
                "selectedType": "openai"
            }
        }
    }))?;
    ctx_fs::permissions::write_private_file_atomic(&qwen_config.join("settings.json"), &payload)
        .await?;
    Ok(())
}

pub(crate) async fn prepare_kimi_share_dir(
    runtime_data_root: &Path,
    endpoint_id: &str,
) -> Result<PathBuf> {
    let share_dir = kimi_endpoint_home(runtime_data_root, endpoint_id).join(".kimi");
    let credentials_dir = share_dir.join("credentials");
    ctx_fs::permissions::ensure_private_dir(&share_dir).await?;
    ctx_fs::permissions::ensure_private_dir(&credentials_dir).await?;
    // Kimi currently refuses endpoint/API-key sessions unless a file-backed token exists.
    // Seed a benign token in the isolated endpoint runtime home so the CLI reaches the
    // actual endpoint-auth path instead of aborting with auth_required before turn start.
    let token_path = credentials_dir.join("kimi-code.json");
    let token = serde_json::json!({
        "access_token": "ctx-endpoint-access-token",
        "refresh_token": "ctx-endpoint-refresh-token",
        "expires_at": 4_102_444_800.0,
        "scope": "openid profile",
        "token_type": "Bearer",
    });
    let token_payload = token.to_string();
    ctx_fs::permissions::write_private_file_atomic(&token_path, token_payload.as_bytes()).await?;
    Ok(share_dir)
}

pub(crate) fn endpoint_preferred_model_id(
    endpoint: &HarnessEndpointRecordInternal,
) -> Option<String> {
    endpoint
        .model_override
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            endpoint.manual_model_ids.iter().find_map(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
        .or_else(|| {
            endpoint.model_catalog_models.iter().find_map(|record| {
                let trimmed = record.id.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
}

fn droid_custom_model_name(model_id: &str) -> Option<String> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_prefix = trimmed
        .strip_prefix("custom:")
        .map(str::trim)
        .unwrap_or(trimmed);
    if without_prefix.is_empty() {
        None
    } else {
        Some(without_prefix.to_string())
    }
}

fn droid_backend_model_id(model_id: &str) -> Option<String> {
    droid_custom_model_name(model_id)
}

fn droid_custom_model_display_name(model_id: &str, base_url: &str) -> Option<String> {
    let backend_model_id = droid_backend_model_id(model_id)?;
    let namespace = model_catalog::infer_endpoint_model_provider_namespace(base_url)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "endpoint".to_string());
    Some(format!("{backend_model_id} [{namespace}]"))
}

fn droid_cli_model_id_from_display_name(display_name: &str) -> Option<String> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "custom:{}-0",
        trimmed.split_whitespace().collect::<Vec<_>>().join("-")
    ))
}

fn droid_host_auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var(CTX_DROID_HOST_AUTH_PATH_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    let base = BaseDirs::new().ok_or_else(|| anyhow::anyhow!("missing home dir"))?;
    Ok(base.home_dir().join(".factory").join("auth.encrypted"))
}

pub(crate) async fn seed_droid_auth_from_host_path(
    droid_home: &Path,
    host_auth_path: &Path,
) -> Result<bool> {
    if !host_auth_path.exists() {
        return Ok(false);
    }
    let bytes = tokio::fs::read(host_auth_path).await?;
    if bytes.is_empty() {
        anyhow::bail!(
            "host droid auth file is empty: {}",
            host_auth_path.display()
        );
    }

    let droid_config = droid_home.join(".factory");
    ctx_fs::permissions::ensure_private_dir(droid_home).await?;
    ctx_fs::permissions::ensure_private_dir(&droid_config).await?;
    let dest = droid_config.join("auth.encrypted");
    let should_write = match tokio::fs::read(&dest).await {
        Ok(existing) => existing != bytes,
        Err(_) => true,
    };
    if should_write {
        ctx_fs::permissions::write_private_file_atomic(&dest, &bytes).await?;
    }

    Ok(should_write)
}

async fn maybe_seed_droid_auth_from_host(droid_home: &Path) -> Result<bool> {
    let host_auth_path = droid_host_auth_path()?;
    seed_droid_auth_from_host_path(droid_home, &host_auth_path).await
}

pub(crate) async fn prepare_droid_home_with_endpoint_settings(
    droid_home: &Path,
    base_url: &str,
    api_key: &str,
    model_id: &str,
) -> Result<Option<String>> {
    let Some(display_name) = droid_custom_model_display_name(model_id, base_url) else {
        return Ok(None);
    };
    let Some(backend_model_id) = droid_backend_model_id(model_id) else {
        return Ok(None);
    };
    let droid_config = droid_home.join(".factory");
    ctx_fs::permissions::ensure_private_dir(droid_home).await?;
    ctx_fs::permissions::ensure_private_dir(&droid_config).await?;
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "customModels": [
            {
                "model": backend_model_id,
                "displayName": display_name,
                "provider": "generic-chat-completion-api",
                "baseUrl": base_url,
                "apiKey": api_key,
            }
        ]
    }))?;
    let settings_path = droid_config.join("settings.json");
    ctx_fs::permissions::write_private_file_atomic(&settings_path, &payload).await?;
    let _ = maybe_seed_droid_auth_from_host(droid_home).await?;
    Ok(droid_cli_model_id_from_display_name(&display_name))
}
