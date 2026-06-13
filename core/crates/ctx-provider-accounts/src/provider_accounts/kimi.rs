use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::shared::{
    apply_email_update, apply_label_update, collect_secret_paths, ensure_account_exists,
    ensure_safe_account_id, load_json_registry, normalize_optional_email,
    parse_required_json_object, remove_projected_account_home_for_runtime_roots,
    save_json_registry, write_secure_file_atomic,
};
use super::{
    kimi_account_home, kimi_registry_path, kimi_secret_path, KIMI_CREDENTIAL_KIND_CREDENTIALS_JSON,
    KIMI_CREDENTIAL_KIND_OAUTH, KIMI_SECRET_VERSION, KIMI_SHARE_DIR_ENV,
};

const KIMI_CANONICAL_PROVIDER: &str = "kimi-code";

fn default_kimi_import_credential_kind() -> String {
    KIMI_CREDENTIAL_KIND_CREDENTIALS_JSON.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KimiAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_kimi_import_credential_kind")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KimiAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<KimiAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KimiLoginStatus {
    pub login_id: String,
    pub status: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub device_code: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct KimiSecretEnvelope {
    version: u32,
    provider: String,
    credentials: serde_json::Value,
    #[serde(default)]
    config_toml: Option<String>,
}

pub async fn load_kimi_registry(data_root: &Path) -> Result<KimiAccountRegistry> {
    load_json_registry(&kimi_registry_path(data_root), "Kimi account registry").await
}

pub async fn save_kimi_registry(data_root: &Path, registry: &KimiAccountRegistry) -> Result<()> {
    save_json_registry(&kimi_registry_path(data_root), registry).await
}

pub async fn add_kimi_account(
    data_root: &Path,
    label: Option<String>,
    provider: Option<String>,
    credentials_json: String,
    config_toml: Option<String>,
    email: Option<String>,
) -> Result<KimiAccountRegistry> {
    upsert_kimi_account(
        data_root,
        label,
        provider,
        credentials_json,
        config_toml,
        email,
        KIMI_CREDENTIAL_KIND_CREDENTIALS_JSON,
    )
    .await
}

pub async fn add_kimi_oauth_account(
    data_root: &Path,
    label: Option<String>,
    credentials_json: String,
    email: Option<String>,
) -> Result<KimiAccountRegistry> {
    upsert_kimi_account(
        data_root,
        label,
        Some(KIMI_CANONICAL_PROVIDER.to_string()),
        credentials_json,
        None,
        email,
        KIMI_CREDENTIAL_KIND_OAUTH,
    )
    .await
}

async fn upsert_kimi_account(
    data_root: &Path,
    label: Option<String>,
    provider: Option<String>,
    credentials_json: String,
    config_toml: Option<String>,
    email: Option<String>,
    kind: &str,
) -> Result<KimiAccountRegistry> {
    let normalized_provider = normalize_kimi_provider(provider)?;
    let credentials = parse_required_json_object(&credentials_json, "credentials_json")?;
    let mut registry = load_kimi_registry(data_root).await?;
    let mut existing_account_id: Option<String> = None;

    for existing in &registry.accounts {
        let Some(secret_ref) = existing.secret_ref.as_deref() else {
            continue;
        };
        let existing_secret = read_kimi_secret_for_ref(data_root, secret_ref).await?;
        if existing_secret.provider == normalized_provider
            && existing_secret.credentials == credentials
        {
            existing_account_id = Some(existing.id.clone());
            break;
        }
    }

    if let Some(account_id) = existing_account_id {
        if let Some(config_toml_value) = config_toml.clone() {
            let _ = write_kimi_secret_for_account(
                data_root,
                &account_id,
                &normalized_provider,
                &credentials_json,
                Some(config_toml_value),
            )
            .await?;
        }
        if let Some(entry) = registry
            .accounts
            .iter_mut()
            .find(|entry| entry.id == account_id)
        {
            apply_label_update(label.clone(), &mut entry.label);
            apply_email_update(email.clone(), &mut entry.email);
            entry.kind = kind.to_string();
            entry.last_used_at = Some(Utc::now());
        }
        registry.active_account_id = Some(account_id);
        save_kimi_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let secret_ref = write_kimi_secret_for_account(
        data_root,
        &account_id,
        &normalized_provider,
        &credentials_json,
        config_toml,
    )
    .await?;
    let entry = KimiAccountEntry {
        id: account_id.clone(),
        label: normalize_kimi_label(label, &account_id),
        kind: kind.to_string(),
        email: normalize_optional_email(email),
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
    };
    registry.accounts.push(entry);
    registry.active_account_id = Some(account_id);
    save_kimi_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_kimi_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<KimiAccountRegistry> {
    let mut registry = load_kimi_registry(data_root).await?;
    if let Some(active_id) = account_id.as_deref() {
        let Some(entry) = registry.accounts.iter().find(|a| a.id == active_id) else {
            bail!("unknown account");
        };
        if entry.secret_ref.as_deref().is_none() {
            bail!("active account has no secret");
        }
    }
    registry.active_account_id = account_id.clone();
    if let Some(active_id) = account_id {
        let now = Utc::now();
        if let Some(entry) = registry.accounts.iter_mut().find(|a| a.id == active_id) {
            entry.last_used_at = Some(now);
        }
    }
    save_kimi_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_kimi_account(
    data_root: &Path,
    account_id: &str,
) -> Result<KimiAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_kimi_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<KimiAccountEntry> = registry
        .accounts
        .iter()
        .filter(|a| a.id == account_id)
        .cloned()
        .collect();
    ensure_account_exists(!removed.is_empty())?;
    let secret_paths = collect_secret_paths(
        data_root,
        removed
            .iter()
            .filter_map(|entry| entry.secret_ref.as_deref()),
        kimi_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_kimi_registry(data_root, &registry).await?;

    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }

    let account_home = kimi_account_home(data_root, account_id);
    if account_home.exists() {
        tokio::fs::remove_dir_all(account_home).await?;
    }
    remove_projected_account_home_for_runtime_roots(
        data_root,
        account_id,
        kimi_account_home,
        "kimi",
    )
    .await?;
    Ok(registry)
}

pub fn kimi_env_for_account(data_root: &Path, account_id: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        KIMI_SHARE_DIR_ENV.to_string(),
        kimi_account_home(data_root, account_id)
            .join(".kimi")
            .to_string_lossy()
            .to_string(),
    );
    env
}

pub async fn kimi_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_kimi_registry(data_root).await?;
    let Some(active) = registry
        .active_account_id
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return Ok(HashMap::new());
    };
    let Some(entry) = registry.accounts.iter().find(|a| a.id == active) else {
        return Ok(HashMap::new());
    };
    let Some(secret_ref) = entry.secret_ref.as_deref() else {
        bail!("active kimi account has no secret reference");
    };
    let secret = read_kimi_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_kimi_account_home(data_root, active, &secret).await?;
    Ok(kimi_env_for_account(data_root, active))
}

pub(crate) async fn kimi_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_kimi_registry(data_root).await?;
    let Some(active) = registry
        .active_account_id
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return Ok(HashMap::new());
    };
    let Some(entry) = registry.accounts.iter().find(|a| a.id == active) else {
        return Ok(HashMap::new());
    };
    let Some(secret_ref) = entry.secret_ref.as_deref() else {
        bail!("active kimi account has no secret reference");
    };
    let secret = read_kimi_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_kimi_account_home(runtime_root, active, &secret).await?;
    Ok(kimi_env_for_account(runtime_root, active))
}

pub fn normalize_kimi_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Kimi Account {account_id}"))
}

fn default_kimi_provider() -> String {
    KIMI_CANONICAL_PROVIDER.to_string()
}

fn normalize_kimi_provider(provider: Option<String>) -> Result<String> {
    let provider = provider
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(default_kimi_provider);
    if provider
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Ok(provider);
    }
    bail!("provider must contain only [A-Za-z0-9_-]");
}

fn normalize_optional_multiline(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_kimi_config_toml() -> String {
    r#"default_model = "kimi-code/kimi-for-coding"
default_thinking = true
default_yolo = false

[models."kimi-code/kimi-for-coding"]
provider = "managed:kimi-code"
model = "kimi-for-coding"
max_context_size = 262144
capabilities = ["image_in", "video_in", "thinking"]

[providers."managed:kimi-code"]
type = "kimi"
base_url = "https://api.kimi.com/coding/v1"
api_key = ""

[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code"

[loop_control]
max_steps_per_turn = 100
max_retries_per_step = 3
max_ralph_iterations = 0
reserved_context_size = 50000

[services.moonshot_search]
base_url = "https://api.kimi.com/coding/v1/search"
api_key = ""

[services.moonshot_search.oauth]
storage = "file"
key = "oauth/kimi-code"

[services.moonshot_fetch]
base_url = "https://api.kimi.com/coding/v1/fetch"
api_key = ""

[services.moonshot_fetch.oauth]
storage = "file"
key = "oauth/kimi-code"

[mcp.client]
tool_call_timeout_ms = 60000
"#
    .to_string()
}

fn projected_kimi_credential_stems(provider: &str, config_toml: Option<&str>) -> Vec<String> {
    if config_toml.is_some() {
        return vec![provider.to_string()];
    }

    let mut stems = vec![KIMI_CANONICAL_PROVIDER.to_string()];
    if provider != KIMI_CANONICAL_PROVIDER {
        stems.push(provider.to_string());
    }
    stems
}

async fn write_kimi_secret_for_account(
    data_root: &Path,
    account_id: &str,
    provider: &str,
    credentials_json: &str,
    config_toml: Option<String>,
) -> Result<String> {
    let credentials = parse_required_json_object(credentials_json, "credentials_json")?;
    let provider = normalize_kimi_provider(Some(provider.to_string()))?;
    let secret_ref = format!("{account_id}.json");
    let path = kimi_secret_path(data_root, &secret_ref)?;
    let envelope = KimiSecretEnvelope {
        version: KIMI_SECRET_VERSION,
        provider,
        credentials,
        config_toml: normalize_optional_multiline(config_toml),
    };
    write_secure_file_atomic(&path, &serde_json::to_vec_pretty(&envelope)?).await?;
    Ok(secret_ref)
}

pub(crate) async fn read_kimi_secret_for_ref(
    data_root: &Path,
    secret_ref: &str,
) -> Result<KimiSecretEnvelope> {
    let path = kimi_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading kimi secret {}", path.display()))?;
    let parsed: KimiSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid kimi secret {}", path.display()))?;
    if parsed.version != KIMI_SECRET_VERSION {
        bail!(
            "unsupported kimi secret version {} at {}",
            parsed.version,
            path.display()
        );
    }
    if !parsed.credentials.is_object() {
        bail!("kimi credentials must be a JSON object");
    }
    let _ = normalize_kimi_provider(Some(parsed.provider.clone()))?;
    Ok(parsed)
}

async fn ensure_kimi_account_home(
    data_root: &Path,
    account_id: &str,
    secret: &KimiSecretEnvelope,
) -> Result<PathBuf> {
    let home = kimi_account_home(data_root, account_id);
    let share_dir = home.join(".kimi");
    let credentials_dir = share_dir.join("credentials");
    ctx_fs::permissions::ensure_private_dir(&home).await?;
    ctx_fs::permissions::ensure_private_dir(&share_dir).await?;
    ctx_fs::permissions::ensure_private_dir(&credentials_dir).await?;
    let provider = normalize_kimi_provider(Some(secret.provider.clone()))?;
    let credentials_payload = serde_json::to_vec_pretty(&secret.credentials)?;
    for stem in projected_kimi_credential_stems(&provider, secret.config_toml.as_deref()) {
        let credentials_path = credentials_dir.join(format!("{stem}.json"));
        write_secure_file_atomic(&credentials_path, &credentials_payload).await?;
    }
    let config_toml = secret
        .config_toml
        .clone()
        .unwrap_or_else(default_kimi_config_toml);
    write_secure_file_atomic(&share_dir.join("config.toml"), config_toml.as_bytes()).await?;
    Ok(share_dir)
}

#[cfg(test)]
mod tests;
