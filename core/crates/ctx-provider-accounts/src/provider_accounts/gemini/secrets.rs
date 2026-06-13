use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::super::shared::{
    parse_optional_json_value, parse_required_json_object, write_secure_file_atomic,
};
use super::{
    gemini_account_home, gemini_secret_path, GeminiSecretEnvelope,
    GEMINI_AUTH_SELECTED_TYPE_OAUTH_PERSONAL, GEMINI_SECRET_VERSION,
};

pub(super) async fn write_gemini_secret_for_account(
    data_root: &Path,
    account_id: &str,
    oauth_creds_json: &str,
    google_accounts_json: Option<&str>,
) -> Result<String> {
    let oauth_creds = parse_required_json_object(oauth_creds_json, "oauth_creds_json")?;
    let google_accounts = parse_optional_json_value(google_accounts_json, "google_accounts_json")?;
    let secret_ref = format!("{account_id}.json");
    let path = gemini_secret_path(data_root, &secret_ref)?;
    let envelope = GeminiSecretEnvelope {
        version: GEMINI_SECRET_VERSION,
        oauth_creds,
        google_accounts,
    };
    write_secure_file_atomic(&path, &serde_json::to_vec_pretty(&envelope)?).await?;
    Ok(secret_ref)
}

pub(crate) async fn read_gemini_secret_for_ref(
    data_root: &Path,
    secret_ref: &str,
) -> Result<GeminiSecretEnvelope> {
    let path = gemini_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading gemini secret {}", path.display()))?;
    let parsed: GeminiSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid gemini secret {}", path.display()))?;
    if parsed.version != GEMINI_SECRET_VERSION {
        bail!(
            "unsupported gemini secret version {} at {}",
            parsed.version,
            path.display()
        );
    }
    if !parsed.oauth_creds.is_object() {
        bail!("gemini oauth_creds must be a JSON object");
    }
    Ok(parsed)
}

pub(super) async fn ensure_gemini_account_home(
    data_root: &Path,
    account_id: &str,
    secret: &GeminiSecretEnvelope,
) -> Result<PathBuf> {
    let home = gemini_account_home(data_root, account_id);
    let gemini_dir = home.join(".gemini");
    ctx_fs::permissions::ensure_private_dir(&home).await?;
    ctx_fs::permissions::ensure_private_dir(&gemini_dir).await?;
    write_secure_file_atomic(
        &gemini_dir.join("oauth_creds.json"),
        &serde_json::to_vec_pretty(&secret.oauth_creds)?,
    )
    .await?;
    if let Some(accounts) = secret.google_accounts.as_ref() {
        write_secure_file_atomic(
            &gemini_dir.join("google_accounts.json"),
            &serde_json::to_vec_pretty(accounts)?,
        )
        .await?;
    } else {
        let accounts_path = gemini_dir.join("google_accounts.json");
        match tokio::fs::remove_file(&accounts_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    write_gemini_auth_settings(&gemini_dir, GEMINI_AUTH_SELECTED_TYPE_OAUTH_PERSONAL).await?;
    Ok(home)
}

pub async fn write_gemini_auth_settings(gemini_dir: &Path, selected_type: &str) -> Result<()> {
    let settings = serde_json::json!({
        "security": {
            "auth": {
                "selectedType": selected_type
            }
        }
    });
    write_secure_file_atomic(
        &gemini_dir.join("settings.json"),
        &serde_json::to_vec_pretty(&settings)?,
    )
    .await
}
