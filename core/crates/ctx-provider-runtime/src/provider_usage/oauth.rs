use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<CodexAuthTokens>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexConfigFile {
    chatgpt_base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexUsageAuthKind {
    OAuth,
    ApiKey,
}

pub(super) async fn codex_usage_auth_kind(
    env: &HashMap<String, String>,
) -> Result<CodexUsageAuthKind> {
    let auth_path = resolve_codex_auth_path(env)?;
    let auth = load_codex_auth(&auth_path).await?;
    if let Some(tokens) = auth.tokens.as_ref() {
        let has_access = tokens
            .access_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_refresh = tokens
            .refresh_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if has_access || has_refresh {
            return Ok(CodexUsageAuthKind::OAuth);
        }
    }
    if auth
        .openai_api_key
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(CodexUsageAuthKind::ApiKey);
    }
    Err(anyhow!("codex auth.json has no tokens or api key"))
}

pub(super) async fn fetch_codex_usage_oauth(
    env: &HashMap<String, String>,
) -> Result<serde_json::Value> {
    let auth_path = resolve_codex_auth_path(env)?;
    let auth = load_codex_auth(&auth_path).await?;
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| anyhow!("codex auth.json missing tokens"))?;
    let access_token = tokens
        .access_token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("codex auth.json missing tokens.access_token"))?
        .to_string();
    let account_id = tokens.account_id.clone();

    let mut base_url = read_codex_base_url(env).await?;
    base_url = base_url.trim_end_matches('/').to_string();
    let usage_url = if base_url.contains("/backend-api") {
        format!("{base_url}/wham/usage")
    } else {
        format!("{base_url}/api/codex/usage")
    };

    let client = reqwest::Client::new();
    codex_usage_request(&client, &usage_url, &access_token, account_id.as_deref()).await
}

async fn codex_usage_request(
    client: &reqwest::Client,
    url: &str,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client.get(url).bearer_auth(access_token);
    req = req.header("User-Agent", "ctx");
    if let Some(account_id) = account_id {
        req = req.header("ChatGPT-Account-Id", account_id);
    }
    let resp = req.send().await.context("codex usage request failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let msg = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            "codex usage unauthorized; ctx does not refresh Codex OAuth tokens from usage polling"
        } else {
            "codex usage request failed"
        };
        return Err(anyhow!("{msg}: {status}; body={body}"));
    }
    let payload = resp.json::<serde_json::Value>().await?;
    Ok(payload)
}

async fn load_codex_auth(auth_path: &Path) -> Result<CodexAuthFile> {
    let contents = tokio::fs::read_to_string(auth_path)
        .await
        .with_context(|| format!("missing codex auth.json at {}", auth_path.display()))?;
    let auth: CodexAuthFile = serde_json::from_str(&contents)?;
    if auth.tokens.is_none() && auth.openai_api_key.is_none() {
        return Err(anyhow!("codex auth.json has no tokens or api key"));
    }
    Ok(auth)
}

async fn read_codex_base_url(env: &HashMap<String, String>) -> Result<String> {
    let codex_home = resolve_codex_home(env)?;
    let config_path = codex_home.join("config.toml");
    let contents = match tokio::fs::read_to_string(&config_path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok("https://chatgpt.com/backend-api".to_string());
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!("reading codex config.toml at {}", config_path.display())
            });
        }
    };
    let config = toml::from_str::<CodexConfigFile>(&contents)
        .with_context(|| format!("invalid codex config.toml at {}", config_path.display()))?;
    if let Some(url) = config.chatgpt_base_url {
        return Ok(url);
    }
    Ok("https://chatgpt.com/backend-api".to_string())
}

fn lookup_env(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key).cloned().or_else(|| std::env::var(key).ok())
}

fn resolve_codex_auth_path(env: &HashMap<String, String>) -> Result<PathBuf> {
    if let Some(path) = lookup_env(env, "CTX_CODEX_AUTH_PATH").filter(|v| !v.trim().is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let codex_home = resolve_codex_home(env)?;
    Ok(codex_home.join("auth.json"))
}

fn resolve_codex_home(env: &HashMap<String, String>) -> Result<PathBuf> {
    if let Some(home) = lookup_env(env, "CODEX_HOME").filter(|v| !v.trim().is_empty()) {
        return Ok(PathBuf::from(home));
    }
    let base = directories::BaseDirs::new().ok_or_else(|| anyhow!("missing home dir"))?;
    Ok(base.home_dir().join(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_usage_auth_kind_classifies_oauth_without_refreshing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        tokio::fs::write(
            &auth_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "account_id": "acct-1"
                }
            })
            .to_string(),
        )
        .await
        .expect("write auth.json");
        let env = HashMap::from([(
            "CODEX_HOME".to_string(),
            temp.path().to_string_lossy().to_string(),
        )]);

        assert_eq!(
            codex_usage_auth_kind(&env).await.expect("auth kind"),
            CodexUsageAuthKind::OAuth
        );
    }

    #[tokio::test]
    async fn codex_usage_auth_kind_classifies_access_token_only_as_oauth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        tokio::fs::write(
            &auth_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "access-token",
                    "account_id": "acct-1"
                }
            })
            .to_string(),
        )
        .await
        .expect("write auth.json");
        let env = HashMap::from([(
            "CODEX_HOME".to_string(),
            temp.path().to_string_lossy().to_string(),
        )]);

        assert_eq!(
            codex_usage_auth_kind(&env).await.expect("auth kind"),
            CodexUsageAuthKind::OAuth
        );
    }

    #[tokio::test]
    async fn codex_usage_auth_kind_classifies_api_key_with_incomplete_token_stub() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        tokio::fs::write(
            &auth_path,
            serde_json::json!({
                "OPENAI_API_KEY": "sk-test",
                "tokens": {
                    "access_token": "",
                    "account_id": "stale-acct"
                }
            })
            .to_string(),
        )
        .await
        .expect("write auth.json");
        let env = HashMap::from([(
            "CODEX_HOME".to_string(),
            temp.path().to_string_lossy().to_string(),
        )]);

        assert_eq!(
            codex_usage_auth_kind(&env).await.expect("auth kind"),
            CodexUsageAuthKind::ApiKey
        );
    }

    #[tokio::test]
    async fn codex_usage_auth_kind_treats_any_refresh_token_as_oauth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        tokio::fs::write(
            &auth_path,
            serde_json::json!({
                "OPENAI_API_KEY": "sk-test",
                "tokens": {
                    "access_token": "",
                    "refresh_token": "refresh-token",
                    "account_id": "stale-acct"
                }
            })
            .to_string(),
        )
        .await
        .expect("write auth.json");
        let env = HashMap::from([(
            "CODEX_HOME".to_string(),
            temp.path().to_string_lossy().to_string(),
        )]);

        assert_eq!(
            codex_usage_auth_kind(&env).await.expect("auth kind"),
            CodexUsageAuthKind::OAuth
        );
    }

    #[tokio::test]
    async fn fetch_codex_usage_oauth_fails_closed_on_malformed_codex_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        tokio::fs::write(
            &auth_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "access-token",
                    "refresh_token": "refresh-token"
                }
            })
            .to_string(),
        )
        .await
        .expect("write auth.json");
        tokio::fs::write(temp.path().join("config.toml"), "chatgpt_base_url = [")
            .await
            .expect("write invalid config.toml");

        let env = HashMap::from([(
            "CODEX_HOME".to_string(),
            temp.path().to_string_lossy().to_string(),
        )]);
        let err = fetch_codex_usage_oauth(&env)
            .await
            .expect_err("malformed codex config.toml should fail closed");
        let message = format!("{err:#}");
        assert!(
            message.contains("invalid codex config.toml"),
            "expected parse context in error: {message}"
        );
        assert!(
            message.contains("config.toml"),
            "expected config path in error: {message}"
        );
    }
}
