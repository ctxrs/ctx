use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use url::Url;

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:4399";
const DAEMON_AUTH_FILENAME: &str = "daemon_auth.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonAuthFile {
    token: String,
    #[serde(default)]
    daemon_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub base_url: String,
    pub auth_token: Option<String>,
}

pub(crate) fn normalize_base_url(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("daemon base URL is empty"));
    }
    Url::parse(trimmed).with_context(|| format!("invalid daemon URL: {trimmed}"))?;
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn default_data_dir() -> Result<PathBuf> {
    let base = BaseDirs::new().context("resolving home directory")?;
    Ok(base.home_dir().join(".ctx"))
}

fn read_daemon_auth_file(path: &Path) -> Result<Option<DaemonAuthFile>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let auth: DaemonAuthFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing daemon auth file {}", path.display()))?;
            if auth.token.trim().is_empty() {
                anyhow::bail!("daemon auth file {} contains empty token", path.display());
            }
            Ok(Some(auth))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(err).with_context(|| format!("reading daemon auth file {}", path.display()))
        }
    }
}

fn resolve_daemon_config_with(
    data_dir: Option<&Path>,
    override_url: Option<&str>,
) -> Result<DaemonConfig> {
    let auth = if let Some(dir) = data_dir {
        read_daemon_auth_file(&dir.join(DAEMON_AUTH_FILENAME))?
    } else {
        None
    };

    let base_url = override_url
        .map(str::to_string)
        .or_else(|| auth.as_ref().and_then(|a| a.daemon_url.clone()))
        .unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string());

    Ok(DaemonConfig {
        base_url: normalize_base_url(&base_url)?,
        auth_token: auth.map(|a| a.token),
    })
}

pub fn resolve_daemon_config() -> Result<DaemonConfig> {
    let override_url = env::var("CTX_DAEMON_URL").ok();
    let data_dir = match env::var("CTX_DATA_DIR") {
        Ok(value) => Some(PathBuf::from(value)),
        Err(_) => Some(default_data_dir()?),
    };
    resolve_daemon_config_with(data_dir.as_deref(), override_url.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_url_trims() {
        let url = normalize_base_url("http://127.0.0.1:4399/").unwrap();
        assert_eq!(url, "http://127.0.0.1:4399");
    }

    #[test]
    fn normalize_base_url_rejects_empty() {
        let err = normalize_base_url(" ").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn resolve_daemon_config_prefers_override() {
        let dir = tempfile::tempdir().unwrap();
        let auth = DaemonAuthFile {
            token: "token".to_string(),
            daemon_url: Some("http://127.0.0.1:1234".to_string()),
        };
        let path = dir.path().join(DAEMON_AUTH_FILENAME);
        std::fs::write(&path, serde_json::to_vec(&auth).unwrap()).unwrap();

        let cfg =
            resolve_daemon_config_with(Some(dir.path()), Some("http://127.0.0.1:5678")).unwrap();
        assert_eq!(cfg.base_url, "http://127.0.0.1:5678");
        assert_eq!(cfg.auth_token.as_deref(), Some("token"));
    }

    #[test]
    fn resolve_daemon_config_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = resolve_daemon_config_with(Some(dir.path()), None).unwrap();
        assert_eq!(cfg.base_url, DEFAULT_DAEMON_URL);
        assert!(cfg.auth_token.is_none());
    }

    #[test]
    fn read_daemon_auth_file_rejects_empty_token() {
        let dir = tempfile::tempdir().unwrap();
        let auth = DaemonAuthFile {
            token: "".to_string(),
            daemon_url: None,
        };
        let path = dir.path().join(DAEMON_AUTH_FILENAME);
        std::fs::write(&path, serde_json::to_vec(&auth).unwrap()).unwrap();

        let err = read_daemon_auth_file(&path).unwrap_err();
        assert!(err.to_string().contains("empty token"));
    }
}
