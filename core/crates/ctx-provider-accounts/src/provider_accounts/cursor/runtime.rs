use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::{
    cursor_account_home, load_cursor_registry, read_cursor_secret_for_ref,
    CURSOR_CREDENTIAL_KIND_OAUTH_TOKEN,
};

pub async fn ensure_cursor_account_home(data_root: &Path, account_id: &str) -> Result<PathBuf> {
    let home = cursor_account_home(data_root, account_id);
    ctx_fs::permissions::ensure_private_dir(&home).await?;
    Ok(home)
}

pub fn cursor_env_for_account(
    data_root: &Path,
    account_id: &str,
    auth_token: &str,
    credential_kind: &str,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "CURSOR_CONFIG_DIR".to_string(),
        cursor_account_home(data_root, account_id)
            .to_string_lossy()
            .to_string(),
    );
    match credential_kind {
        CURSOR_CREDENTIAL_KIND_OAUTH_TOKEN => {
            env.insert("CURSOR_AUTH_TOKEN".to_string(), auth_token.to_string());
        }
        _ => {
            env.insert("CURSOR_API_KEY".to_string(), auth_token.to_string());
        }
    }
    env
}

pub async fn cursor_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_cursor_registry(data_root).await?;
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
        bail!("active cursor account has no secret reference");
    };
    let secret = read_cursor_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_cursor_account_home(data_root, active).await?;
    Ok(cursor_env_for_account(
        data_root,
        active,
        &secret.auth_token,
        &entry.kind,
    ))
}

pub(crate) async fn cursor_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_cursor_registry(data_root).await?;
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
        bail!("active cursor account has no secret reference");
    };
    let secret = read_cursor_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_cursor_account_home(runtime_root, active).await?;
    Ok(cursor_env_for_account(
        runtime_root,
        active,
        &secret.auth_token,
        &entry.kind,
    ))
}
