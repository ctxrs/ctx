use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use serde::{Deserialize, Serialize};

mod amp;
mod bootstrap_models;
mod claude;
mod codex_auth;
mod copilot;
mod cursor;
mod gemini;
mod kimi;
mod mistral;
mod paths;
mod qwen;
mod runtime_env;
mod shared;
#[cfg(test)]
mod tests;

use self::amp::amp_env_for_active_account_with_runtime_root;
use self::claude::claude_env_for_active_account_with_runtime_root;
#[cfg(test)]
use self::codex_auth::write_runtime_owner_marker;
use self::codex_auth::{
    clear_legacy_runtime_auth_projection_for_runtime_roots, clear_runtime_auth_projection,
    clear_runtime_auth_projection_for_runtime_roots, normalize_endpoint_profile,
};
use self::copilot::copilot_env_for_active_account_with_runtime_root;
use self::cursor::cursor_env_for_active_account_with_runtime_root;
use self::gemini::gemini_env_for_active_account_with_runtime_root;
use self::kimi::kimi_env_for_active_account_with_runtime_root;
use self::mistral::mistral_env_for_active_account_with_runtime_root;
use self::paths::{
    claude_secret_path, codex_runtime_owner_path, codex_secret_path, copilot_secret_path,
    cursor_secret_path, gemini_secret_path, kimi_secret_path, qwen_secret_path,
};
use self::qwen::qwen_env_for_active_account_with_runtime_root;
use self::shared::{
    collect_secret_paths, ensure_account_exists, ensure_safe_account_id, load_json_registry,
    save_json_registry, write_secure_file_atomic,
};

pub use self::amp::{
    amp_env_for_active_account, clear_amp_runtime_home, ensure_amp_registry_from_runtime_auth,
    ensure_amp_runtime_home, load_amp_registry, normalize_amp_label, remove_amp_account,
    save_amp_registry, set_active_amp_account, upsert_amp_account, AmpAccountEntry,
    AmpAccountRegistry, AmpLoginStatus,
};
pub use self::bootstrap_models::pinned_subscription_models_value;
pub use self::claude::{
    add_claude_account, claude_env_for_account, claude_env_for_active_account,
    ensure_claude_account_dir, load_claude_registry, normalize_claude_label, remove_claude_account,
    save_claude_registry, set_active_claude_account, ClaudeAccountEntry, ClaudeAccountRegistry,
    ClaudeLoginStatus,
};
pub use self::codex_auth::{
    acquire_codex_runtime_continuity_lock_from_env, codex_env_for_active_account,
    codex_env_for_active_account_with_runtime_root, codex_env_for_runtime_home,
    codex_has_active_auth, codex_has_active_auth_with_runtime_root, codex_usage_env_for_account,
    codex_usage_env_for_active_account, ensure_codex_auth_ready,
    ensure_codex_endpoint_profile_compatible, host_codex_auth_path,
    hydrate_codex_account_home_from_secret, import_codex_auth_value_to_secret_store,
    import_host_codex_auth_to_secret_store, ingest_codex_account_auth_to_secret_store,
    probe_host_codex_auth_candidate, remove_codex_account_home_auth_if_present,
    seed_codex_auth_from_host, seeding_codex_auth_from_host_enabled,
};
#[cfg(test)]
pub(crate) use self::codex_auth::{
    expose_legacy_codex_state_from_home, expose_legacy_codex_state_to_broker_home,
};
pub use self::copilot::copilot_models_value_for_version;
pub use self::copilot::{
    add_copilot_account, copilot_env_for_account, copilot_env_for_active_account,
    ensure_copilot_account_dir, load_copilot_registry, normalize_copilot_label,
    remove_copilot_account, save_copilot_registry, set_active_copilot_account, CopilotAccountEntry,
    CopilotAccountRegistry,
};
pub use self::cursor::{
    add_cursor_account, add_cursor_oauth_account, cursor_env_for_account,
    cursor_env_for_active_account, ensure_cursor_account_home, load_cursor_registry,
    normalize_cursor_label, remove_cursor_account, save_cursor_registry, set_active_cursor_account,
    CursorAccountEntry, CursorAccountRegistry, CursorLoginStatus,
};
pub use self::gemini::{
    add_gemini_account, gemini_env_for_account, gemini_env_for_active_account,
    load_gemini_registry, normalize_gemini_label, remove_gemini_account, save_gemini_registry,
    set_active_gemini_account, GeminiAccountEntry, GeminiAccountRegistry, GeminiLoginStatus,
};
pub use self::gemini::{
    apply_gemini_api_key_runtime_auth_env, apply_gemini_vertex_runtime_auth_env,
    write_gemini_auth_settings,
};
pub use self::kimi::{
    add_kimi_account, add_kimi_oauth_account, kimi_env_for_account, kimi_env_for_active_account,
    load_kimi_registry, normalize_kimi_label, remove_kimi_account, save_kimi_registry,
    set_active_kimi_account, KimiAccountEntry, KimiAccountRegistry, KimiLoginStatus,
};
pub use self::mistral::{
    clear_mistral_runtime_home, ensure_mistral_runtime_home, load_mistral_registry,
    mistral_env_for_active_account, normalize_mistral_label, remove_mistral_account,
    save_mistral_registry, set_active_mistral_account, upsert_mistral_account, MistralAccountEntry,
    MistralAccountRegistry, MistralLoginStatus,
};
pub use self::paths::*;
pub use self::qwen::{
    add_qwen_account, load_qwen_registry, normalize_qwen_label, qwen_env_for_account,
    qwen_env_for_active_account, remove_qwen_account, save_qwen_registry, set_active_qwen_account,
    QwenAccountEntry, QwenAccountRegistry, QwenLoginStatus,
};
pub use self::runtime_env::{
    ensure_codex_endpoint_runtime_home_from_env, ensure_provider_runtime_home_env,
};

const CTX_SEED_CODEX_AUTH_FROM_HOST_ENV: &str = "CTX_SEED_CODEX_AUTH_FROM_HOST";
const CTX_CODEX_HOST_AUTH_PATH_ENV: &str = "CTX_CODEX_HOST_AUTH_PATH";
const CODEX_SECRET_VERSION: u32 = 1;
const CLAUDE_SECRET_VERSION: u32 = 1;
const GEMINI_SECRET_VERSION: u32 = 1;
const QWEN_SECRET_VERSION: u32 = 1;
const KIMI_SECRET_VERSION: u32 = 1;
const COPILOT_SECRET_VERSION: u32 = 1;
const CURSOR_SECRET_VERSION: u32 = 1;
const CODEX_RUNTIME_OWNER_FILE: &str = ".ctx-active-account-id";
pub const CODEX_CREDENTIAL_KIND_OAUTH: &str = "oauth";
pub const CODEX_CREDENTIAL_KIND_API_KEY: &str = "api_key";
pub const CLAUDE_CREDENTIAL_KIND_SETUP_TOKEN: &str = "setup_token";
pub const GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL: &str = "oauth-personal";
pub const QWEN_CREDENTIAL_KIND_OAUTH: &str = "oauth";
pub const KIMI_CREDENTIAL_KIND_CREDENTIALS_JSON: &str = "credentials-json";
pub const KIMI_CREDENTIAL_KIND_OAUTH: &str = "oauth";
pub const MISTRAL_CREDENTIAL_KIND_BROWSER_OAUTH: &str = "browser-oauth";
pub const COPILOT_CREDENTIAL_KIND_GH_TOKEN: &str = "gh-token";
pub const CURSOR_CREDENTIAL_KIND_API_KEY: &str = "api-key";
pub const CURSOR_CREDENTIAL_KIND_OAUTH_TOKEN: &str = "oauth-token";
pub const AMP_CREDENTIAL_KIND_BROWSER_OAUTH: &str = "browser-oauth";
pub const GEMINI_AUTH_SELECTED_TYPE_OAUTH_PERSONAL: &str = "oauth-personal";
pub const GEMINI_AUTH_SELECTED_TYPE_API_KEY: &str = "gemini-api-key";
pub const GEMINI_AUTH_SELECTED_TYPE_VERTEX_AI: &str = "vertex-ai";
pub const QWEN_AUTH_SELECTED_TYPE_OAUTH: &str = "qwen-oauth";
pub const GEMINI_FORCE_FILE_STORAGE_ENV: &str = "GEMINI_FORCE_FILE_STORAGE";
pub const KIMI_SHARE_DIR_ENV: &str = "KIMI_SHARE_DIR";
pub const CODEX_API_SHAPE_OPENAI_RESPONSES: &str = "openai_responses";
pub const CODEX_AUTH_TYPE_BEARER: &str = "bearer";
pub const CODEX_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const QWEN_OAUTH_CREDS_RELATIVE_PATH: &str = ".qwen/oauth_creds.json";

fn default_codex_credential_kind() -> String {
    CODEX_CREDENTIAL_KIND_OAUTH.to_string()
}

fn default_codex_api_shape() -> String {
    CODEX_API_SHAPE_OPENAI_RESPONSES.to_string()
}

fn default_codex_auth_type() -> String {
    CODEX_AUTH_TYPE_BEARER.to_string()
}

fn default_codex_base_url() -> Option<String> {
    Some(CODEX_DEFAULT_BASE_URL.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexEndpointProfile {
    #[serde(default = "default_codex_api_shape")]
    pub api_shape: String,
    #[serde(default = "default_codex_auth_type")]
    pub auth_type: String,
    #[serde(
        default = "default_codex_base_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_url: Option<String>,
}

impl Default for CodexEndpointProfile {
    fn default() -> Self {
        Self {
            api_shape: default_codex_api_shape(),
            auth_type: default_codex_auth_type(),
            base_url: default_codex_base_url(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAccountEntry {
    pub id: String,
    pub label: String,
    #[serde(default = "default_codex_credential_kind")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub provider_account_id: Option<String>,
    #[serde(default)]
    pub plan_type: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub secret_ref: Option<String>,
    #[serde(default)]
    pub endpoint_profile: CodexEndpointProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexAccountRegistry {
    #[serde(default)]
    pub active_account_id: Option<String>,
    #[serde(default)]
    pub accounts: Vec<CodexAccountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexLoginStatus {
    pub account_id: String,
    pub auth_url: String,
    #[serde(default)]
    pub expected_callback_url: Option<String>,
    #[serde(default)]
    pub completion_token: Option<String>,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexHostImportProbe {
    pub available: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub auth_kind: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexAuthImportOutcome {
    pub registry: CodexAccountRegistry,
    pub account_id: String,
    pub created: bool,
}

async fn migrate_legacy_codex_storage(data_root: &Path) -> Result<()> {
    let moves = [
        (
            legacy_codex_accounts_root(data_root),
            codex_accounts_root(data_root),
        ),
        (
            legacy_codex_secrets_root(data_root),
            codex_secrets_root(data_root),
        ),
        (
            legacy_codex_runtime_home(data_root),
            codex_runtime_home(data_root),
        ),
    ];
    for (legacy_path, canonical_path) in moves {
        if !legacy_path.exists() || canonical_path.exists() {
            continue;
        }
        if let Some(parent) = canonical_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&legacy_path, &canonical_path).await?;
    }
    Ok(())
}

pub async fn load_codex_registry(data_root: &Path) -> Result<CodexAccountRegistry> {
    migrate_legacy_codex_storage(data_root).await?;
    load_json_registry(&codex_registry_path(data_root), "Codex account registry").await
}

pub async fn save_codex_registry(data_root: &Path, registry: &CodexAccountRegistry) -> Result<()> {
    migrate_legacy_codex_storage(data_root).await?;
    save_json_registry(&codex_registry_path(data_root), registry).await
}

pub async fn upsert_codex_account(
    data_root: &Path,
    entry: CodexAccountEntry,
) -> Result<CodexAccountRegistry> {
    let mut registry = load_codex_registry(data_root).await?;
    let mut normalized = entry;
    let account_id = normalized.id.clone();
    if normalized.kind.trim().is_empty() {
        normalized.kind = default_codex_credential_kind();
    }
    normalize_endpoint_profile(&mut normalized.endpoint_profile);
    ensure_codex_endpoint_profile_compatible(&normalized.endpoint_profile)?;
    if let Some(existing) = registry.accounts.iter_mut().find(|a| a.id == account_id) {
        let prev_secret_ref = existing.secret_ref.clone();
        *existing = normalized;
        if existing.secret_ref.is_none() {
            existing.secret_ref = prev_secret_ref;
        }
    } else {
        registry.accounts.push(normalized);
    }
    save_codex_registry(data_root, &registry).await?;
    Ok(registry)
}

pub async fn remove_codex_account(
    data_root: &Path,
    account_id: &str,
) -> Result<CodexAccountRegistry> {
    ensure_safe_account_id(account_id)?;
    let mut registry = load_codex_registry(data_root).await?;
    let was_active = registry.active_account_id.as_deref() == Some(account_id);
    let removed: Vec<CodexAccountEntry> = registry
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
        codex_secret_path,
    );
    registry.accounts.retain(|a| a.id != account_id);
    if was_active {
        registry.active_account_id = None;
    }
    save_codex_registry(data_root, &registry).await?;
    clear_runtime_auth_projection_for_runtime_roots(data_root, account_id).await?;
    for secret_path in secret_paths {
        if secret_path.exists() {
            let _ = tokio::fs::remove_file(secret_path).await;
        }
    }
    let account_dir = codex_account_dir(data_root, account_id);
    if account_dir.exists() {
        tokio::fs::remove_dir_all(account_dir).await?;
    }
    Ok(registry)
}

pub async fn require_codex_account_exists(data_root: &Path, account_id: &str) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let registry = load_codex_registry(data_root).await?;
    ensure_account_exists(
        registry
            .accounts
            .iter()
            .any(|account| account.id == account_id),
    )
}

pub async fn codex_account_deletion_in_progress(
    data_root: &Path,
    account_id: &str,
) -> Result<bool> {
    ensure_safe_account_id(account_id)?;
    Ok(self::paths::codex_account_deletion_marker(data_root, account_id).exists())
}

pub async fn begin_codex_account_deletion(
    data_root: &Path,
    account_id: &str,
) -> Result<Option<String>> {
    ensure_safe_account_id(account_id)?;
    let registry = load_codex_registry(data_root).await?;
    ensure_account_exists(
        registry
            .accounts
            .iter()
            .any(|account| account.id == account_id),
    )?;
    let marker = self::paths::codex_account_deletion_marker(data_root, account_id);
    write_secure_file_atomic(&marker, b"deleting")
        .await
        .with_context(|| format!("writing Codex account deletion marker {}", marker.display()))?;
    let previous_active = registry.active_account_id.clone();
    if previous_active.as_deref() == Some(account_id) {
        set_active_codex_account(data_root, None).await?;
    }
    Ok(previous_active)
}

pub async fn finish_codex_account_deletion(data_root: &Path, account_id: &str) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let marker = self::paths::codex_account_deletion_marker(data_root, account_id);
    match tokio::fs::remove_file(&marker).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "removing Codex account deletion marker {}",
                marker.display()
            )
        }),
    }
}

pub async fn abort_codex_account_deletion(
    data_root: &Path,
    account_id: &str,
    previous_active: Option<String>,
) -> Result<()> {
    finish_codex_account_deletion(data_root, account_id).await?;
    if previous_active.as_deref() == Some(account_id) {
        set_active_codex_account(data_root, previous_active).await?;
    }
    Ok(())
}

pub async fn cleanup_codex_account_broker_home(data_root: &Path, account_id: &str) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let broker_home = codex_broker_home(data_root, account_id);
    if let Some(broker_root) = broker_home.parent() {
        if broker_root.exists() {
            tokio::fs::remove_dir_all(broker_root)
                .await
                .with_context(|| {
                    format!(
                        "removing Codex broker home directory {}",
                        broker_root.display()
                    )
                })?;
        }
    }
    Ok(())
}

pub async fn set_active_codex_account(
    data_root: &Path,
    account_id: Option<String>,
) -> Result<CodexAccountRegistry> {
    let mut registry = load_codex_registry(data_root).await?;
    let previous_active = registry.active_account_id.clone();
    if let Some(active_id) = account_id.as_deref() {
        let Some(entry) = registry.accounts.iter().find(|a| a.id == active_id) else {
            anyhow::bail!("unknown account");
        };
        ensure_codex_endpoint_profile_compatible(&entry.endpoint_profile)?;
    }
    registry.active_account_id = account_id.clone();
    if let Some(active_id) = account_id {
        let now = Utc::now();
        if let Some(entry) = registry.accounts.iter_mut().find(|a| a.id == active_id) {
            entry.last_used_at = Some(now);
        }
    }
    save_codex_registry(data_root, &registry).await?;
    if previous_active.as_deref() != registry.active_account_id.as_deref() {
        if let Some(previous_active) = previous_active.as_deref() {
            clear_legacy_runtime_auth_projection_for_runtime_roots(data_root, previous_active)
                .await?;
        }
    }
    if registry.active_account_id.is_none() {
        clear_runtime_auth_projection(data_root).await?;
    }
    Ok(registry)
}

pub async fn ensure_codex_account_dir(data_root: &Path, account_id: &str) -> Result<PathBuf> {
    let dir = codex_account_dir(data_root, account_id);
    ctx_fs::permissions::ensure_private_dir(&dir).await?;
    Ok(dir)
}

pub fn codex_env_for_account(data_root: &Path, account_id: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let dir = codex_broker_home(data_root, account_id);
    env.insert("CODEX_HOME".to_string(), dir.to_string_lossy().to_string());
    env.insert(
        "CTX_CODEX_AUTH_ACCOUNT_ID".to_string(),
        account_id.to_string(),
    );
    env
}

pub async fn codex_env_for_available_account(
    data_root: &Path,
    account_id: &str,
) -> Result<HashMap<String, String>> {
    ensure_safe_account_id(account_id)?;
    require_codex_account_exists(data_root, account_id).await?;
    if codex_account_deletion_in_progress(data_root, account_id).await? {
        anyhow::bail!("codex account is being deleted");
    }
    Ok(codex_env_for_account(data_root, account_id))
}

pub async fn subscription_env_for_active_account(
    data_root: &Path,
    provider_id: &str,
) -> Result<HashMap<String, String>> {
    match provider_id {
        CODEX_PROVIDER_ID => codex_env_for_active_account(data_root).await,
        "claude-crp" => claude_env_for_active_account(data_root).await,
        "gemini" => gemini_env_for_active_account(data_root).await,
        "qwen" => qwen_env_for_active_account(data_root).await,
        "kimi" => kimi_env_for_active_account(data_root).await,
        "mistral" => mistral_env_for_active_account(data_root).await,
        "copilot" => copilot_env_for_active_account(data_root).await,
        "cursor" => cursor_env_for_active_account(data_root).await,
        "amp" => amp_env_for_active_account(data_root).await,
        _ => Ok(HashMap::new()),
    }
}

pub async fn subscription_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
    provider_id: &str,
) -> Result<HashMap<String, String>> {
    if data_root == runtime_root {
        return subscription_env_for_active_account(data_root, provider_id).await;
    }

    match provider_id {
        CODEX_PROVIDER_ID => {
            codex_env_for_active_account_with_runtime_root(data_root, runtime_root).await
        }
        "claude-crp" => {
            claude_env_for_active_account_with_runtime_root(data_root, runtime_root).await
        }
        "gemini" => gemini_env_for_active_account_with_runtime_root(data_root, runtime_root).await,
        "qwen" => qwen_env_for_active_account_with_runtime_root(data_root, runtime_root).await,
        "kimi" => kimi_env_for_active_account_with_runtime_root(data_root, runtime_root).await,
        "mistral" => {
            mistral_env_for_active_account_with_runtime_root(data_root, runtime_root).await
        }
        "copilot" => {
            copilot_env_for_active_account_with_runtime_root(data_root, runtime_root).await
        }
        "cursor" => cursor_env_for_active_account_with_runtime_root(data_root, runtime_root).await,
        "amp" => amp_env_for_active_account_with_runtime_root(data_root, runtime_root).await,
        _ => Ok(HashMap::new()),
    }
}

pub fn normalize_label(label: Option<String>, account_id: &str) -> String {
    label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Codex Account {account_id}"))
}
