use std::path::Path;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod runtime;
mod secrets;

use super::shared::{
    apply_email_update, apply_label_update, collect_secret_paths, ensure_account_exists,
    ensure_safe_account_id, load_json_registry, normalize_optional_email,
    remove_projected_account_home_for_runtime_roots, save_json_registry, write_secure_file_atomic,
};
use super::{
    cursor_account_home, cursor_registry_path, cursor_secret_path, CURSOR_CREDENTIAL_KIND_API_KEY,
    CURSOR_CREDENTIAL_KIND_OAUTH_TOKEN, CURSOR_SECRET_VERSION,
};
pub(crate) use runtime::cursor_env_for_active_account_with_runtime_root;
pub use runtime::{
    cursor_env_for_account, cursor_env_for_active_account, ensure_cursor_account_home,
};
use secrets::{
    read_cursor_secret_for_ref, write_cursor_secret_for_account, write_cursor_secret_for_ref,
};

fn default_cursor_credential_kind() -> String {
    CURSOR_CREDENTIAL_KIND_API_KEY.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_cursor_credential_kind")]
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
pub struct CursorAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<CursorAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorSecretEnvelope {
    version: u32,
    #[serde(alias = "api_key")]
    auth_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorLoginStatus {
    pub login_id: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    pub status: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct CursorSecretRecord {
    auth_token: String,
    refresh_token: Option<String>,
}

pub async fn load_cursor_registry(data_root: &Path) -> Result<CursorAccountRegistry> {
    load_json_registry(&cursor_registry_path(data_root), "Cursor account registry").await
}

pub async fn save_cursor_registry(
    data_root: &Path,
    registry: &CursorAccountRegistry,
) -> Result<()> {
    save_json_registry(&cursor_registry_path(data_root), registry).await
}

pub async fn add_cursor_account(
    data_root: &Path,
    label: Option<String>,
    token: String,
    email: Option<String>,
) -> Result<CursorAccountRegistry> {
    upsert_cursor_account_internal(
        data_root,
        label,
        token,
        None,
        email,
        CURSOR_CREDENTIAL_KIND_API_KEY,
    )
    .await
}

pub async fn add_cursor_oauth_account(
    data_root: &Path,
    label: Option<String>,
    auth_token: String,
    refresh_token: Option<String>,
    email: Option<String>,
) -> Result<CursorAccountRegistry> {
    upsert_cursor_account_internal(
        data_root,
        label,
        auth_token,
        refresh_token,
        email,
        CURSOR_CREDENTIAL_KIND_OAUTH_TOKEN,
    )
    .await
}

async fn upsert_cursor_account_internal(
    data_root: &Path,
    label: Option<String>,
    auth_token: String,
    refresh_token: Option<String>,
    email: Option<String>,
    credential_kind: &str,
) -> Result<CursorAccountRegistry> {
    let auth_token = normalize_cursor_auth_token(&auth_token)?;
    let refresh_token = normalize_optional_cursor_auth_token(refresh_token.as_deref())?;
    let mut registry = load_cursor_registry(data_root).await?;
    let mut existing_account: Option<(String, Option<String>, Option<String>)> = None;

    for existing in &registry.accounts {
        let Some(secret_ref) = existing.secret_ref.as_deref() else {
            continue;
        };
        let existing_secret = read_cursor_secret_for_ref(data_root, secret_ref).await?;
        if existing_secret.auth_token == auth_token {
            existing_account = Some((
                existing.id.clone(),
                Some(secret_ref.to_string()),
                existing_secret.refresh_token,
            ));
            break;
        }
    }

    if let Some((account_id, existing_secret_ref, existing_refresh_token)) = existing_account {
        let next_refresh_token = refresh_token.clone().or(existing_refresh_token);
        if let Some(entry) = registry
            .accounts
            .iter_mut()
            .find(|entry| entry.id == account_id)
        {
            if let Some(secret_ref) = existing_secret_ref {
                write_cursor_secret_for_ref(
                    data_root,
                    &secret_ref,
                    &auth_token,
                    next_refresh_token.as_deref(),
                )
                .await?;
            } else {
                entry.secret_ref = Some(
                    write_cursor_secret_for_account(
                        data_root,
                        &account_id,
                        &auth_token,
                        next_refresh_token.as_deref(),
                    )
                    .await?,
                );
            }
            apply_label_update(label.clone(), &mut entry.label);
            apply_email_update(email.clone(), &mut entry.email);
            entry.kind = credential_kind.to_string();
            entry.last_used_at = Some(Utc::now());
        }
        registry.active_account_id = Some(account_id);
        save_cursor_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let secret_ref = write_cursor_secret_for_account(
        data_root,
        &account_id,
        &auth_token,
        refresh_token.as_deref(),
    )
    .await?;
    let entry = CursorAccountEntry {
        id: account_id.clone(),
        label: normalize_cursor_label(label, &account_id),
        kind: credential_kind.to_string(),
        email: normalize_optional_email(email),
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
    };
    registry.accounts.push(entry);
    registry.active_account_id = Some(account_id);
    save_cursor_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_cursor_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<CursorAccountRegistry> {
    let mut registry = load_cursor_registry(data_root).await?;
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
    save_cursor_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_cursor_account(
    data_root: &Path,
    account_id: &str,
) -> Result<CursorAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_cursor_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<CursorAccountEntry> = registry
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
        cursor_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_cursor_registry(data_root, &registry).await?;

    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }

    let account_home = cursor_account_home(data_root, account_id);
    if account_home.exists() {
        tokio::fs::remove_dir_all(account_home).await?;
    }
    remove_projected_account_home_for_runtime_roots(
        data_root,
        account_id,
        cursor_account_home,
        "cursor",
    )
    .await?;
    Ok(registry)
}

pub fn normalize_cursor_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Cursor Account {account_id}"))
}

fn normalize_cursor_auth_token(token: &str) -> Result<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        bail!("auth token is required");
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_cursor_auth_token(token: Option<&str>) -> Result<Option<String>> {
    match token {
        Some(value) => normalize_cursor_auth_token(value).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
