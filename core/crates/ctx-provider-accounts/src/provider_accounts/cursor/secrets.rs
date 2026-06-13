use std::path::Path;

use anyhow::{bail, Context, Result};

use super::{
    cursor_secret_path, normalize_cursor_auth_token, normalize_optional_cursor_auth_token,
    write_secure_file_atomic, CursorSecretEnvelope, CursorSecretRecord, CURSOR_SECRET_VERSION,
};

pub(super) async fn write_cursor_secret_for_account(
    data_root: &Path,
    account_id: &str,
    auth_token: &str,
    refresh_token: Option<&str>,
) -> Result<String> {
    let secret_ref = format!("{account_id}.json");
    write_cursor_secret_for_ref(data_root, &secret_ref, auth_token, refresh_token).await?;
    Ok(secret_ref)
}

pub(super) async fn write_cursor_secret_for_ref(
    data_root: &Path,
    secret_ref: &str,
    auth_token: &str,
    refresh_token: Option<&str>,
) -> Result<()> {
    let auth_token = normalize_cursor_auth_token(auth_token)?;
    let refresh_token =
        normalize_optional_cursor_auth_token(refresh_token)?.filter(|token| token != &auth_token);
    let path = cursor_secret_path(data_root, secret_ref)?;
    let envelope = CursorSecretEnvelope {
        version: CURSOR_SECRET_VERSION,
        auth_token,
        refresh_token,
    };
    write_secure_file_atomic(&path, &serde_json::to_vec_pretty(&envelope)?).await?;
    Ok(())
}

pub(super) async fn read_cursor_secret_for_ref(
    data_root: &Path,
    secret_ref: &str,
) -> Result<CursorSecretRecord> {
    let path = cursor_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading cursor secret {}", path.display()))?;
    let parsed: CursorSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid cursor secret {}", path.display()))?;
    if parsed.version != CURSOR_SECRET_VERSION {
        bail!(
            "unsupported cursor secret version {} at {}",
            parsed.version,
            path.display()
        );
    }
    Ok(CursorSecretRecord {
        auth_token: normalize_cursor_auth_token(&parsed.auth_token)?,
        refresh_token: normalize_optional_cursor_auth_token(parsed.refresh_token.as_deref())?,
    })
}
