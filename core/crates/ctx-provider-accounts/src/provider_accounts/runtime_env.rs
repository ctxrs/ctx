use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use super::codex_runtime_home;
use super::shared::write_secure_file_atomic;

async fn codex_endpoint_api_key_from_provider_env(
    provider_env: &HashMap<String, String>,
) -> Result<String> {
    if let Some(api_key) = provider_env.get("OPENAI_API_KEY") {
        let trimmed = api_key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let endpoint_home = provider_env
        .get("CODEX_HOME")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "missing OPENAI_API_KEY and CODEX_HOME while preparing codex endpoint runtime home"
            )
        })?;
    let auth_path = Path::new(endpoint_home).join("auth.json");
    let payload = tokio::fs::read_to_string(&auth_path)
        .await
        .with_context(|| format!("reading endpoint auth from {}", auth_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("parsing endpoint auth JSON at {}", auth_path.display()))?;
    let api_key = parsed
        .get("OPENAI_API_KEY")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "endpoint auth at {} has no OPENAI_API_KEY",
                auth_path.display()
            )
        })?;
    Ok(api_key.to_string())
}

async fn codex_endpoint_base_url_from_provider_env(
    provider_env: &HashMap<String, String>,
) -> Result<Option<String>> {
    if let Some(base_url) = provider_env.get("OPENAI_BASE_URL") {
        let trimmed = base_url.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    let Some(endpoint_home) = provider_env
        .get("CODEX_HOME")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let auth_path = Path::new(endpoint_home).join("auth.json");
    let payload = match tokio::fs::read_to_string(&auth_path).await {
        Ok(payload) => payload,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading endpoint auth from {}", auth_path.display()));
        }
    };
    let parsed: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("parsing endpoint auth JSON at {}", auth_path.display()))?;
    Ok(parsed
        .get("OPENAI_BASE_URL")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

pub async fn ensure_codex_endpoint_runtime_home_from_env(
    runtime_root: &Path,
    provider_env: &mut HashMap<String, String>,
) -> Result<()> {
    let api_key = codex_endpoint_api_key_from_provider_env(provider_env).await?;
    let base_url = codex_endpoint_base_url_from_provider_env(provider_env).await?;
    let codex_home = codex_runtime_home(runtime_root);
    ctx_fs::permissions::ensure_private_dir(&codex_home)
        .await
        .context("creating CODEX_HOME for endpoint runtime")?;
    let mut auth = serde_json::Map::new();
    auth.insert(
        "OPENAI_API_KEY".to_string(),
        serde_json::Value::String(api_key.clone()),
    );
    if let Some(base_url) = base_url.clone() {
        auth.insert(
            "OPENAI_BASE_URL".to_string(),
            serde_json::Value::String(base_url),
        );
    }
    let auth_payload = serde_json::to_vec_pretty(&serde_json::Value::Object(auth))
        .context("serializing endpoint auth payload")?;
    write_secure_file_atomic(&codex_home.join("auth.json"), &auth_payload)
        .await
        .context("writing endpoint CODEX_HOME auth.json")?;
    provider_env.insert("OPENAI_API_KEY".to_string(), api_key);
    if let Some(base_url) = base_url {
        provider_env.insert("OPENAI_BASE_URL".to_string(), base_url);
    }
    provider_env.insert(
        "CODEX_HOME".to_string(),
        codex_home.to_string_lossy().to_string(),
    );
    Ok(())
}

pub async fn ensure_provider_runtime_home_env(
    runtime_root: &Path,
    provider_id: &str,
    provider_env: &mut HashMap<String, String>,
) -> Result<()> {
    let preferred_home = provider_env
        .get("CODEX_HOME")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let home = preferred_home.unwrap_or_else(|| {
        runtime_root
            .join("providers")
            .join(provider_id)
            .join("home")
    });
    let config_home = home.join(".config");
    let cache_home = home.join(".cache");
    let data_home = home.join(".local").join("share");
    let state_home = home.join(".local").join("state");
    ctx_fs::permissions::ensure_private_dir(&home)
        .await
        .with_context(|| format!("creating provider runtime home {}", home.display()))?;
    ctx_fs::permissions::ensure_private_dir(&config_home)
        .await
        .with_context(|| {
            format!(
                "creating provider runtime config dir {}",
                config_home.display()
            )
        })?;
    ctx_fs::permissions::ensure_private_dir(&cache_home)
        .await
        .with_context(|| {
            format!(
                "creating provider runtime cache dir {}",
                cache_home.display()
            )
        })?;
    ctx_fs::permissions::ensure_private_dir(&data_home)
        .await
        .with_context(|| format!("creating provider runtime data dir {}", data_home.display()))?;
    ctx_fs::permissions::ensure_private_dir(&state_home)
        .await
        .with_context(|| {
            format!(
                "creating provider runtime state dir {}",
                state_home.display()
            )
        })?;
    provider_env
        .entry("HOME".to_string())
        .or_insert_with(|| home.to_string_lossy().to_string());
    provider_env
        .entry("XDG_CONFIG_HOME".to_string())
        .or_insert_with(|| config_home.to_string_lossy().to_string());
    provider_env
        .entry("XDG_CACHE_HOME".to_string())
        .or_insert_with(|| cache_home.to_string_lossy().to_string());
    provider_env
        .entry("XDG_DATA_HOME".to_string())
        .or_insert_with(|| data_home.to_string_lossy().to_string());
    provider_env
        .entry("XDG_STATE_HOME".to_string())
        .or_insert_with(|| state_home.to_string_lossy().to_string());
    Ok(())
}
