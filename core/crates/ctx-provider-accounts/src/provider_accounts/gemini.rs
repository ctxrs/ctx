use std::path::Path;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod runtime;
mod secrets;

use super::shared::{
    apply_email_update, apply_label_update, collect_secret_paths, ensure_account_exists,
    ensure_safe_account_id, load_json_registry, normalize_optional_email,
    parse_required_json_object, remove_projected_account_home_for_runtime_roots,
    save_json_registry,
};
use super::{
    gemini_account_home, gemini_registry_path, gemini_secret_path,
    GEMINI_AUTH_SELECTED_TYPE_OAUTH_PERSONAL, GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL,
    GEMINI_FORCE_FILE_STORAGE_ENV, GEMINI_SECRET_VERSION,
};
pub(crate) use runtime::gemini_env_for_active_account_with_runtime_root;
pub use runtime::{
    apply_gemini_api_key_runtime_auth_env, apply_gemini_vertex_runtime_auth_env,
    gemini_env_for_account, gemini_env_for_active_account,
};
pub use secrets::write_gemini_auth_settings;
use secrets::{
    ensure_gemini_account_home, read_gemini_secret_for_ref, write_gemini_secret_for_account,
};

const GEMINI_RUNTIME_AUTH_ENV_KEYS: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_PROJECT_ID",
    "GOOGLE_CLOUD_LOCATION",
];

fn default_gemini_credential_kind() -> String {
    GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_gemini_credential_kind")]
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
pub struct GeminiAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<GeminiAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiLoginStatus {
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
pub(crate) struct GeminiSecretEnvelope {
    version: u32,
    oauth_creds: serde_json::Value,
    #[serde(default)]
    google_accounts: Option<serde_json::Value>,
}

pub async fn load_gemini_registry(data_root: &Path) -> Result<GeminiAccountRegistry> {
    load_json_registry(&gemini_registry_path(data_root), "Gemini account registry").await
}

pub async fn save_gemini_registry(
    data_root: &Path,
    registry: &GeminiAccountRegistry,
) -> Result<()> {
    save_json_registry(&gemini_registry_path(data_root), registry).await
}

pub async fn add_gemini_account(
    data_root: &Path,
    label: Option<String>,
    oauth_creds_json: String,
    google_accounts_json: Option<String>,
    email: Option<String>,
) -> Result<GeminiAccountRegistry> {
    let oauth_creds = parse_required_json_object(&oauth_creds_json, "oauth_creds_json")?;
    let mut registry = load_gemini_registry(data_root).await?;
    let mut existing_account_id: Option<String> = None;

    for existing in &registry.accounts {
        let Some(secret_ref) = existing.secret_ref.as_deref() else {
            continue;
        };
        let existing_secret = read_gemini_secret_for_ref(data_root, secret_ref).await?;
        if existing_secret.oauth_creds == oauth_creds {
            existing_account_id = Some(existing.id.clone());
            break;
        }
    }

    if let Some(account_id) = existing_account_id {
        if let Some(google_accounts) = google_accounts_json.as_deref() {
            let _ = write_gemini_secret_for_account(
                data_root,
                &account_id,
                &oauth_creds_json,
                Some(google_accounts),
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
            entry.last_used_at = Some(Utc::now());
        }
        registry.active_account_id = Some(account_id);
        save_gemini_registry(data_root, &registry).await?;
        return Ok(registry);
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let secret_ref = write_gemini_secret_for_account(
        data_root,
        &account_id,
        &oauth_creds_json,
        google_accounts_json.as_deref(),
    )
    .await?;
    let entry = GeminiAccountEntry {
        id: account_id.clone(),
        label: normalize_gemini_label(label, &account_id),
        kind: GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL.to_string(),
        email: normalize_optional_email(email),
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
    };
    registry.accounts.push(entry);
    registry.active_account_id = Some(account_id);
    save_gemini_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn set_active_gemini_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<GeminiAccountRegistry> {
    let mut registry = load_gemini_registry(data_root).await?;
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
    save_gemini_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_gemini_account(
    data_root: &Path,
    account_id: &str,
) -> Result<GeminiAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_gemini_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<GeminiAccountEntry> = registry
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
        gemini_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_gemini_registry(data_root, &registry).await?;

    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }

    let account_home = gemini_account_home(data_root, account_id);
    if account_home.exists() {
        tokio::fs::remove_dir_all(account_home).await?;
    }
    remove_projected_account_home_for_runtime_roots(
        data_root,
        account_id,
        gemini_account_home,
        "gemini",
    )
    .await?;

    Ok(registry)
}

pub fn normalize_gemini_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Gemini Account {account_id}"))
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod gemini_tests;
