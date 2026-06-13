use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::time::sleep;
use url::Url;

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Strategy {
    Auto,
    Llms,
    Repo,
    Sitemap,
    EditLink,
    Html,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DocsMirrorConfig {
    pub(crate) source: String,
    pub(crate) title: Option<String>,
    pub(crate) docs_url: Option<String>,
    pub(crate) repo_url: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) repo_subpath: Option<String>,
    pub(crate) llms_url: Option<String>,
    pub(crate) sitemap_url: Option<String>,
    pub(crate) include: Option<Vec<String>>,
    pub(crate) exclude: Option<Vec<String>>,
    pub(crate) strip_prefix: Option<String>,
    pub(crate) max_pages: Option<usize>,
    pub(crate) min_pages: Option<usize>,
    pub(crate) strategy: Option<Strategy>,
    pub(crate) throttle_ms: Option<u64>,
}

pub(crate) fn load_config(path: &Path) -> Result<DocsMirrorConfig> {
    let txt =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: DocsMirrorConfig = toml::from_str(&txt).context("parsing config")?;
    Ok(cfg)
}

pub(crate) fn resolve_docs_url(cfg: &DocsMirrorConfig) -> Option<String> {
    if let Some(url) = cfg.docs_url.as_ref() {
        return Some(url.clone());
    }
    if cfg.source.starts_with("http://") || cfg.source.starts_with("https://") {
        Some(cfg.source.clone())
    } else {
        None
    }
}

pub(crate) fn resolve_repo_url(cfg: &DocsMirrorConfig) -> Option<String> {
    if let Some(url) = cfg.repo_url.as_ref() {
        return Some(url.clone());
    }
    if looks_like_git_url(&cfg.source) {
        return Some(cfg.source.clone());
    }
    None
}

fn looks_like_git_url(value: &str) -> bool {
    value.ends_with(".git") || value.contains("git@") || value.starts_with("ssh://")
}

pub(crate) async fn throttle(cfg: &DocsMirrorConfig) {
    if let Some(ms) = cfg.throttle_ms {
        sleep(Duration::from_millis(ms)).await;
    }
}

pub(crate) fn filter_urls(cfg: &DocsMirrorConfig, urls: Vec<String>) -> Vec<String> {
    let includes = cfg.include.as_deref().unwrap_or(&[]);
    let excludes = cfg.exclude.as_deref().unwrap_or(&[]);
    if includes.is_empty() && excludes.is_empty() {
        return urls;
    }

    urls.into_iter()
        .filter(|url| {
            let path = Url::parse(url).ok().map(|u| u.path().to_string());
            let Some(path) = path else {
                return false;
            };
            let include_ok = if includes.is_empty() {
                true
            } else {
                includes.iter().any(|p| path.starts_with(p))
            };
            let exclude_ok = !excludes.iter().any(|p| path.starts_with(p));
            include_ok && exclude_ok
        })
        .collect()
}

pub(crate) fn apply_max(cfg: &DocsMirrorConfig, mut urls: Vec<String>) -> Vec<String> {
    if let Some(max) = cfg.max_pages {
        urls.truncate(max);
    }
    urls
}

pub(crate) fn derive_output_path(
    cfg: &DocsMirrorConfig,
    docs_url: Option<&str>,
    url: &str,
) -> Result<PathBuf> {
    let parsed = Url::parse(url).context("parsing url")?;
    let mut path = parsed.path().to_string();
    if path.starts_with('/') {
        path = path.trim_start_matches('/').to_string();
    }

    if let Some(prefix) = cfg.strip_prefix.as_deref() {
        let prefix = prefix.trim_start_matches('/');
        if path.starts_with(prefix) {
            path = path[prefix.len()..].trim_start_matches('/').to_string();
        }
    } else if let Some(base) = docs_url {
        if let Ok(base_url) = Url::parse(base) {
            let base_path = base_url.path().trim_start_matches('/');
            if !base_path.is_empty() && path.starts_with(base_path) {
                path = path[base_path.len()..].trim_start_matches('/').to_string();
            }
        }
    }

    if path.is_empty() {
        path = "index".to_string();
    } else if path.ends_with('/') {
        path.push_str("index");
    }

    if path.ends_with(".html") {
        path = path.trim_end_matches(".html").to_string();
    }

    if !path.ends_with(".md") && !path.ends_with(".mdx") {
        path.push_str(".md");
    }

    Ok(PathBuf::from(path))
}
