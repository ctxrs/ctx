use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use reqwest::header;
use url::Url;

use crate::config::{throttle, DocsMirrorConfig};
use crate::http::send_with_retries;

pub(crate) async fn crawl_html_pages(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    docs_url: &str,
) -> Result<Vec<String>> {
    crawl_html_pages_with_seeds(client, cfg, docs_url, &[]).await
}

pub(crate) async fn crawl_html_pages_rendered(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    docs_url: &str,
) -> Result<Option<Vec<String>>> {
    let Some(seeds) = try_playwright_links(docs_url).await? else {
        return Ok(None);
    };
    let pages = crawl_html_pages_with_seeds(client, cfg, docs_url, &seeds).await?;
    Ok(Some(pages))
}

async fn crawl_html_pages_with_seeds(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    docs_url: &str,
    seeds: &[String],
) -> Result<Vec<String>> {
    let base = Url::parse(docs_url).context("parsing docs_url")?;
    let origin_scheme = base.scheme().to_string();
    let origin_host = base
        .host_str()
        .ok_or_else(|| anyhow!("docs_url missing host"))?
        .to_string();
    let origin_port = base.port_or_known_default();
    let mut prefix = path_scope_prefix(base.path());
    let mut allow_all = prefix.is_empty() || prefix == "/";
    let max_pages = cfg.max_pages.unwrap_or(200);

    let start = normalize_url(base.clone());
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    let start_key = start.to_string();
    seen.insert(start_key);
    queue.push_back(start);

    let mut seed_urls = Vec::new();
    for link in seeds {
        let resolved = match base.join(link) {
            Ok(url) => url,
            Err(_) => continue,
        };
        if let Some(normalized) = normalize_candidate(
            resolved,
            &origin_scheme,
            &origin_host,
            origin_port,
            &prefix,
            allow_all,
        ) {
            let key = normalized.to_string();
            if seen.insert(key) {
                seed_urls.push(normalized);
            }
        }
    }
    seed_urls.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for seed in seed_urls {
        if results.len() + queue.len() >= max_pages {
            break;
        }
        queue.push_back(seed);
    }

    while let Some(url) = queue.pop_front() {
        if results.len() >= max_pages {
            break;
        }
        let resp = match send_with_retries(client, url.as_str()).await {
            Ok(resp) => resp,
            Err(_) => {
                throttle(cfg).await;
                continue;
            }
        };
        let effective_url = resp.url().clone();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
            .unwrap_or_default();
        let html = match resp.text().await {
            Ok(text) => text,
            Err(_) => {
                throttle(cfg).await;
                continue;
            }
        };
        let is_first = results.is_empty();
        results.push(url.to_string());
        if is_first {
            if let Some(host) = effective_url.host_str() {
                if host == origin_host
                    && effective_url.scheme() == origin_scheme
                    && effective_url.port_or_known_default() == origin_port
                {
                    prefix = path_scope_prefix(effective_url.path());
                    allow_all = prefix.is_empty() || prefix == "/";
                }
            }
        }
        let mut next_urls = Vec::new();
        let is_html = content_type.contains("text/html")
            || content_type.contains("application/xhtml")
            || looks_like_html(&html);
        let link_candidates = if is_html {
            extract_links(&html)
        } else {
            extract_markdown_links(&html)
        };
        for link in link_candidates {
            if link.is_empty()
                || link.starts_with('#')
                || link.starts_with("mailto:")
                || link.starts_with("javascript:")
                || link.starts_with("tel:")
            {
                continue;
            }
            let resolved = match effective_url.join(&link) {
                Ok(u) => u,
                Err(_) => continue,
            };
            if resolved.scheme() != "http" && resolved.scheme() != "https" {
                continue;
            }
            let Some(normalized) = normalize_candidate(
                resolved,
                &origin_scheme,
                &origin_host,
                origin_port,
                &prefix,
                allow_all,
            ) else {
                continue;
            };
            let key = normalized.to_string();
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            next_urls.push(normalized);
        }
        next_urls.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for next in next_urls {
            if results.len() + queue.len() >= max_pages {
                break;
            }
            queue.push_back(next);
        }
        throttle(cfg).await;
    }

    Ok(results)
}

fn normalize_candidate(
    url: Url,
    origin_scheme: &str,
    origin_host: &str,
    origin_port: Option<u16>,
    prefix: &str,
    allow_all: bool,
) -> Option<Url> {
    if url.scheme() != origin_scheme {
        return None;
    }
    if url.host_str() != Some(origin_host) {
        return None;
    }
    if url.port_or_known_default() != origin_port {
        return None;
    }
    if !allow_all && !path_in_scope(url.path(), prefix) {
        return None;
    }
    if is_asset_path(url.path()) {
        return None;
    }
    Some(normalize_url(url))
}

pub(crate) fn looks_like_html(text: &str) -> bool {
    let trimmed = text.trim_start().to_ascii_lowercase();
    trimmed.starts_with("<!doctype html") || trimmed.starts_with("<html")
}

fn normalize_url(mut url: Url) -> Url {
    url.set_fragment(None);
    url.set_query(None);
    let trimmed = url.path().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        url.set_path("/");
    } else {
        url.set_path(&trimmed);
    }
    url
}

fn path_in_scope(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() || prefix == "/" {
        return true;
    }
    if path == prefix {
        return true;
    }
    path.starts_with(prefix) && path[prefix.len()..].starts_with('/')
}

pub(crate) fn path_scope_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let ext = Path::new(trimmed)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !ext.is_empty() {
        if let Some((dir, _)) = trimmed.rsplit_once('/') {
            if dir.is_empty() {
                return "/".to_string();
            }
            return dir.to_string();
        }
    }
    trimmed.to_string()
}

fn is_asset_path(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return false;
    }
    matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
            | "css"
            | "js"
            | "json"
            | "map"
            | "ico"
            | "webmanifest"
            | "xml"
            | "txt"
            | "pdf"
            | "zip"
            | "tar"
            | "gz"
            | "bz2"
            | "xz"
            | "7z"
            | "mp4"
            | "mp3"
            | "wav"
            | "avi"
            | "mov"
            | "mkv"
    )
}

fn extract_links(html: &str) -> Vec<String> {
    let href_re = match Regex::new("href=[\"']([^\"']+)[\"']") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile href regex: {err}");
            return Vec::new();
        }
    };
    let mut links = Vec::new();
    for cap in href_re.captures_iter(html) {
        links.push(cap[1].to_string());
    }
    links
}

fn extract_markdown_links(text: &str) -> Vec<String> {
    let inline_re = match Regex::new(r"\[[^\]]+\]\(([^)\s]+)") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile markdown inline link regex: {err}");
            return Vec::new();
        }
    };
    let ref_re = match Regex::new(r"(?m)^\s*\[[^\]]+\]:\s*(\S+)") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile markdown ref link regex: {err}");
            return Vec::new();
        }
    };
    let auto_re = match Regex::new(r"<(https?://[^>]+)>") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile markdown autolink regex: {err}");
            return Vec::new();
        }
    };
    let mut links = Vec::new();
    for cap in inline_re.captures_iter(text) {
        links.push(cap[1].to_string());
    }
    for cap in ref_re.captures_iter(text) {
        links.push(cap[1].to_string());
    }
    for cap in auto_re.captures_iter(text) {
        links.push(cap[1].to_string());
    }
    links.sort();
    links.dedup();
    links
}

async fn try_playwright_links(docs_url: &str) -> Result<Option<Vec<String>>> {
    let Some(root) = find_playwright_root() else {
        return Ok(None);
    };
    let script = r#"
const { createRequire } = require('module');
const req = createRequire(process.cwd() + '/');
const { chromium } = req('playwright');
const url = process.argv[2];
(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForTimeout(1000);
  const links = await page.$$eval('a[href]', els => els.map(el => el.getAttribute('href')).filter(Boolean));
  console.log(JSON.stringify(links));
  await browser.close();
})().catch(err => {
  console.error(String(err));
  process.exit(1);
});
"#;
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(script.as_bytes())?;
    let output = Command::new("node")
        .arg(tmp.path())
        .arg(docs_url)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            return Err(anyhow!("playwright crawl failed"));
        }
        return Err(anyhow!("playwright crawl failed: {msg}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut links: Vec<String> = serde_json::from_str(&stdout).unwrap_or_default();
    links.retain(|link| !link.trim().is_empty());
    links.sort();
    links.dedup();
    Ok(Some(links))
}

fn find_playwright_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("apps/web/node_modules/playwright");
        if candidate.exists() {
            return Some(dir.join("apps/web"));
        }
        let candidate = dir.join("core/apps/web/node_modules/playwright");
        if candidate.exists() {
            return Some(dir.join("core/apps/web"));
        }
        let candidate = dir.join("node_modules/playwright");
        if candidate.exists() {
            return Some(dir.clone());
        }
        if !dir.pop() {
            break;
        }
    }
    None
}
