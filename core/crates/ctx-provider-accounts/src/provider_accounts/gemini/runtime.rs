use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::{
    ensure_gemini_account_home, gemini_account_home, load_gemini_registry,
    read_gemini_secret_for_ref, GEMINI_FORCE_FILE_STORAGE_ENV, GEMINI_RUNTIME_AUTH_ENV_KEYS,
};

pub(crate) fn clear_gemini_runtime_auth_env(env: &mut HashMap<String, String>) {
    for key in GEMINI_RUNTIME_AUTH_ENV_KEYS {
        env.insert((*key).to_string(), String::new());
    }
}

pub fn apply_gemini_api_key_runtime_auth_env(env: &mut HashMap<String, String>, api_key: String) {
    clear_gemini_runtime_auth_env(env);
    env.insert("GEMINI_API_KEY".to_string(), api_key);
}

pub fn apply_gemini_vertex_runtime_auth_env(
    env: &mut HashMap<String, String>,
    credentials_path: PathBuf,
    project_id: String,
    location: String,
) {
    clear_gemini_runtime_auth_env(env);
    env.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
        credentials_path.to_string_lossy().to_string(),
    );
    env.insert("GOOGLE_CLOUD_PROJECT".to_string(), project_id.clone());
    env.insert("GOOGLE_CLOUD_PROJECT_ID".to_string(), project_id);
    env.insert("GOOGLE_CLOUD_LOCATION".to_string(), location);
    env.insert("GOOGLE_GENAI_USE_VERTEXAI".to_string(), "true".to_string());
}

pub fn gemini_env_for_account(data_root: &Path, account_id: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    clear_gemini_runtime_auth_env(&mut env);
    env.insert(
        "GEMINI_CLI_HOME".to_string(),
        gemini_account_home(data_root, account_id)
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        GEMINI_FORCE_FILE_STORAGE_ENV.to_string(),
        "true".to_string(),
    );
    env
}

pub async fn gemini_env_for_active_account(data_root: &Path) -> Result<HashMap<String, String>> {
    let registry = load_gemini_registry(data_root).await?;
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
        bail!("active gemini account has no secret reference");
    };

    let secret = read_gemini_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_gemini_account_home(data_root, active, &secret).await?;
    Ok(gemini_env_for_account(data_root, active))
}

pub(crate) async fn gemini_env_for_active_account_with_runtime_root(
    data_root: &Path,
    runtime_root: &Path,
) -> Result<HashMap<String, String>> {
    let registry = load_gemini_registry(data_root).await?;
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
        bail!("active gemini account has no secret reference");
    };

    let secret = read_gemini_secret_for_ref(data_root, secret_ref).await?;
    let _ = ensure_gemini_account_home(runtime_root, active, &secret).await?;
    Ok(gemini_env_for_account(runtime_root, active))
}
