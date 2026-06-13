use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::shared::{
    apply_label_update, ensure_account_exists, ensure_home_config_cache_dirs,
    ensure_safe_account_id, home_config_cache_env, load_json_registry, normalize_optional_email,
    save_json_registry, write_secure_file_atomic,
};
use super::{amp_registry_path, amp_runtime_home, AMP_CREDENTIAL_KIND_BROWSER_OAUTH};

fn default_amp_credential_kind() -> String {
    AMP_CREDENTIAL_KIND_BROWSER_OAUTH.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmpAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_amp_credential_kind")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AmpAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<AmpAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmpLoginStatus {
    pub login_id: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn save_amp_registry(data_root: &Path, registry: &AmpAccountRegistry) -> Result<()> {
    save_json_registry(&amp_registry_path(data_root), registry).await
}

pub async fn ensure_amp_runtime_home(data_root: &Path) -> Result<PathBuf> {
    let home = amp_runtime_home(data_root);
    ensure_home_config_cache_dirs(&home).await?;
    Ok(home)
}

pub async fn clear_amp_runtime_home(data_root: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(amp_runtime_home(data_root)).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("removing amp runtime home"),
    }
}

pub async fn load_amp_registry(data_root: &Path) -> Result<AmpAccountRegistry> {
    load_json_registry(&amp_registry_path(data_root), "Amp account registry").await
}

pub async fn upsert_amp_account(
    data_root: &Path,
    label: Option<String>,
    email: Option<String>,
) -> Result<AmpAccountRegistry> {
    let mut registry = load_amp_registry(data_root).await?;
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
        let _ = ensure_amp_runtime_home(data_root).await?;
        save_amp_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    registry.accounts.push(AmpAccountEntry {
        id: account_id.clone(),
        label: normalize_amp_label(label, &account_id),
        kind: AMP_CREDENTIAL_KIND_BROWSER_OAUTH.to_string(),
        email: normalized_email,
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
    });
    registry.active_account_id = Some(account_id);
    let _ = ensure_amp_runtime_home(data_root).await?;
    save_amp_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_amp_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<AmpAccountRegistry> {
    let mut registry = load_amp_registry(data_root).await?;
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
        let _ = ensure_amp_runtime_home(data_root).await?;
    } else {
        clear_amp_runtime_home(data_root).await?;
    }
    save_amp_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_amp_account(data_root: &Path, account_id: &str) -> Result<AmpAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_amp_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed = registry.accounts.iter().any(|entry| entry.id == account_id);
    ensure_account_exists(removed)?;
    registry.accounts.retain(|entry| entry.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_amp_registry(data_root, &registry).await?;
    if was_active {
        clear_amp_runtime_home(data_root).await?;
    }
    Ok(registry)
}

pub async fn amp_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = ensure_amp_registry_from_runtime_auth(data_root).await?;
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
    let home = ensure_amp_runtime_home(data_root).await?;
    Ok(home_config_cache_env(&home))
}

pub(crate) async fn amp_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = ensure_amp_registry_from_runtime_auth(data_root).await?;
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
    let home = ensure_amp_runtime_home(runtime_root).await?;
    sync_amp_runtime_auth_to_runtime_root(data_root, runtime_root).await?;
    Ok(home_config_cache_env(&home))
}

pub fn normalize_amp_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Amp Account {account_id}"))
}

pub async fn ensure_amp_registry_from_runtime_auth(data_root: &Path) -> Result<AmpAccountRegistry> {
    let registry = load_amp_registry(data_root).await?;
    if !registry.accounts.is_empty() && registry.active_account_id.is_some() {
        return Ok(registry);
    }
    if !amp_home_has_persisted_auth(data_root).await {
        return Ok(registry);
    }
    upsert_amp_account(data_root, Some("Amp Imported Session".to_string()), None).await
}

pub(crate) fn amp_secrets_path(data_root: &Path) -> PathBuf {
    amp_runtime_home(data_root)
        .join(".local")
        .join("share")
        .join("amp")
        .join("secrets.json")
}

async fn amp_home_has_persisted_auth(data_root: &Path) -> bool {
    let path = amp_secrets_path(data_root);
    let Ok(raw) = tokio::fs::read_to_string(path).await else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(map) = value.as_object() else {
        return false;
    };
    map.iter()
        .any(|(key, token)| key.starts_with("apiKey@") && !token.is_null())
}

async fn sync_amp_runtime_auth_to_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<()> {
    let source = amp_secrets_path(data_root);
    let target = amp_secrets_path(runtime_root);
    match tokio::fs::read(&source).await {
        Ok(bytes) if !bytes.is_empty() => write_secure_file_atomic(&target, &bytes)
            .await
            .with_context(|| format!("writing projected amp auth to {}", target.display()))?,
        Ok(_) => match tokio::fs::remove_file(&target).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("removing projected amp auth at {}", target.display())
                });
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::remove_file(&target).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("removing projected amp auth at {}", target.display())
                    });
                }
            }
        }
        Err(err) => {
            return Err(err).with_context(|| format!("reading amp auth from {}", source.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn amp_active_account_projects_home_env() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let registry = upsert_amp_account(
            root,
            Some("Amp Test".to_string()),
            Some("amp@example.com".to_string()),
        )
        .await
        .unwrap();
        let active_id = registry.active_account_id.clone().expect("active account");
        assert_eq!(registry.accounts.len(), 1);
        assert_eq!(registry.accounts[0].id, active_id);

        let env = amp_env_for_active_account(root).await.unwrap();
        let home = PathBuf::from(env.get("HOME").expect("HOME should be set"));
        assert!(home.starts_with(root));
        assert!(home.join(".config").exists());
        assert!(home.join(".cache").exists());
    }

    #[tokio::test]
    async fn deleting_active_amp_account_clears_projection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let registry = upsert_amp_account(
            root,
            Some("Amp Test".to_string()),
            Some("amp@example.com".to_string()),
        )
        .await
        .unwrap();
        let active_id = registry.active_account_id.clone().expect("active account");
        let _ = remove_amp_account(root, &active_id).await.unwrap();

        let env = amp_env_for_active_account(root).await.unwrap();
        assert!(env.is_empty());
        assert!(!amp_runtime_home(root).exists());
    }

    #[tokio::test]
    async fn amp_upsert_does_not_persist_registry_when_runtime_home_setup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let home = amp_runtime_home(root);
        tokio::fs::create_dir_all(home.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&home, b"occupied").await.unwrap();

        let err = upsert_amp_account(
            root,
            Some("Amp Test".to_string()),
            Some("amp@example.com".to_string()),
        )
        .await
        .unwrap_err();

        assert!(!err.to_string().is_empty());
        let registry = load_amp_registry(root).await.unwrap();
        assert!(registry.accounts.is_empty());
        assert!(registry.active_account_id.is_none());
    }

    #[tokio::test]
    async fn amp_registry_bootstraps_from_persisted_runtime_auth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let secrets_path = amp_runtime_home(root)
            .join(".local")
            .join("share")
            .join("amp")
            .join("secrets.json");
        tokio::fs::create_dir_all(secrets_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &secrets_path,
            br#"{"apiKey@https://ampcode.com/":"token-value"}"#,
        )
        .await
        .unwrap();

        let registry = ensure_amp_registry_from_runtime_auth(root).await.unwrap();
        assert_eq!(registry.accounts.len(), 1);
        assert!(registry.active_account_id.is_some());
        assert_eq!(registry.accounts[0].label, "Amp Imported Session");
    }

    #[tokio::test]
    async fn amp_runtime_root_projects_persisted_auth_and_bootstraps_registry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runtime_root = root
            .join("containers")
            .join("workspaces")
            .join("workspace-amp")
            .join("data");
        let source_secret = amp_secrets_path(root);
        tokio::fs::create_dir_all(source_secret.parent().unwrap())
            .await
            .unwrap();
        let source_payload = br#"{"apiKey@https://ampcode.com/":"runtime-token"}"#;
        tokio::fs::write(&source_secret, source_payload)
            .await
            .unwrap();

        let env = amp_env_for_active_account_with_runtime_root(root, &runtime_root)
            .await
            .unwrap();
        let home = PathBuf::from(env.get("HOME").expect("HOME should be set"));
        assert!(home.starts_with(&runtime_root));

        let registry = load_amp_registry(root).await.unwrap();
        assert_eq!(registry.accounts.len(), 1);
        assert!(registry.active_account_id.is_some());

        let projected_secret = amp_secrets_path(&runtime_root);
        let projected_payload = tokio::fs::read(&projected_secret).await.unwrap();
        assert_eq!(projected_payload, source_payload);
    }
}
