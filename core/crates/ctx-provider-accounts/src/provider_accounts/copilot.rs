use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use ctx_fs::permissions::ensure_private_dir;
use serde::{Deserialize, Serialize};

use super::shared::{
    apply_email_update, apply_label_update, collect_secret_paths, ensure_account_exists,
    ensure_safe_account_id, load_json_registry, normalize_optional_email,
    remove_projected_account_home_for_runtime_roots, save_json_registry, write_secure_file_atomic,
};
use super::{
    copilot_account_dir, copilot_registry_path, copilot_secret_path,
    COPILOT_CREDENTIAL_KIND_GH_TOKEN, COPILOT_SECRET_VERSION,
};

pub(crate) const COPILOT_BOOTSTRAP_MODEL_ID: &str = "gpt-5-mini";

const COPILOT_CATALOG_VERSION_1_0_0: &str = "1.0.0";
const COPILOT_CATALOG_VERSION_1_0_3: &str = "1.0.3";
const COPILOT_DEFAULT_MODEL_ID: &str = "claude-sonnet-4.6";

fn default_copilot_credential_kind() -> String {
    COPILOT_CREDENTIAL_KIND_GH_TOKEN.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopilotKnownModel {
    pub(crate) id: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) requires_enablement: bool,
    pub(crate) is_default: bool,
    pub(crate) bootstrap_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopilotModelCatalog {
    pub(crate) version: &'static str,
    pub(crate) default_model_id: &'static str,
    pub(crate) bootstrap_model_id: &'static str,
    pub(crate) models: &'static [CopilotKnownModel],
}

const COPILOT_MODEL_CATALOG_1_0_X: [CopilotKnownModel; 7] = [
    CopilotKnownModel {
        id: "claude-sonnet-4.6",
        display_name: "Claude Sonnet 4.6 (requires enablement)",
        requires_enablement: true,
        is_default: true,
        bootstrap_safe: false,
    },
    CopilotKnownModel {
        id: "claude-haiku-4.5",
        display_name: "Claude Haiku 4.5",
        requires_enablement: false,
        is_default: false,
        bootstrap_safe: true,
    },
    CopilotKnownModel {
        id: "claude-opus-4.6",
        display_name: "Claude Opus 4.6 (requires enablement)",
        requires_enablement: true,
        is_default: false,
        bootstrap_safe: false,
    },
    CopilotKnownModel {
        id: "claude-opus-4.6-fast",
        display_name: "Claude Opus 4.6 (fast mode) (Preview) (requires enablement)",
        requires_enablement: true,
        is_default: false,
        bootstrap_safe: false,
    },
    CopilotKnownModel {
        id: "gpt-5.2-codex",
        display_name: "GPT-5.2-Codex (requires enablement)",
        requires_enablement: true,
        is_default: false,
        bootstrap_safe: false,
    },
    CopilotKnownModel {
        id: "gpt-5-mini",
        display_name: "GPT-5 mini",
        requires_enablement: false,
        is_default: false,
        bootstrap_safe: true,
    },
    CopilotKnownModel {
        id: "gpt-4.1",
        display_name: "GPT-4.1",
        requires_enablement: false,
        is_default: false,
        bootstrap_safe: true,
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_copilot_credential_kind")]
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
pub struct CopilotAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<CopilotAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotSecretEnvelope {
    version: u32,
    gh_token: String,
}

pub async fn load_copilot_registry(data_root: &Path) -> Result<CopilotAccountRegistry> {
    load_json_registry(
        &copilot_registry_path(data_root),
        "Copilot account registry",
    )
    .await
}

pub async fn save_copilot_registry(
    data_root: &Path,
    registry: &CopilotAccountRegistry,
) -> Result<()> {
    save_json_registry(&copilot_registry_path(data_root), registry).await
}

pub async fn ensure_copilot_account_dir(data_root: &Path, account_id: &str) -> Result<PathBuf> {
    let dir = copilot_account_dir(data_root, account_id);
    ensure_private_dir(&dir).await?;
    ensure_private_dir(&dir.join(".config")).await?;
    ensure_private_dir(&dir.join(".cache")).await?;
    ensure_private_dir(&dir.join(".local").join("share")).await?;
    ensure_private_dir(&dir.join(".local").join("state")).await?;
    Ok(dir)
}

pub async fn add_copilot_account(
    data_root: &Path,
    label: Option<String>,
    token: String,
    email: Option<String>,
) -> Result<CopilotAccountRegistry> {
    let token = normalize_copilot_token(&token)?;
    let mut registry = load_copilot_registry(data_root).await?;
    let mut existing_account_id: Option<String> = None;

    for existing in &registry.accounts {
        let Some(secret_ref) = existing.secret_ref.as_deref() else {
            continue;
        };
        let existing_token = read_copilot_secret_for_ref(data_root, secret_ref).await?;
        if existing_token == token {
            existing_account_id = Some(existing.id.clone());
            break;
        }
    }

    if let Some(account_id) = existing_account_id {
        if let Some(entry) = registry
            .accounts
            .iter_mut()
            .find(|entry| entry.id == account_id)
        {
            apply_label_update(label.clone(), &mut entry.label);
            apply_email_update(email.clone(), &mut entry.email);
            entry.last_used_at = Some(Utc::now());
        }
        registry.active_account_id = Some(account_id);
        save_copilot_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let secret_ref = write_copilot_secret_for_account(data_root, &account_id, &token).await?;
    let entry = CopilotAccountEntry {
        id: account_id.clone(),
        label: normalize_copilot_label(label, &account_id),
        kind: COPILOT_CREDENTIAL_KIND_GH_TOKEN.to_string(),
        email: normalize_optional_email(email),
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
    };
    registry.accounts.push(entry);
    registry.active_account_id = Some(account_id);
    save_copilot_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_copilot_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<CopilotAccountRegistry> {
    let mut registry = load_copilot_registry(data_root).await?;
    let previous_active = registry.active_account_id.clone();
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
    save_copilot_registry(data_root, &registry).await?;
    if previous_active.as_deref() != registry.active_account_id.as_deref() {
        if let Some(previous_active) = previous_active.as_deref() {
            remove_projected_account_home_for_runtime_roots(
                data_root,
                previous_active,
                copilot_account_dir,
                "copilot",
            )
            .await?;
        }
    }
    Ok(registry)
}

pub async fn remove_copilot_account(
    data_root: &Path,
    account_id: &str,
) -> Result<CopilotAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_copilot_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<CopilotAccountEntry> = registry
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
        copilot_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_copilot_registry(data_root, &registry).await?;
    remove_projected_account_home_for_runtime_roots(
        data_root,
        account_id,
        copilot_account_dir,
        "copilot",
    )
    .await?;
    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }
    let account_dir = copilot_account_dir(data_root, account_id);
    if account_dir.exists() {
        tokio::fs::remove_dir_all(account_dir).await?;
    }
    Ok(registry)
}

pub fn copilot_env_for_account(
    data_root: &Path,
    account_id: &str,
    token: &str,
) -> HashMap<String, String> {
    let home = copilot_account_dir(data_root, account_id);
    let config = home.join(".config");
    let cache = home.join(".cache");
    let data = home.join(".local").join("share");
    let state = home.join(".local").join("state");
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), home.to_string_lossy().to_string());
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        config.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_CACHE_HOME".to_string(),
        cache.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_DATA_HOME".to_string(),
        data.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_STATE_HOME".to_string(),
        state.to_string_lossy().to_string(),
    );
    env.insert(
        "COPILOT_MODEL".to_string(),
        COPILOT_BOOTSTRAP_MODEL_ID.to_string(),
    );
    env.insert("COPILOT_GITHUB_TOKEN".to_string(), token.to_string());
    env.insert("GH_TOKEN".to_string(), token.to_string());
    env.insert("GITHUB_TOKEN".to_string(), token.to_string());
    env
}

pub async fn copilot_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_copilot_registry(data_root).await?;
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
        bail!("active copilot account has no secret reference");
    };
    let token = read_copilot_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_copilot_account_dir(data_root, active).await?;
    Ok(copilot_env_for_account(data_root, active, &token))
}

pub(crate) async fn copilot_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_copilot_registry(data_root).await?;
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
        bail!("active copilot account has no secret reference");
    };
    let token = read_copilot_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_copilot_account_dir(runtime_root, active).await?;
    Ok(copilot_env_for_account(runtime_root, active, &token))
}

fn normalize_copilot_cli_version(version: &str) -> Option<String> {
    let trimmed = version.trim().trim_start_matches('v').trim_end_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn copilot_model_catalog_for_version(version: &str) -> Option<CopilotModelCatalog> {
    match normalize_copilot_cli_version(version)?.as_str() {
        COPILOT_CATALOG_VERSION_1_0_0 => Some(CopilotModelCatalog {
            version: COPILOT_CATALOG_VERSION_1_0_0,
            default_model_id: COPILOT_DEFAULT_MODEL_ID,
            bootstrap_model_id: COPILOT_BOOTSTRAP_MODEL_ID,
            models: &COPILOT_MODEL_CATALOG_1_0_X,
        }),
        COPILOT_CATALOG_VERSION_1_0_3 => Some(CopilotModelCatalog {
            version: COPILOT_CATALOG_VERSION_1_0_3,
            default_model_id: COPILOT_DEFAULT_MODEL_ID,
            bootstrap_model_id: COPILOT_BOOTSTRAP_MODEL_ID,
            models: &COPILOT_MODEL_CATALOG_1_0_X,
        }),
        _ => None,
    }
}

pub fn copilot_models_value_for_version(version: &str) -> Option<serde_json::Value> {
    let catalog = copilot_model_catalog_for_version(version)?;
    Some(serde_json::json!({
        "catalog_source": "copilot_version_pinned",
        "catalog_version": catalog.version,
        "default_model_id": catalog.default_model_id,
        "current_model_id": catalog.bootstrap_model_id,
        "models": catalog.models.iter().map(|model| serde_json::json!({
            "id": model.id,
            "name": model.display_name,
            "requires_enablement": model.requires_enablement,
            "is_default": model.is_default,
            "bootstrap_safe": model.bootstrap_safe,
        })).collect::<Vec<_>>(),
    }))
}

pub fn normalize_copilot_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Copilot Account {account_id}"))
}

fn normalize_copilot_token(token: &str) -> Result<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        bail!("token is required");
    }
    Ok(trimmed.to_string())
}

async fn write_copilot_secret_for_account(
    data_root: &Path,
    account_id: &str,
    token: &str,
) -> Result<String> {
    let token = normalize_copilot_token(token)?;
    let secret_ref = format!("{account_id}.json");
    let path = copilot_secret_path(data_root, &secret_ref)?;
    let envelope = CopilotSecretEnvelope {
        version: COPILOT_SECRET_VERSION,
        gh_token: token,
    };
    write_secure_file_atomic(&path, &serde_json::to_vec_pretty(&envelope)?).await?;
    Ok(secret_ref)
}

async fn read_copilot_secret_for_ref(data_root: &Path, secret_ref: &str) -> Result<String> {
    let path = copilot_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading copilot secret {}", path.display()))?;
    let parsed: CopilotSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid copilot secret {}", path.display()))?;
    if parsed.version != COPILOT_SECRET_VERSION {
        bail!(
            "unsupported copilot secret version {} at {}",
            parsed.version,
            path.display()
        );
    }
    normalize_copilot_token(&parsed.gh_token)
}

#[cfg(test)]
mod tests;
