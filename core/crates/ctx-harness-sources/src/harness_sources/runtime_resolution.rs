use super::*;
use ctx_provider_accounts::{
    apply_gemini_api_key_runtime_auth_env, apply_gemini_vertex_runtime_auth_env,
    write_gemini_auth_settings, GEMINI_AUTH_SELECTED_TYPE_API_KEY,
    GEMINI_AUTH_SELECTED_TYPE_VERTEX_AI, KIMI_SHARE_DIR_ENV,
};
mod provider_env;
mod provider_fs;
mod provider_relay;

use self::provider_fs::{
    amp_subscription_home, endpoint_preferred_model_id, goose_subscription_path_root,
    kimi_endpoint_home, normalize_openhands_endpoint_model_id,
    prepare_cline_home_with_endpoint_settings, prepare_codex_home_with_api_key,
    prepare_droid_home_with_endpoint_settings, prepare_goose_endpoint_path_root,
    prepare_kimi_share_dir, prepare_openhands_persistence_dir, prepare_openhands_python_hook_dir,
    prepare_qwen_home_with_openai_settings, prepend_pythonpath,
};
pub(super) use self::provider_fs::{
    cline_endpoint_home, codex_endpoint_home, droid_endpoint_home, gemini_endpoint_home,
    goose_endpoint_path_root, legacy_codex_endpoint_home, qwen_endpoint_home,
};
#[cfg(test)]
pub(crate) use self::provider_fs::{openhands_endpoint_home, seed_droid_auth_from_host_path};
use self::provider_relay::ctx_managed_relay_env;

pub fn droid_cli_model_id_for_endpoint_model(
    model_id: Option<&str>,
    base_url: Option<&str>,
) -> Option<String> {
    provider_fs::droid_cli_model_id_for_endpoint_model(model_id, base_url)
}

pub async fn resolve_provider_source_for_probe(
    data_root: &Path,
    provider_id: &str,
) -> Result<ResolvedHarnessSource> {
    resolve_provider_source_for_probe_with_runtime_root(data_root, provider_id, None).await
}

pub async fn resolve_provider_source_for_probe_with_runtime_root(
    data_root: &Path,
    provider_id: &str,
    runtime_data_root: Option<&Path>,
) -> Result<ResolvedHarnessSource> {
    resolve_internal(data_root, provider_id, false, runtime_data_root).await
}

pub async fn resolve_provider_source_for_run(
    data_root: &Path,
    provider_id: &str,
) -> Result<ResolvedHarnessSource> {
    resolve_provider_source_for_run_with_runtime_root(data_root, provider_id, None).await
}

pub async fn resolve_provider_source_for_run_with_runtime_root(
    data_root: &Path,
    provider_id: &str,
    runtime_data_root: Option<&Path>,
) -> Result<ResolvedHarnessSource> {
    let require_verified_endpoint = validation::normalize_provider_id(provider_id)
        .is_some_and(validation::provider_requires_verified_endpoint_for_run);
    resolve_internal(
        data_root,
        provider_id,
        require_verified_endpoint,
        runtime_data_root,
    )
    .await
}

pub(super) struct ProviderRuntimeContext<'a> {
    canonical: &'static str,
    data_root: &'a Path,
    runtime_data_root: Option<&'a Path>,
}

impl<'a> ProviderRuntimeContext<'a> {
    pub(super) fn new(
        canonical: &'static str,
        data_root: &'a Path,
        runtime_data_root: Option<&'a Path>,
    ) -> Self {
        Self {
            canonical,
            data_root,
            runtime_data_root,
        }
    }

    async fn endpoint_env(
        &self,
        endpoint: &HarnessEndpointRecordInternal,
        secret: &secrets::EndpointSecretMaterial,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();

        match self.canonical {
            PROVIDER_CODEX => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let codex_home = codex_endpoint_home(self.data_root, &endpoint.id);
                let legacy_codex_home = legacy_codex_endpoint_home(self.data_root, &endpoint.id);
                if !codex_home.exists() && legacy_codex_home.exists() {
                    if let Some(parent) = codex_home.parent() {
                        ctx_fs::permissions::ensure_private_dir(parent)
                            .await
                            .with_context(|| {
                                format!(
                                    "creating canonical codex endpoint parent for endpoint {}",
                                    endpoint.id
                                )
                            })?;
                    }
                    tokio::fs::rename(&legacy_codex_home, &codex_home)
                        .await
                        .with_context(|| {
                            format!(
                                "migrating adapter-keyed codex endpoint home to codex for endpoint {}",
                                endpoint.id
                            )
                        })?;
                }
                prepare_codex_home_with_api_key(&codex_home, &api_key, &base_url).await?;
                env.insert(
                    "CODEX_HOME".to_string(),
                    codex_home.to_string_lossy().to_string(),
                );
                env.insert("OPENAI_API_KEY".to_string(), api_key);
                if let Some(provider_namespace) =
                    model_catalog::infer_endpoint_model_provider_namespace(&base_url)
                {
                    env.insert("CTX_MODEL_PROVIDER".to_string(), provider_namespace);
                }
                env.insert("OPENAI_BASE_URL".to_string(), base_url);
            }
            PROVIDER_CLINE => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let model_id = endpoint_preferred_model_id(endpoint).ok_or_else(|| {
                    anyhow::anyhow!(
                        "selected endpoint '{}' for {} is missing a concrete model id",
                        endpoint.name,
                        self.canonical
                    )
                })?;
                let cline_dir = prepare_cline_home_with_endpoint_settings(
                    &cline_endpoint_home(self.runtime_data_root(), &endpoint.id),
                    &api_key,
                    &model_id,
                    &base_url,
                )
                .await?;
                env.insert(
                    "CLINE_DIR".to_string(),
                    cline_dir.to_string_lossy().to_string(),
                );
                env.insert("CLINE_NO_AUTO_UPDATE".to_string(), "1".to_string());
                env.insert("OPENAI_MODEL".to_string(), model_id);
                env.insert(
                    "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
                    "1".to_string(),
                );
                env.insert("CTX_PROVIDER_MODE".to_string(), "act".to_string());
            }
            PROVIDER_CLAUDE => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                env.insert("ANTHROPIC_API_KEY".to_string(), api_key);
                env.insert("ANTHROPIC_BASE_URL".to_string(), base_url);
            }
            PROVIDER_GEMINI => {
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let gemini_home = gemini_endpoint_home(self.runtime_data_root(), &endpoint.id);
                let gemini_dir = gemini_home.join(".gemini");
                ctx_fs::permissions::ensure_private_dir(&gemini_home)
                    .await
                    .with_context(|| {
                        format!("creating gemini endpoint home for endpoint {}", endpoint.id)
                    })?;
                ctx_fs::permissions::ensure_private_dir(&gemini_dir)
                    .await
                    .with_context(|| {
                        format!("creating gemini endpoint home for endpoint {}", endpoint.id)
                    })?;
                env.insert(
                    "HOME".to_string(),
                    gemini_home.to_string_lossy().to_string(),
                );
                env.insert(
                    "GEMINI_CLI_HOME".to_string(),
                    gemini_home.to_string_lossy().to_string(),
                );
                env.insert("GEMINI_FORCE_FILE_STORAGE".to_string(), "true".to_string());
                match endpoint.auth_type.as_str() {
                    GEMINI_AUTH_TYPE_VERTEX_AI => {
                        let vertex_secret = secrets::endpoint_secret_gemini_vertex(secret)?;
                        let credentials_path = gemini_dir.join("vertex-service-account.json");
                        ctx_fs::permissions::write_private_file_atomic(
                            &credentials_path,
                            vertex_secret.service_account_json.as_bytes(),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "writing Gemini Vertex service account JSON {}",
                                credentials_path.display()
                            )
                        })?;
                        apply_gemini_vertex_runtime_auth_env(
                            &mut env,
                            credentials_path,
                            vertex_secret.project_id,
                            vertex_secret.location,
                        );
                        write_gemini_auth_settings(
                            &gemini_dir,
                            GEMINI_AUTH_SELECTED_TYPE_VERTEX_AI,
                        )
                        .await?;
                    }
                    GEMINI_AUTH_TYPE_GEMINI_API_KEY => {
                        let api_key = secrets::endpoint_secret_api_key(secret)?;
                        apply_gemini_api_key_runtime_auth_env(&mut env, api_key);
                        write_gemini_auth_settings(&gemini_dir, GEMINI_AUTH_SELECTED_TYPE_API_KEY)
                            .await?;
                    }
                    _ => {
                        anyhow::bail!(
                            "unsupported gemini endpoint auth_type '{}' (use '{}' or '{}')",
                            endpoint.auth_type,
                            GEMINI_AUTH_TYPE_GEMINI_API_KEY,
                            GEMINI_AUTH_TYPE_VERTEX_AI
                        );
                    }
                }
            }
            PROVIDER_KIMI => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                let model_id = endpoint_preferred_model_id(endpoint).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Kimi endpoint '{}' requires a model override or discovered model catalog before launch",
                        endpoint.name
                    )
                })?;
                let kimi_share_dir =
                    prepare_kimi_share_dir(self.runtime_data_root(), &endpoint.id).await?;
                env.insert("OPENAI_API_KEY".to_string(), api_key.clone());
                env.insert("OPENAI_BASE_URL".to_string(), base_url.clone());
                env.insert("OPENAI_MODEL".to_string(), model_id.clone());
                env.insert("KIMI_API_KEY".to_string(), api_key);
                env.insert("KIMI_BASE_URL".to_string(), base_url);
                env.insert("KIMI_MODEL_NAME".to_string(), model_id);
                env.insert(
                    KIMI_SHARE_DIR_ENV.to_string(),
                    kimi_share_dir.to_string_lossy().to_string(),
                );
                env.insert(
                    "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
                    "1".to_string(),
                );
            }
            PROVIDER_QWEN => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let qwen_home = qwen_endpoint_home(self.runtime_data_root(), &endpoint.id);
                prepare_qwen_home_with_openai_settings(&qwen_home).await?;
                env.insert("HOME".to_string(), qwen_home.to_string_lossy().to_string());
                env.insert("OPENAI_API_KEY".to_string(), api_key);
                env.insert("OPENAI_BASE_URL".to_string(), base_url);
                if let Some(model) = endpoint
                    .model_override
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                {
                    env.insert("OPENAI_MODEL".to_string(), model);
                }
            }
            PROVIDER_OPENCODE => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                let provider_namespace =
                    model_catalog::infer_endpoint_model_provider_namespace(&base_url)
                        .unwrap_or_else(|| "endpoint".to_string());
                env.insert("OPENAI_API_KEY".to_string(), api_key.clone());
                env.insert("OPENAI_BASE_URL".to_string(), base_url.clone());
                if provider_namespace == "openrouter" {
                    env.insert("OPENROUTER_API_KEY".to_string(), api_key.clone());
                    env.insert("OPENROUTER_BASE_URL".to_string(), base_url.clone());
                }

                let mut provider_config = serde_json::Map::new();
                provider_config.insert(
                    provider_namespace.clone(),
                    serde_json::json!({
                        "options": {
                            "baseURL": base_url,
                            "apiKey": api_key,
                        }
                    }),
                );
                let mut root = serde_json::Map::new();
                if let Some(model) = endpoint
                    .model_override
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                {
                    root.insert(
                        "model".to_string(),
                        serde_json::Value::String(
                            model_catalog::normalize_namespaced_model_override(
                                &model,
                                Some(provider_namespace.as_str()),
                            ),
                        ),
                    );
                }
                root.insert(
                    "permission".to_string(),
                    serde_json::json!({
                        "edit": "deny",
                        "bash": "allow",
                    }),
                );
                root.insert(
                    "provider".to_string(),
                    serde_json::Value::Object(provider_config),
                );
                env.insert(
                    "OPENCODE_CONFIG_CONTENT".to_string(),
                    serde_json::Value::Object(root).to_string(),
                );
            }
            PROVIDER_GOOSE => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                if model_catalog::infer_endpoint_model_provider_namespace(&base_url).as_deref()
                    != Some("openrouter")
                {
                    anyhow::bail!(
                        "goose harness endpoints currently require an OpenRouter base_url; found {base_url}"
                    );
                }
                let path_root = goose_endpoint_path_root(self.runtime_data_root(), &endpoint.id);
                prepare_goose_endpoint_path_root(&path_root).await?;
                env.insert("OPENROUTER_API_KEY".to_string(), api_key);
                env.insert("GOOSE_PROVIDER".to_string(), "openrouter".to_string());
                env.insert(
                    "GOOSE_PATH_ROOT".to_string(),
                    path_root.to_string_lossy().to_string(),
                );
                env.insert("GOOSE_DISABLE_KEYRING".to_string(), "1".to_string());
                env.insert("GOOSE_MODE".to_string(), "auto".to_string());
                // Override the host-wide provider mode so Goose uses its own ACP defaults.
                env.insert("CTX_PROVIDER_MODE".to_string(), String::new());
                if let Some(model) = endpoint
                    .model_override
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                {
                    env.insert("GOOSE_MODEL".to_string(), model.clone());
                }
            }
            PROVIDER_MISTRAL => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                env.insert("MISTRAL_API_KEY".to_string(), api_key.clone());
                env.insert("MISTRAL_BASE_URL".to_string(), base_url.clone());
                env.insert("OPENAI_API_KEY".to_string(), api_key);
                env.insert("OPENAI_BASE_URL".to_string(), base_url);
            }
            PROVIDER_AMP => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                env.insert("AMP_API_KEY".to_string(), api_key);
            }
            PROVIDER_DROID => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let droid_home = droid_endpoint_home(self.runtime_data_root(), &endpoint.id);
                let model_id = endpoint_preferred_model_id(endpoint).ok_or_else(|| {
                    anyhow::anyhow!(
                        "selected endpoint '{}' for {} is missing a concrete model id",
                        endpoint.name,
                        self.canonical
                    )
                })?;
                let droid_default_model = prepare_droid_home_with_endpoint_settings(
                    &droid_home,
                    &base_url,
                    &api_key,
                    &model_id,
                )
                .await?;
                env.insert("HOME".to_string(), droid_home.to_string_lossy().to_string());
                env.insert("OPENAI_API_KEY".to_string(), api_key);
                env.insert("OPENAI_BASE_URL".to_string(), base_url);
                if let Ok(factory_api_key) = std::env::var("FACTORY_API_KEY") {
                    let trimmed = factory_api_key.trim();
                    if !trimmed.is_empty() {
                        env.insert("FACTORY_API_KEY".to_string(), trimmed.to_string());
                    }
                }
                if let Some(model) = droid_default_model {
                    env.insert("DROID_DEFAULT_MODEL".to_string(), model);
                }
            }
            PROVIDER_OPENHANDS => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                let base_url = validation::endpoint_base_url_or_err(endpoint)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                validation::ensure_safe_endpoint_id(&endpoint.id)?;
                let model_id = endpoint_preferred_model_id(endpoint).ok_or_else(|| {
                    anyhow::anyhow!(
                        "selected endpoint '{}' for {} is missing a concrete model id",
                        endpoint.name,
                        self.canonical
                    )
                })?;
                let normalized_model_id =
                    normalize_openhands_endpoint_model_id(&base_url, &model_id);
                let persistence_dir = prepare_openhands_persistence_dir(
                    &provider_fs::openhands_endpoint_home(self.runtime_data_root(), &endpoint.id),
                    &api_key,
                    &normalized_model_id,
                    &base_url,
                )
                .await?;
                let python_hook_dir = prepare_openhands_python_hook_dir(&persistence_dir).await?;
                env.insert("LLM_API_KEY".to_string(), api_key.clone());
                env.insert("LLM_BASE_URL".to_string(), base_url.clone());
                env.insert("LLM_MODEL".to_string(), normalized_model_id);
                env.insert(
                    "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
                    "1".to_string(),
                );
                env.insert(
                    "OPENHANDS_PERSISTENCE_DIR".to_string(),
                    persistence_dir.to_string_lossy().to_string(),
                );
                env.insert(
                    "PYTHONPATH".to_string(),
                    prepend_pythonpath(&python_hook_dir)?
                        .to_string_lossy()
                        .to_string(),
                );
                env.insert(
                    "CTX_PROVIDER_MODE".to_string(),
                    "always-approve".to_string(),
                );
            }
            PROVIDER_COPILOT => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                env.insert("GH_TOKEN".to_string(), api_key.clone());
                env.insert("GITHUB_TOKEN".to_string(), api_key);
            }
            PROVIDER_PI => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                let provider =
                    model_catalog::infer_endpoint_model_provider_namespace(&endpoint.base_url)
                        .unwrap_or_else(|| "openai".to_string());
                if provider == "openrouter" {
                    env.insert("OPENROUTER_API_KEY".to_string(), api_key);
                } else {
                    env.insert("OPENAI_API_KEY".to_string(), api_key);
                }
                env.insert("PI_ACP_PROVIDER".to_string(), provider);
                let base_url = endpoint.base_url.trim().to_string();
                if !base_url.is_empty() {
                    env.insert("OPENAI_BASE_URL".to_string(), base_url);
                }
                if let Some(model) = endpoint
                    .model_override
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                {
                    env.insert("PI_ACP_MODEL".to_string(), model);
                }
            }
            PROVIDER_AUGGIE => {
                let api_key = secrets::endpoint_secret_api_key(secret)?;
                validation::ensure_shape_compatible(self.canonical, endpoint.api_shape)?;
                env.insert("AUGMENT_SESSION_AUTH".to_string(), api_key.clone());
                env.insert("AUGMENT_API_TOKEN".to_string(), api_key);
            }
            _ => {}
        }

        Ok(env)
    }
}

async fn resolve_internal(
    data_root: &Path,
    provider_id: &str,
    require_verified_endpoint: bool,
    runtime_data_root: Option<&Path>,
) -> Result<ResolvedHarnessSource> {
    if provider_id == "fake" {
        return Ok(ResolvedHarnessSource {
            source_kind: HarnessSourceKind::Subscription,
            endpoint: None,
            env: HashMap::new(),
        });
    }
    let canonical = match validation::normalize_provider_id(provider_id) {
        Some(id) => id,
        None => {
            return Ok(ResolvedHarnessSource {
                source_kind: HarnessSourceKind::Subscription,
                endpoint: None,
                env: HashMap::new(),
            });
        }
    };
    let runtime = ProviderRuntimeContext::new(canonical, data_root, runtime_data_root);
    let endpoint_supported = validation::provider_supports_harness_endpoint(canonical);

    let provider =
        selection::load_provider_internal(data_root, canonical, endpoint_supported).await?;

    if provider.selected_source_kind != HarnessSourceKind::Endpoint {
        return Ok(ResolvedHarnessSource {
            source_kind: HarnessSourceKind::Subscription,
            endpoint: None,
            env: runtime.subscription_env(),
        });
    }

    if !endpoint_supported {
        return Ok(ResolvedHarnessSource {
            source_kind: HarnessSourceKind::Subscription,
            endpoint: None,
            env: runtime.subscription_env(),
        });
    }

    let endpoint_id = provider
        .selected_endpoint_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no endpoint selected for provider {canonical}"))?;
    let endpoint = provider
        .endpoints
        .iter()
        .find(|ep| ep.id == endpoint_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("selected endpoint not found for provider {canonical}"))?;

    if require_verified_endpoint
        && endpoint.last_verification_status != HarnessEndpointVerificationStatus::Valid
    {
        anyhow::bail!(
            "selected endpoint '{}' for {} is not verified; verify it in Settings before running",
            endpoint.name,
            canonical
        );
    }

    let public = selection::public_endpoint_from_internal(data_root, &endpoint).await;
    if public.route_backend() == HarnessRouteBackend::CtxManagedRelay {
        let env = ctx_managed_relay_env(canonical, &endpoint)?;
        return Ok(ResolvedHarnessSource {
            source_kind: HarnessSourceKind::Endpoint,
            endpoint: Some(public),
            env,
        });
    }

    let secret = secrets::read_endpoint_secret(data_root, &endpoint.secret_ref).await?;
    let env = runtime.endpoint_env(&endpoint, &secret).await?;
    Ok(ResolvedHarnessSource {
        source_kind: HarnessSourceKind::Endpoint,
        endpoint: Some(public),
        env,
    })
}
