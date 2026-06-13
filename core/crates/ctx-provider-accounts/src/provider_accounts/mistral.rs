use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::shared::{
    apply_label_update, ensure_account_exists, ensure_home_config_cache_dirs,
    ensure_safe_account_id, home_config_cache_env, load_json_registry, normalize_optional_email,
    save_json_registry,
};
use super::{mistral_registry_path, mistral_runtime_home, MISTRAL_CREDENTIAL_KIND_BROWSER_OAUTH};

fn default_mistral_credential_kind() -> String {
    MISTRAL_CREDENTIAL_KIND_BROWSER_OAUTH.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistralAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_mistral_credential_kind")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MistralAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<MistralAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistralLoginStatus {
    pub login_id: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn save_mistral_registry(
    data_root: &Path,
    registry: &MistralAccountRegistry,
) -> Result<()> {
    save_json_registry(&mistral_registry_path(data_root), registry).await
}

pub async fn ensure_mistral_runtime_home(data_root: &Path) -> Result<PathBuf> {
    let home = mistral_runtime_home(data_root);
    ensure_home_config_cache_dirs(&home).await?;
    Ok(home)
}

pub async fn clear_mistral_runtime_home(data_root: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(mistral_runtime_home(data_root)).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("removing mistral runtime home"),
    }
}

pub async fn load_mistral_registry(data_root: &Path) -> Result<MistralAccountRegistry> {
    load_json_registry(
        &mistral_registry_path(data_root),
        "Mistral account registry",
    )
    .await
}

pub async fn upsert_mistral_account(
    data_root: &Path,
    label: Option<String>,
    email: Option<String>,
) -> Result<MistralAccountRegistry> {
    let mut registry = load_mistral_registry(data_root).await?;
    let normalized_email = normalize_optional_email(email);
    let existing_id = normalized_email
        .as_deref()
        .and_then(|target| {
            registry
                .accounts
                .iter()
                .find(|entry| entry.email.as_deref() == Some(target))
                .map(|entry| entry.id.clone())
        })
        .or_else(|| {
            registry
                .active_account_id
                .clone()
                .filter(|id| registry.accounts.iter().any(|entry| entry.id == *id))
        });

    if let Some(existing_id) = existing_id {
        if let Some(entry) = registry
            .accounts
            .iter_mut()
            .find(|entry| entry.id == existing_id)
        {
            apply_label_update(label, &mut entry.label);
            if normalized_email.is_some() {
                entry.email = normalized_email.clone();
            }
            entry.last_used_at = Some(Utc::now());
        }
        registry.active_account_id = Some(existing_id);
        let _ = ensure_mistral_runtime_home(data_root).await?;
        save_mistral_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    registry.accounts.push(MistralAccountEntry {
        id: account_id.clone(),
        label: normalize_mistral_label(label, &account_id),
        kind: MISTRAL_CREDENTIAL_KIND_BROWSER_OAUTH.to_string(),
        email: normalized_email,
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
    });
    registry.active_account_id = Some(account_id);
    let _ = ensure_mistral_runtime_home(data_root).await?;
    save_mistral_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_mistral_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<MistralAccountRegistry> {
    let mut registry = load_mistral_registry(data_root).await?;
    if let Some(active_id) = account_id.as_deref() {
        if !registry.accounts.iter().any(|entry| entry.id == active_id) {
            anyhow::bail!("unknown account");
        }
    }
    registry.active_account_id = account_id.clone();
    if let Some(active_id) = account_id {
        if let Some(entry) = registry
            .accounts
            .iter_mut()
            .find(|entry| entry.id == active_id)
        {
            entry.last_used_at = Some(Utc::now());
        }
        let _ = ensure_mistral_runtime_home(data_root).await?;
    } else {
        clear_mistral_runtime_home(data_root).await?;
    }
    save_mistral_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_mistral_account(
    data_root: &Path,
    account_id: &str,
) -> Result<MistralAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_mistral_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed = registry.accounts.iter().any(|entry| entry.id == account_id);
    ensure_account_exists(removed)?;
    registry.accounts.retain(|entry| entry.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_mistral_registry(data_root, &registry).await?;
    if was_active {
        clear_mistral_runtime_home(data_root).await?;
    }
    Ok(registry)
}

pub async fn mistral_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_mistral_registry(data_root).await?;
    let Some(active) = registry
        .active_account_id
        .as_deref()
        .map(|raw| raw.trim())
        .filter(|raw| !raw.is_empty())
    else {
        return Ok(HashMap::new());
    };
    if !registry.accounts.iter().any(|entry| entry.id == active) {
        return Ok(HashMap::new());
    }
    let home = ensure_mistral_runtime_home(data_root).await?;
    Ok(home_config_cache_env(&home))
}

pub(crate) async fn mistral_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_mistral_registry(data_root).await?;
    let Some(active) = registry
        .active_account_id
        .as_deref()
        .map(|raw| raw.trim())
        .filter(|raw| !raw.is_empty())
    else {
        return Ok(HashMap::new());
    };
    if !registry.accounts.iter().any(|entry| entry.id == active) {
        return Ok(HashMap::new());
    }
    let home = ensure_mistral_runtime_home(runtime_root).await?;
    Ok(home_config_cache_env(&home))
}

pub fn normalize_mistral_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Mistral Account {account_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mistral_active_account_projects_runtime_home_env() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let registry = upsert_mistral_account(
            root,
            Some("Mistral".to_string()),
            Some("mistral@example.com".to_string()),
        )
        .await
        .unwrap();
        assert!(registry.active_account_id.is_some());
        let env = mistral_env_for_active_account(root).await.unwrap();
        let home = PathBuf::from(env.get("HOME").expect("HOME should be set"));
        assert_eq!(home, mistral_runtime_home(root));
        assert!(home.join(".config").exists());
        assert!(home.join(".cache").exists());
    }

    #[tokio::test]
    async fn mistral_upsert_does_not_persist_registry_when_runtime_home_setup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let home = mistral_runtime_home(root);
        tokio::fs::create_dir_all(home.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&home, b"occupied").await.unwrap();

        let err = upsert_mistral_account(
            root,
            Some("Mistral".to_string()),
            Some("mistral@example.com".to_string()),
        )
        .await
        .unwrap_err();

        assert!(!err.to_string().is_empty());
        let registry = load_mistral_registry(root).await.unwrap();
        assert!(registry.accounts.is_empty());
        assert!(registry.active_account_id.is_none());
    }
}
