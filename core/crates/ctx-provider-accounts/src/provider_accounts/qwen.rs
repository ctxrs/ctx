use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::shared::{
    apply_email_update, apply_label_update, collect_secret_paths, ensure_account_exists,
    ensure_home_config_cache_dirs, ensure_safe_account_id, home_config_cache_env,
    load_json_registry, normalize_optional_email, parse_required_json_object,
    remove_projected_account_home_for_runtime_roots, save_json_registry, write_secure_file_atomic,
};
use super::{
    qwen_account_home, qwen_registry_path, qwen_secret_path, QWEN_AUTH_SELECTED_TYPE_OAUTH,
    QWEN_CREDENTIAL_KIND_OAUTH, QWEN_SECRET_VERSION,
};

fn default_qwen_credential_kind() -> String {
    QWEN_CREDENTIAL_KIND_OAUTH.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QwenAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_qwen_credential_kind")]
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
pub struct QwenAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<QwenAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QwenLoginStatus {
    pub login_id: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    pub status: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QwenSecretEnvelope {
    version: u32,
    oauth_creds: serde_json::Value,
}

pub async fn load_qwen_registry(data_root: &Path) -> Result<QwenAccountRegistry> {
    load_json_registry(&qwen_registry_path(data_root), "Qwen account registry").await
}

pub async fn save_qwen_registry(data_root: &Path, registry: &QwenAccountRegistry) -> Result<()> {
    save_json_registry(&qwen_registry_path(data_root), registry).await
}

pub async fn add_qwen_account(
    data_root: &Path,
    label: Option<String>,
    oauth_creds_json: String,
    email: Option<String>,
) -> Result<QwenAccountRegistry> {
    let oauth_creds = parse_required_json_object(&oauth_creds_json, "oauth_creds_json")?;
    let mut registry = load_qwen_registry(data_root).await?;
    let mut existing_account_id: Option<String> = None;

    for existing in &registry.accounts {
        let Some(secret_ref) = existing.secret_ref.as_deref() else {
            continue;
        };
        let existing_secret = read_qwen_secret_for_ref(data_root, secret_ref).await?;
        if existing_secret.oauth_creds == oauth_creds {
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
        save_qwen_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let secret_ref =
        write_qwen_secret_for_account(data_root, &account_id, &oauth_creds_json).await?;
    let entry = QwenAccountEntry {
        id: account_id.clone(),
        label: normalize_qwen_label(label, &account_id),
        kind: QWEN_CREDENTIAL_KIND_OAUTH.to_string(),
        email: normalize_optional_email(email),
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
    };
    registry.accounts.push(entry);
    registry.active_account_id = Some(account_id);
    save_qwen_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_qwen_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<QwenAccountRegistry> {
    let mut registry = load_qwen_registry(data_root).await?;
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
    save_qwen_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_qwen_account(
    data_root: &Path,
    account_id: &str,
) -> Result<QwenAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_qwen_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<QwenAccountEntry> = registry
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
        qwen_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_qwen_registry(data_root, &registry).await?;

    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }

    let account_home = qwen_account_home(data_root, account_id);
    if account_home.exists() {
        tokio::fs::remove_dir_all(account_home).await?;
    }
    remove_projected_account_home_for_runtime_roots(
        data_root,
        account_id,
        qwen_account_home,
        "qwen",
    )
    .await?;

    Ok(registry)
}

pub fn qwen_env_for_account(data_root: &Path, account_id: &str) -> HashMap<String, String> {
    let home = qwen_account_home(data_root, account_id);
    home_config_cache_env(&home)
}

pub async fn qwen_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_qwen_registry(data_root).await?;
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
        bail!("active qwen account has no secret reference");
    };
    let secret = read_qwen_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_qwen_account_home(data_root, active, &secret).await?;
    Ok(qwen_env_for_account(data_root, active))
}

pub(crate) async fn qwen_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_qwen_registry(data_root).await?;
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
        bail!("active qwen account has no secret reference");
    };
    let secret = read_qwen_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_qwen_account_home(runtime_root, active, &secret).await?;
    Ok(qwen_env_for_account(runtime_root, active))
}

pub fn normalize_qwen_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Qwen Account {account_id}"))
}

async fn write_qwen_secret_for_account(
    data_root: &Path,
    account_id: &str,
    oauth_creds_json: &str,
) -> Result<String> {
    let oauth_creds = parse_required_json_object(oauth_creds_json, "oauth_creds_json")?;
    let secret_ref = format!("{account_id}.json");
    let path = qwen_secret_path(data_root, &secret_ref)?;
    let envelope = QwenSecretEnvelope {
        version: QWEN_SECRET_VERSION,
        oauth_creds,
    };
    write_secure_file_atomic(&path, &serde_json::to_vec_pretty(&envelope)?).await?;
    Ok(secret_ref)
}

async fn read_qwen_secret_for_ref(
    data_root: &Path,
    secret_ref: &str,
) -> Result<QwenSecretEnvelope> {
    let path = qwen_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading qwen secret {}", path.display()))?;
    let parsed: QwenSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid qwen secret {}", path.display()))?;
    if parsed.version != QWEN_SECRET_VERSION {
        bail!(
            "unsupported qwen secret version {} at {}",
            parsed.version,
            path.display()
        );
    }
    if !parsed.oauth_creds.is_object() {
        bail!("qwen oauth_creds must be a JSON object");
    }
    Ok(parsed)
}

async fn ensure_qwen_account_home(
    data_root: &Path,
    account_id: &str,
    secret: &QwenSecretEnvelope,
) -> Result<PathBuf> {
    let home = qwen_account_home(data_root, account_id);
    let qwen_dir = home.join(".qwen");
    ctx_fs::permissions::ensure_private_dir(&home).await?;
    ctx_fs::permissions::ensure_private_dir(&qwen_dir).await?;
    write_secure_file_atomic(
        &qwen_dir.join("oauth_creds.json"),
        &serde_json::to_vec_pretty(&secret.oauth_creds)?,
    )
    .await?;
    let settings = serde_json::json!({
        "$version": 2,
        "security": {
            "auth": {
                "selectedType": QWEN_AUTH_SELECTED_TYPE_OAUTH
            }
        }
    });
    write_secure_file_atomic(
        &qwen_dir.join("settings.json"),
        &serde_json::to_vec_pretty(&settings)?,
    )
    .await?;
    ensure_home_config_cache_dirs(&home).await?;
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_accounts::QWEN_OAUTH_CREDS_RELATIVE_PATH;

    #[tokio::test]
    async fn qwen_active_account_projects_oauth_creds_under_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let oauth_json = r#"{"access_token":"token-a","refresh_token":"token-r","token_type":"Bearer","expiry_date":4102444800000}"#;
        let registry =
            add_qwen_account(root, Some("Qwen".to_string()), oauth_json.to_string(), None)
                .await
                .unwrap();
        let active_id = registry.active_account_id.clone().expect("active account");
        let env = qwen_env_for_active_account(root).await.unwrap();
        let home = PathBuf::from(env.get("HOME").expect("HOME should be set"));
        assert!(home.ends_with(active_id));
        let creds_path = home.join(QWEN_OAUTH_CREDS_RELATIVE_PATH);
        assert!(creds_path.exists());
        let settings_path = home.join(".qwen").join("settings.json");
        let settings = tokio::fs::read_to_string(settings_path).await.unwrap();
        assert!(settings.contains(QWEN_AUTH_SELECTED_TYPE_OAUTH));
    }

    #[tokio::test]
    async fn adding_existing_qwen_account_fails_closed_on_malformed_secret() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let secret_ref = "acct-1.json";
        save_qwen_registry(
            root,
            &QwenAccountRegistry {
                active_account_id: Some("acct-1".to_string()),
                accounts: vec![QwenAccountEntry {
                    id: "acct-1".to_string(),
                    label: "Existing".to_string(),
                    kind: QWEN_CREDENTIAL_KIND_OAUTH.to_string(),
                    email: None,
                    created_at: Utc::now(),
                    last_used_at: Some(Utc::now()),
                    secret_ref: Some(secret_ref.to_string()),
                }],
            },
        )
        .await
        .unwrap();
        let secret_path = qwen_secret_path(root, secret_ref).unwrap();
        tokio::fs::create_dir_all(secret_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&secret_path, "{ invalid json")
            .await
            .unwrap();

        let err = add_qwen_account(
            root,
            Some("Qwen Updated".to_string()),
            r#"{"access_token":"token-a","refresh_token":"token-r","token_type":"Bearer","expiry_date":4102444800000}"#.to_string(),
            None,
        )
        .await
        .expect_err("malformed existing qwen secret should fail closed");
        let message = format!("{err:#}");
        assert!(
            message.contains("invalid qwen secret"),
            "expected parse context in error: {message}"
        );
        assert!(
            message.contains("acct-1.json"),
            "expected secret path in error: {message}"
        );
    }
}
