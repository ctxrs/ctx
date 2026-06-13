use std::io::Read;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use regex::Regex;
use url::Url;

use crate::config::{apply_max, filter_urls, throttle, DocsMirrorConfig};
use crate::crawler::path_scope_prefix;
use crate::http::{fetch_bytes, fetch_text};

pub(crate) async fn try_sitemap_pages(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    base_url: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut warnings = Vec::new();
    let base = Url::parse(base_url).context("parsing docs_url")?;
    let origin = base.join("/").context("building origin url")?;
    let mut sitemap_candidates = Vec::new();
    if let Some(url) = cfg.sitemap_url.as_deref() {
        sitemap_candidates.push(url.to_string());
    } else {
        if let Ok(url) = base.join("sitemap.xml") {
            sitemap_candidates.push(url.to_string());
        }
        if let Ok(url) = base.join("sitemap.xml.gz") {
            sitemap_candidates.push(url.to_string());
        }
        if let Ok(url) = base.join("sitemap-index.xml") {
            sitemap_candidates.push(url.to_string());
        }
        if let Ok(url) = origin.join("sitemap.xml") {
            sitemap_candidates.push(url.to_string());
        }
        if let Ok(url) = origin.join("sitemap.xml.gz") {
            sitemap_candidates.push(url.to_string());
        }
        if let Ok(url) = origin.join("sitemap-index.xml") {
            sitemap_candidates.push(url.to_string());
        }
    }

    let mut locs = Vec::new();
    for sitemap_url in sitemap_candidates {
        match fetch_sitemap_text(client, &sitemap_url).await {
            Ok(xml) => {
                let parsed = parse_sitemap_locs(&xml);
                if parsed.is_empty() {
                    warnings.push(format!("sitemap {sitemap_url} returned no locs"));
                    continue;
                }
                locs = parsed;
                break;
            }
            Err(err) => {
                warnings.push(format!("sitemap fetch failed for {sitemap_url}: {err}"));
            }
        }
    }

    if locs.is_empty() && cfg.sitemap_url.is_none() {
        let mut robots_sources = Vec::new();
        if let Ok(robots_url) = base.join("robots.txt") {
            robots_sources.push(robots_url);
        }
        if base != origin {
            if let Ok(robots_url) = origin.join("robots.txt") {
                robots_sources.push(robots_url);
            }
        }
        let mut found = false;
        for robots_url in robots_sources {
            match fetch_text(client, robots_url.as_str()).await {
                Ok(text) => {
                    let robots_sitemaps = parse_robots_sitemaps(&text);
                    if !robots_sitemaps.is_empty() {
                        locs = robots_sitemaps;
                        found = true;
                        break;
                    }
                }
                Err(err) => {
                    warnings.push(format!("robots.txt fetch failed for {robots_url}: {err}"));
                }
            }
        }
        if !found {
            warnings.push("robots.txt had no sitemap entries".to_string());
        }
    }

    let mut pages = Vec::new();
    if locs.iter().all(|loc| is_sitemap_url(loc)) {
        for loc in locs {
            let xml = fetch_sitemap_text(client, &loc).await?;
            let inner = parse_sitemap_locs(&xml);
            pages.extend(inner);
            throttle(cfg).await;
        }
    } else {
        pages = locs;
    }

    let docs_prefix = path_scope_prefix(base.path());
    let pages = if !docs_prefix.is_empty() && docs_prefix != "/" {
        pages
            .into_iter()
            .filter(|url| {
                Url::parse(url)
                    .ok()
                    .map(|u| u.path().starts_with(&docs_prefix))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        pages
    };
    let pages = filter_urls(cfg, pages);
    let pages = apply_max(cfg, pages);
    if pages.is_empty() {
        warnings.push("sitemap returned no pages after filtering".to_string());
    }
    Ok((pages, warnings))
}

async fn fetch_sitemap_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let bytes = fetch_bytes(client, url).await?;
    if url.ends_with(".gz") {
        let mut decoder = GzDecoder::new(bytes.as_slice());
        let mut out = String::new();
        decoder
            .read_to_string(&mut out)
            .with_context(|| format!("decoding gzip sitemap {url}"))?;
        Ok(out)
    } else {
        String::from_utf8(bytes).with_context(|| format!("decoding sitemap {url}"))
    }
}

fn parse_sitemap_locs(xml: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let re = match Regex::new(r"<loc>([^<]+)</loc>") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile sitemap <loc> regex: {err}");
            return Vec::new();
        }
    };
    for cap in re.captures_iter(xml) {
        urls.push(cap[1].trim().to_string());
    }
    urls
}

fn parse_robots_sitemaps(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let re = match Regex::new(r"(?i)^sitemap:\s*(\S+)") {
        Ok(re) => re,
        Err(err) => {
            eprintln!("failed to compile robots sitemap regex: {err}");
            return Vec::new();
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(cap) = re.captures(line) {
            urls.push(cap[1].trim().to_string());
        }
    }
    urls.sort();
    urls.dedup();
    urls
}

fn is_sitemap_url(url: &str) -> bool {
    let url = url.split('#').next().unwrap_or(url);
    url.ends_with(".xml") || url.ends_with(".xml.gz")
}
