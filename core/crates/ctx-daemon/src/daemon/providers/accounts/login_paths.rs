use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use ctx_provider_accounts as provider_accounts;

pub struct PreparedAmpLoginPaths {
    pub login_home: PathBuf,
    pub workdir: PathBuf,
    pub amp_home: PathBuf,
}

pub struct PreparedMistralLoginPaths {
    pub login_home: PathBuf,
    pub workdir: PathBuf,
    pub mistral_home: PathBuf,
}

pub struct PreparedGeminiLoginPaths {
    pub login_home: PathBuf,
    pub workdir: PathBuf,
    pub oauth_path: PathBuf,
    pub google_accounts_path: PathBuf,
}

pub struct PreparedQwenLoginPaths {
    pub login_home: PathBuf,
    pub workdir: PathBuf,
    pub oauth_path: PathBuf,
}

fn provider_login_home(data_root: &Path, provider_id: &str, login_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join(provider_id)
        .join("login-sessions")
        .join(login_id)
}

fn login_provider_base_env(data_root: &Path, daemon_url: &str) -> HashMap<String, String> {
    HashMap::from([
        ("CTX_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("CTX_MCP_DISABLED".to_string(), "1".to_string()),
        (
            "CTX_DATA_ROOT".to_string(),
            data_root.to_string_lossy().to_string(),
        ),
    ])
}

pub async fn prepare_amp_login_paths(
    data_root: &Path,
    login_id: &str,
) -> Result<PreparedAmpLoginPaths, String> {
    let login_home = provider_login_home(data_root, "amp", login_id);
    let workdir = login_home.join("workspace");
    if let Err(err) = tokio::fs::create_dir_all(&workdir).await {
        return Err(format!("failed to prepare login workspace: {err}"));
    }
    let amp_home = match provider_accounts::ensure_amp_runtime_home(data_root).await {
        Ok(home) => home,
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&login_home).await;
            return Err(format!("failed to prepare amp runtime home: {err}"));
        }
    };

    Ok(PreparedAmpLoginPaths {
        login_home,
        workdir,
        amp_home,
    })
}

pub fn amp_login_provider_env(
    data_root: &Path,
    daemon_url: &str,
    amp_home: &Path,
) -> HashMap<String, String> {
    let mut provider_env = login_provider_base_env(data_root, daemon_url);
    provider_env.insert("HOME".to_string(), amp_home.to_string_lossy().to_string());
    provider_env.insert(
        "XDG_CONFIG_HOME".to_string(),
        amp_home.join(".config").to_string_lossy().to_string(),
    );
    provider_env.insert(
        "XDG_CACHE_HOME".to_string(),
        amp_home.join(".cache").to_string_lossy().to_string(),
    );
    provider_env
}

pub async fn prepare_mistral_login_paths(
    data_root: &Path,
    login_id: &str,
) -> Result<PreparedMistralLoginPaths, String> {
    let login_home = provider_login_home(data_root, "mistral", login_id);
    let workdir = login_home.join("workspace");
    if let Err(err) = tokio::fs::create_dir_all(&workdir).await {
        return Err(format!("failed to prepare login workspace: {err}"));
    }
    let mistral_home = match provider_accounts::ensure_mistral_runtime_home(data_root).await {
        Ok(home) => home,
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&login_home).await;
            return Err(format!("failed to prepare mistral runtime home: {err}"));
        }
    };

    Ok(PreparedMistralLoginPaths {
        login_home,
        workdir,
        mistral_home,
    })
}

pub fn mistral_login_provider_env(
    data_root: &Path,
    daemon_url: &str,
    mistral_home: &Path,
) -> HashMap<String, String> {
    let mut provider_env = login_provider_base_env(data_root, daemon_url);
    provider_env.insert(
        "HOME".to_string(),
        mistral_home.to_string_lossy().to_string(),
    );
    provider_env.insert(
        "XDG_CONFIG_HOME".to_string(),
        mistral_home.join(".config").to_string_lossy().to_string(),
    );
    provider_env.insert(
        "XDG_CACHE_HOME".to_string(),
        mistral_home.join(".cache").to_string_lossy().to_string(),
    );
    provider_env
}

pub async fn prepare_gemini_login_paths(
    data_root: &Path,
    login_id: &str,
) -> Result<PreparedGeminiLoginPaths, String> {
    let login_home = provider_login_home(data_root, "gemini", login_id);
    let workdir = login_home.join("workspace");
    tokio::fs::create_dir_all(&workdir)
        .await
        .map_err(|err| format!("failed to prepare login workspace: {err}"))?;
    Ok(PreparedGeminiLoginPaths {
        oauth_path: login_home.join(".gemini").join("oauth_creds.json"),
        google_accounts_path: login_home.join(".gemini").join("google_accounts.json"),
        login_home,
        workdir,
    })
}

pub fn gemini_login_provider_env(
    data_root: &Path,
    daemon_url: &str,
    login_home: &Path,
) -> HashMap<String, String> {
    let mut provider_env = login_provider_base_env(data_root, daemon_url);
    provider_env.insert(
        "GEMINI_CLI_HOME".to_string(),
        login_home.to_string_lossy().to_string(),
    );
    provider_env.insert(
        provider_accounts::GEMINI_FORCE_FILE_STORAGE_ENV.to_string(),
        "true".to_string(),
    );
    provider_env
}

pub async fn prepare_qwen_login_paths(
    data_root: &Path,
    login_id: &str,
) -> Result<PreparedQwenLoginPaths, String> {
    let login_home = provider_login_home(data_root, "qwen", login_id);
    let workdir = login_home.join("workspace");
    if let Err(err) = tokio::fs::create_dir_all(&workdir).await {
        return Err(format!("failed to prepare login workspace: {err}"));
    }
    let _ = tokio::fs::create_dir_all(login_home.join(".config")).await;
    let _ = tokio::fs::create_dir_all(login_home.join(".cache")).await;
    let oauth_path = login_home.join(provider_accounts::QWEN_OAUTH_CREDS_RELATIVE_PATH);

    Ok(PreparedQwenLoginPaths {
        login_home,
        workdir,
        oauth_path,
    })
}

pub fn qwen_login_provider_env(
    data_root: &Path,
    daemon_url: &str,
    login_home: &Path,
) -> HashMap<String, String> {
    let mut provider_env = login_provider_base_env(data_root, daemon_url);
    provider_env.insert("HOME".to_string(), login_home.to_string_lossy().to_string());
    provider_env.insert(
        "XDG_CONFIG_HOME".to_string(),
        login_home.join(".config").to_string_lossy().to_string(),
    );
    provider_env.insert(
        "XDG_CACHE_HOME".to_string(),
        login_home.join(".cache").to_string_lossy().to_string(),
    );
    provider_env
}
