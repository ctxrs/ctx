use std::path::PathBuf;

use anyhow::{anyhow, Result};
use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use tempfile::TempDir;
use url::Url;

use crate::config::{
    apply_max, derive_output_path, filter_urls, resolve_docs_url, resolve_repo_url, throttle,
    DocsMirrorConfig, Strategy,
};
use crate::crawler::{crawl_html_pages, crawl_html_pages_rendered, looks_like_html};
use crate::http::send_with_retries;
use crate::repo::{infer_repo_hint_from_docs_url, mirror_repo_plan};
use crate::sitemap::try_sitemap_pages;

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MirrorMethod {
    Llms,
    Repo,
    SitemapMd,
    EditLink,
    Html,
}

#[derive(Debug, Serialize)]
pub(crate) struct MirrorManifest {
    pub(crate) version: u32,
    pub(crate) generated_at: String,
    pub(crate) method: MirrorMethod,
    pub(crate) source: String,
    pub(crate) title: Option<String>,
    pub(crate) docs_url: Option<String>,
    pub(crate) repo_url: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) pages: Vec<ManifestPage>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ManifestPage {
    pub(crate) path: String,
    pub(crate) url: Option<String>,
}

pub(crate) struct MirrorPlan {
    pub(crate) method: MirrorMethod,
    pub(crate) docs_url: Option<String>,
    pub(crate) repo_url: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) pages: Vec<PageRef>,
    pub(crate) warnings: Vec<String>,
    pub(crate) repo_root: Option<PathBuf>,
    pub(crate) repo_temp: Option<TempDir>,
}

#[derive(Debug)]
pub(crate) struct PageRef {
    pub(crate) path: PathBuf,
    pub(crate) url: Option<String>,
}

pub(crate) async fn resolve_plan(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
) -> Result<MirrorPlan> {
    let strategy = cfg.strategy.unwrap_or(Strategy::Auto);
    let mut warnings = Vec::new();

    let docs_url = resolve_docs_url(cfg);
    let repo_url = resolve_repo_url(cfg);
    let repo_hint = docs_url.as_deref().and_then(infer_repo_hint_from_docs_url);

    let min_pages = cfg.min_pages.unwrap_or(10);
    let mut candidate: Option<(MirrorMethod, Vec<PageRef>)> = None;

    if matches!(strategy, Strategy::Auto | Strategy::Llms) {
        if let Some(urls) = try_llms_urls(client, cfg, docs_url.as_deref()).await? {
            let page_refs = urls
                .into_iter()
                .map(|url| {
                    let path = derive_output_path(cfg, docs_url.as_deref(), &url)?;
                    Ok(PageRef {
                        path,
                        url: Some(url),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if matches!(strategy, Strategy::Llms) {
                return Ok(MirrorPlan {
                    method: MirrorMethod::Llms,
                    docs_url: docs_url.clone(),
                    repo_url: None,
                    revision: cfg.revision.clone(),
                    pages: page_refs,
                    warnings,
                    repo_root: None,
                    repo_temp: None,
                });
            }
            candidate = Some((MirrorMethod::Llms, page_refs));
        }
        if matches!(strategy, Strategy::Llms) {
            return Err(anyhow!("llms.txt strategy failed"));
        }
    }

    if matches!(strategy, Strategy::Auto | Strategy::Repo) {
        if let Some(repo_url) = repo_url.clone() {
            let repo_plan = mirror_repo_plan(cfg, &repo_url, None)?;
            return Ok(MirrorPlan {
                method: MirrorMethod::Repo,
                docs_url: docs_url.clone(),
                repo_url: Some(repo_url),
                revision: cfg.revision.clone(),
                pages: repo_plan.pages,
                warnings,
                repo_root: Some(repo_plan.docs_root),
                repo_temp: Some(repo_plan.temp_dir),
            });
        } else if let Some(hint) = repo_hint.clone() {
            let repo_plan = mirror_repo_plan(cfg, &hint.repo_url, hint.subpath.as_deref())?;
            return Ok(MirrorPlan {
                method: MirrorMethod::Repo,
                docs_url: docs_url.clone(),
                repo_url: Some(hint.repo_url),
                revision: cfg.revision.clone(),
                pages: repo_plan.pages,
                warnings,
                repo_root: Some(repo_plan.docs_root),
                repo_temp: Some(repo_plan.temp_dir),
            });
        }
        if matches!(strategy, Strategy::Repo) {
            return Err(anyhow!("repo strategy requires repo_url or git source"));
        }
    }

    let (pages_from_sitemap, sitemap_warnings) = if matches!(
        strategy,
        Strategy::Auto | Strategy::Sitemap | Strategy::EditLink | Strategy::Html
    ) {
        if let Some(base_url) = docs_url.as_deref() {
            match try_sitemap_pages(client, cfg, base_url).await {
                Ok((pages, warns)) => (Some(pages), warns),
                Err(err) => {
                    warnings.push(format!("sitemap lookup failed: {err}"));
                    (None, vec![])
                }
            }
        } else {
            (None, vec!["docs_url missing".to_string()])
        }
    } else {
        (None, vec![])
    };

    warnings.extend(sitemap_warnings);

    if matches!(strategy, Strategy::Auto | Strategy::Sitemap) {
        if let Some(pages) = pages_from_sitemap.clone() {
            if let Some(md_urls) = try_sitemap_md_urls(client, cfg, &pages).await? {
                let page_refs = md_urls
                    .into_iter()
                    .map(|url| {
                        let path = derive_output_path(cfg, docs_url.as_deref(), &url)?;
                        Ok(PageRef {
                            path,
                            url: Some(url),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if matches!(strategy, Strategy::Sitemap) {
                    return Ok(MirrorPlan {
                        method: MirrorMethod::SitemapMd,
                        docs_url: docs_url.clone(),
                        repo_url: None,
                        revision: cfg.revision.clone(),
                        pages: page_refs,
                        warnings,
                        repo_root: None,
                        repo_temp: None,
                    });
                }
                candidate = Some((MirrorMethod::SitemapMd, page_refs));
            }
        }
        if matches!(strategy, Strategy::Sitemap) {
            return Err(anyhow!(
                "sitemap strategy failed to find markdown endpoints"
            ));
        }
    }

    if matches!(strategy, Strategy::Auto | Strategy::EditLink) {
        if let Some(pages) = pages_from_sitemap.clone() {
            if let Some(raw_urls) = try_edit_link_urls(client, cfg, &pages).await? {
                let page_refs = raw_urls
                    .into_iter()
                    .map(|url| {
                        let path = derive_output_path(cfg, docs_url.as_deref(), &url)?;
                        Ok(PageRef {
                            path,
                            url: Some(url),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if matches!(strategy, Strategy::EditLink) {
                    return Ok(MirrorPlan {
                        method: MirrorMethod::EditLink,
                        docs_url: docs_url.clone(),
                        repo_url: None,
                        revision: cfg.revision.clone(),
                        pages: page_refs,
                        warnings,
                        repo_root: None,
                        repo_temp: None,
                    });
                }
                let replace = candidate
                    .as_ref()
                    .map(|(_, pages)| page_refs.len() > pages.len())
                    .unwrap_or(true);
                if replace {
                    candidate = Some((MirrorMethod::EditLink, page_refs));
                }
            }
        }
        if matches!(strategy, Strategy::EditLink) {
            return Err(anyhow!("edit_link strategy failed"));
        }
    }

    if matches!(strategy, Strategy::Auto | Strategy::Html) {
        if matches!(strategy, Strategy::Html) {
            let base_url = docs_url
                .clone()
                .ok_or_else(|| anyhow!("html fallback requires docs_url"))?;
            let mut pages = crawl_html_pages(client, cfg, &base_url).await?;
            if pages.len() <= min_pages {
                match crawl_html_pages_rendered(client, cfg, &base_url).await {
                    Ok(Some(rendered)) => {
                        if rendered.len() > pages.len() {
                            pages = rendered;
                            warnings.push("html crawl used rendered links".to_string());
                        }
                    }
                    Ok(None) => {
                        warnings.push("playwright not available for rendered crawl".to_string());
                    }
                    Err(err) => {
                        warnings.push(format!("rendered crawl failed: {err}"));
                    }
                }
            }
            if pages.is_empty() {
                warnings.push("html crawl found no pages; using docs_url only".to_string());
                pages.push(base_url.clone());
            }
            let page_refs = pages
                .into_iter()
                .map(|url| {
                    let path = derive_output_path(cfg, docs_url.as_deref(), &url)?;
                    Ok(PageRef {
                        path,
                        url: Some(url),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(MirrorPlan {
                method: MirrorMethod::Html,
                docs_url,
                repo_url: None,
                revision: cfg.revision.clone(),
                pages: page_refs,
                warnings,
                repo_root: None,
                repo_temp: None,
            });
        }

        let candidate_count = candidate
            .as_ref()
            .map(|(_, pages)| pages.len())
            .unwrap_or(0);
        let html_threshold = match candidate.as_ref().map(|(method, _)| *method) {
            Some(MirrorMethod::EditLink) => min_pages.max(20),
            _ => min_pages,
        };
        let should_consider_html = candidate.is_none() || candidate_count <= html_threshold;
        if should_consider_html {
            let mut pages = pages_from_sitemap.clone().unwrap_or_default();
            if pages.len() <= min_pages {
                let base_url = docs_url
                    .clone()
                    .ok_or_else(|| anyhow!("html fallback requires docs_url"))?;
                let crawled = crawl_html_pages(client, cfg, &base_url).await?;
                if crawled.len() > pages.len() {
                    pages = crawled;
                }
                if pages.len() <= min_pages {
                    match crawl_html_pages_rendered(client, cfg, &base_url).await {
                        Ok(Some(rendered)) => {
                            if rendered.len() > pages.len() {
                                pages = rendered;
                                warnings.push("html crawl used rendered links".to_string());
                            }
                        }
                        Ok(None) => {
                            warnings
                                .push("playwright not available for rendered crawl".to_string());
                        }
                        Err(err) => {
                            warnings.push(format!("rendered crawl failed: {err}"));
                        }
                    }
                }
            }
            if pages.is_empty() {
                if let Some(base_url) = docs_url.clone() {
                    warnings.push("html crawl found no pages; using docs_url only".to_string());
                    pages.push(base_url);
                }
            }
            let page_refs = pages
                .into_iter()
                .map(|url| {
                    let path = derive_output_path(cfg, docs_url.as_deref(), &url)?;
                    Ok(PageRef {
                        path,
                        url: Some(url),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if page_refs.len() > candidate_count {
                candidate = Some((MirrorMethod::Html, page_refs));
            }
        }
    }

    if let Some((method, pages)) = candidate {
        return Ok(MirrorPlan {
            method,
            docs_url,
            repo_url: None,
            revision: cfg.revision.clone(),
            pages,
            warnings,
            repo_root: None,
            repo_temp: None,
        });
    }

    Err(anyhow!("no mirror strategy succeeded"))
}

pub(crate) fn manifest_from_plan(cfg: &DocsMirrorConfig, plan: &MirrorPlan) -> MirrorManifest {
    let pages = plan
        .pages
        .iter()
        .map(|page| ManifestPage {
            path: page.path.to_string_lossy().to_string(),
            url: page.url.clone(),
        })
        .collect::<Vec<_>>();

    MirrorManifest {
        version: 1,
        generated_at: Utc::now().to_rfc3339(),
        method: plan.method,
        source: cfg.source.clone(),
        title: cfg.title.clone(),
        docs_url: plan.docs_url.clone(),
        repo_url: plan.repo_url.clone(),
        revision: plan.revision.clone(),
        pages,
        warnings: plan.warnings.clone(),
    }
}

async fn try_llms_urls(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    docs_url: Option<&str>,
) -> Result<Option<Vec<String>>> {
    let mut candidates = Vec::new();
    if let Some(url) = cfg.llms_url.as_deref() {
        candidates.push(url.to_string());
    } else if let Some(base) = docs_url {
        if let Ok(base_url) = Url::parse(base) {
            if let Ok(url) = base_url.join("llms.txt") {
                candidates.push(url.to_string());
            }
            if let Ok(origin) = base_url.join("/") {
                if let Ok(url) = origin.join("llms.txt") {
                    candidates.push(url.to_string());
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();

    let url_re = match Regex::new("https?://[^\\s)\\\"'>]+") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile llms url regex: {err}");
            return Ok(None);
        }
    };
    for llms_url in candidates {
        let text = match crate::http::fetch_text(client, &llms_url).await {
            Ok(txt) => txt,
            Err(_) => continue,
        };
        if looks_like_html(&text) {
            continue;
        }

        let mut urls: Vec<String> = url_re
            .find_iter(&text)
            .map(|m| m.as_str().to_string())
            .collect();
        urls.retain(|url| Url::parse(url).is_ok());
        urls.sort();
        urls.dedup();

        let urls = filter_urls(cfg, urls);
        let urls = apply_max(cfg, urls);
        if !urls.is_empty() {
            return Ok(Some(urls));
        }
    }

    Ok(None)
}

async fn try_sitemap_md_urls(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    pages: &[String],
) -> Result<Option<Vec<String>>> {
    let mut md_urls = Vec::new();
    for page in pages {
        let page = page.split('#').next().unwrap_or(page).trim().to_string();
        let mut parsed = match Url::parse(&page) {
            Ok(url) => url,
            Err(_) => continue,
        };
        let path = parsed.path();
        if path.is_empty() || path == "/" {
            continue;
        }
        parsed.set_query(None);
        parsed.set_fragment(None);
        let mut url = parsed.to_string();
        if url.ends_with('/') {
            url.pop();
        }
        if !url.ends_with(".md") && !url.ends_with(".mdx") {
            url.push_str(".md");
        }
        if send_with_retries(client, &url).await.is_ok() {
            md_urls.push(url);
        }
        throttle(cfg).await;
    }
    md_urls.sort();
    md_urls.dedup();
    if md_urls.is_empty() {
        return Ok(None);
    }
    Ok(Some(md_urls))
}

async fn try_edit_link_urls(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    pages: &[String],
) -> Result<Option<Vec<String>>> {
    let mut raw_urls = Vec::new();
    for page in pages {
        let html = match crate::http::fetch_text(client, page).await {
            Ok(txt) => txt,
            Err(_) => continue,
        };
        if let Some(raw) = extract_raw_url_from_edit_link(&html) {
            raw_urls.push(raw);
        }
        throttle(cfg).await;
    }
    raw_urls.sort();
    raw_urls.dedup();
    if raw_urls.is_empty() {
        return Ok(None);
    }
    let raw_urls = apply_max(cfg, raw_urls);
    Ok(Some(raw_urls))
}

fn extract_raw_url_from_edit_link(html: &str) -> Option<String> {
    let href_re = match Regex::new("href=\"([^\"]+)\"") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile edit link regex: {err}");
            return None;
        }
    };
    let mut links = Vec::new();
    for cap in href_re.captures_iter(html) {
        links.push(cap[1].to_string());
    }

    let candidates: Vec<String> = links
        .iter()
        .filter(|href| href.contains("github.com") || href.contains("gitlab.com"))
        .cloned()
        .collect();

    for href in candidates {
        if let Some(raw) = github_edit_to_raw(&href).or_else(|| gitlab_edit_to_raw(&href)) {
            return Some(raw);
        }
    }
    None
}

fn github_edit_to_raw(href: &str) -> Option<String> {
    let href = href.split('#').next().unwrap_or(href);
    let url = Url::parse(href).ok()?;
    if url.domain()? != "github.com" {
        return None;
    }
    let path = url.path().trim_start_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    let org = parts[0];
    let repo = parts[1];
    let mode = parts[2];
    if mode != "edit" && mode != "blob" {
        return None;
    }
    let branch = parts[3];
    let file_path = parts[4..].join("/");
    let raw = format!("https://raw.githubusercontent.com/{org}/{repo}/{branch}/{file_path}");
    Some(raw)
}

fn gitlab_edit_to_raw(href: &str) -> Option<String> {
    let href = href.split('#').next().unwrap_or(href);
    let url = Url::parse(href).ok()?;
    if url.domain()? != "gitlab.com" {
        return None;
    }
    let path = url.path().trim_start_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 6 {
        return None;
    }
    let org = parts[0];
    let repo = parts[1];
    if parts[2] != "-" || parts[3] != "edit" {
        return None;
    }
    let branch = parts[4];
    let file_path = parts[5..].join("/");
    let raw = format!("https://gitlab.com/{org}/{repo}/-/raw/{branch}/{file_path}");
    Some(raw)
}
