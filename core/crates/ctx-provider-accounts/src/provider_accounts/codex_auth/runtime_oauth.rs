use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;

use super::continuity::expose_legacy_codex_state_from_home;
use super::secret_store::{
    codex_auth_has_access_token, codex_auth_has_refresh_token, ingest_auth_value_for_account,
    project_auth_value_to_home,
};
use super::*;
use crate::provider_accounts::paths::{
    validate_codex_provider_root_before_broker_access,
    validate_codex_runtime_home_before_broker_access,
};

const CODEX_OAUTH_TOKEN_URL_ENV: &str = "CTX_CODEX_OAUTH_TOKEN_URL";
const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_REFRESH_SCOPE: &str = "openid profile email";
const CODEX_OAUTH_REFRESH_SKEW_SECS: i64 = 300;
const CODEX_OAUTH_REAUTH_REQUIRED_FILE: &str = ".ctx-oauth-reauth-required";

pub(super) async fn project_oauth_authority_to_runtime_home(
    data_root: &Path,
    runtime_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<bool> {
    validate_codex_provider_root_before_broker_access(data_root)?;
    validate_codex_runtime_home_before_broker_access(runtime_root)?;
    let projected_auth = oauth_runtime_projection(auth)?;
    let runtime_home = codex_oauth_runtime_home(runtime_root, account_id)?;
    let projected = project_auth_value_to_home(&runtime_home, &projected_auth).await?;
    expose_legacy_codex_state_from_home(data_root, &codex_runtime_home(data_root), &runtime_home)
        .await?;
    write_oauth_runtime_owner_marker(&runtime_home, account_id).await?;
    Ok(projected)
}

pub(crate) fn codex_oauth_runtime_home(runtime_root: &Path, account_id: &str) -> Result<PathBuf> {
    ensure_safe_account_id(account_id)?;
    validate_codex_runtime_home_before_broker_access(runtime_root)?;
    Ok(codex_runtime_home(runtime_root)
        .join("oauth-accounts")
        .join(account_id))
}

pub(super) async fn clear_oauth_runtime_home_for_account(
    runtime_root: &Path,
    account_id: &str,
) -> Result<()> {
    let runtime_home = codex_oauth_runtime_home(runtime_root, account_id)?;
    match tokio::fs::remove_dir_all(&runtime_home).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "removing Codex OAuth runtime home {}",
                runtime_home.display()
            )
        }),
    }
}

async fn write_oauth_runtime_owner_marker(runtime_home: &Path, account_id: &str) -> Result<()> {
    let marker = runtime_home.join(".ctx-auth-authority");
    write_secure_file_atomic(&marker, account_id.as_bytes()).await
}

pub(super) async fn ensure_broker_oauth_access_token_fresh(
    data_root: &Path,
    account_id: &str,
    broker_home: &Path,
) -> Result<serde_json::Value> {
    let Some(auth) = read_auth_value_from_home(broker_home).await? else {
        anyhow::bail!(
            "codex broker auth at {} is missing",
            broker_home.join("auth.json").display()
        );
    };
    if !codex_auth_has_refresh_token(&auth) {
        anyhow::bail!(
            "codex broker auth at {} is not an OAuth refresh-token credential",
            broker_home.join("auth.json").display()
        );
    }
    if !codex_access_token_needs_refresh(&auth) {
        return Ok(auth);
    }

    let lock_path = broker_home.join(".ctx-refresh-token.lock");
    let lock_file = tokio::task::spawn_blocking({
        let lock_path = lock_path.clone();
        move || -> Result<std::fs::File> {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .with_context(|| {
                    format!("opening Codex OAuth authority lock {}", lock_path.display())
                })?;
            fs2::FileExt::lock_exclusive(&file).with_context(|| {
                format!("locking Codex OAuth authority {}", lock_path.display())
            })?;
            Ok(file)
        }
    })
    .await
    .context("joining Codex OAuth refresh lock task")??;

    let locked_auth = read_auth_value_from_home(broker_home)
        .await?
        .ok_or_else(|| anyhow::anyhow!("codex broker auth disappeared during refresh"))?;
    if !codex_access_token_needs_refresh(&locked_auth) {
        drop(lock_file);
        return Ok(locked_auth);
    }

    let refreshed = refresh_codex_oauth_auth(data_root, account_id, &locked_auth).await?;
    project_auth_value_to_home(broker_home, &refreshed).await?;
    ingest_auth_value_for_account(data_root, account_id, &refreshed).await?;
    drop(lock_file);
    Ok(refreshed)
}

pub(super) async fn fail_if_codex_oauth_reauth_required(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let marker = codex_oauth_reauth_required_path(data_root, account_id);
    let detail = match tokio::fs::read_to_string(&marker).await {
        Ok(detail) => detail,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading Codex OAuth reauthentication marker {}",
                    marker.display()
                )
            });
        }
    };
    let detail = detail.trim();
    if detail.is_empty() {
        anyhow::bail!(
            "Codex OAuth reauthentication is required for account {account_id}. Sign in to Codex again through ctx to restore this account."
        );
    }
    anyhow::bail!(
        "Codex OAuth reauthentication is required for account {account_id}. Sign in to Codex again through ctx to restore this account. {detail}"
    );
}

pub(super) async fn codex_oauth_reauth_required(
    data_root: &Path,
    account_id: &str,
) -> Result<bool> {
    ensure_safe_account_id(account_id)?;
    let marker = codex_oauth_reauth_required_path(data_root, account_id);
    match tokio::fs::metadata(&marker).await {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| {
            format!(
                "checking Codex OAuth reauthentication marker {}",
                marker.display()
            )
        }),
    }
}

pub(super) async fn clear_codex_oauth_reauth_required(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let marker = codex_oauth_reauth_required_path(data_root, account_id);
    match tokio::fs::remove_file(&marker).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "removing Codex OAuth reauthentication marker {}",
                marker.display()
            )
        }),
    }
}

async fn read_auth_value_from_home(home: &Path) -> Result<Option<serde_json::Value>> {
    let auth_path = home.join("auth.json");
    let payload = match tokio::fs::read_to_string(&auth_path).await {
        Ok(payload) => payload,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading codex auth at {}", auth_path.display()));
        }
    };
    let auth: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("invalid codex auth JSON at {}", auth_path.display()))?;
    Ok(Some(auth))
}

fn oauth_runtime_projection(auth: &serde_json::Value) -> Result<serde_json::Value> {
    if !codex_auth_has_access_token(auth) {
        anyhow::bail!("codex OAuth auth is missing tokens.access_token");
    }
    let mut projected = auth.clone();
    let Some(object) = projected.as_object_mut() else {
        anyhow::bail!("codex OAuth auth must be a JSON object");
    };
    object.remove("OPENAI_API_KEY");
    let Some(tokens) = object
        .get_mut("tokens")
        .and_then(|value| value.as_object_mut())
    else {
        anyhow::bail!("codex OAuth auth is missing tokens");
    };
    tokens.remove("refresh_token");
    Ok(projected)
}

#[derive(Debug, Deserialize)]
struct CodexOAuthRefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

fn access_token_exp_epoch(auth: &serde_json::Value) -> Option<i64> {
    let token = auth
        .get("tokens")
        .and_then(|value| value.as_object())
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    claims.get("exp").and_then(|value| value.as_i64())
}

fn codex_access_token_needs_refresh(auth: &serde_json::Value) -> bool {
    if !codex_auth_has_refresh_token(auth) {
        return false;
    }
    if !codex_auth_has_access_token(auth) {
        return true;
    }
    let Some(exp) = access_token_exp_epoch(auth) else {
        return true;
    };
    exp <= Utc::now().timestamp() + CODEX_OAUTH_REFRESH_SKEW_SECS
}

async fn refresh_codex_oauth_auth(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<serde_json::Value> {
    let refresh_token = auth
        .get("tokens")
        .and_then(|value| value.as_object())
        .and_then(|tokens| tokens.get("refresh_token"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("codex OAuth auth is missing tokens.refresh_token"))?;
    let token_url = std::env::var(CODEX_OAUTH_TOKEN_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CODEX_OAUTH_TOKEN_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building Codex OAuth refresh client")?;
    let response = client
        .post(&token_url)
        .form(&[
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", CODEX_OAUTH_REFRESH_SCOPE),
        ])
        .send()
        .await
        .with_context(|| format!("refreshing Codex OAuth access token via {token_url}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if codex_oauth_refresh_failure_requires_reauth(status, &body) {
            mark_codex_oauth_reauth_required(data_root, account_id, status, &body).await?;
            anyhow::bail!(
                "Codex OAuth refresh token for account {account_id} is invalid or expired. Sign in to Codex again through ctx to restore this account."
            );
        }
        anyhow::bail!("Codex OAuth refresh failed: {status}; body={body}");
    }
    let refreshed = response
        .json::<CodexOAuthRefreshResponse>()
        .await
        .context("decoding Codex OAuth refresh response")?;
    let mut updated = auth.clone();
    let Some(object) = updated.as_object_mut() else {
        anyhow::bail!("codex OAuth auth must be a JSON object");
    };
    let tokens = object
        .entry("tokens")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("codex OAuth auth tokens must be an object"))?;
    tokens.insert(
        "access_token".to_string(),
        serde_json::Value::String(refreshed.access_token),
    );
    if let Some(refresh_token) = refreshed.refresh_token {
        if !refresh_token.trim().is_empty() {
            tokens.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token),
            );
        }
    }
    if let Some(id_token) = refreshed.id_token {
        if !id_token.trim().is_empty() {
            tokens.insert("id_token".to_string(), serde_json::Value::String(id_token));
        }
    }
    object.insert(
        "last_refresh".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339()),
    );
    Ok(updated)
}

fn codex_oauth_reauth_required_path(data_root: &Path, account_id: &str) -> PathBuf {
    codex_account_dir(data_root, account_id).join(CODEX_OAUTH_REAUTH_REQUIRED_FILE)
}

fn codex_oauth_refresh_failure_requires_reauth(status: StatusCode, body: &str) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("invalid_grant")
        || lower.contains("invalid_refresh_token")
        || lower.contains("refresh_token_reused")
        || lower.contains("token_expired")
        || lower.contains("token_invalidated")
        || lower.contains("invalid refresh")
        || lower.contains("refresh token")
            && (lower.contains("expired")
                || lower.contains("invalid")
                || lower.contains("revoked")
                || lower.contains("reuse"))
}

async fn mark_codex_oauth_reauth_required(
    data_root: &Path,
    account_id: &str,
    status: StatusCode,
    body: &str,
) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let account_dir = codex_account_dir(data_root, account_id);
    ctx_fs::permissions::ensure_private_dir(&account_dir).await?;
    let marker = codex_oauth_reauth_required_path(data_root, account_id);
    let detail = format!(
        "Last refresh failure: {status}; recovery: sign in again through ctx. Body: {}",
        body.trim()
    );
    write_secure_file_atomic(&marker, detail.as_bytes()).await
}
