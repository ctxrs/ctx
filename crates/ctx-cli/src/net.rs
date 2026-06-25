use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Context, Result};

pub fn get_bytes(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = file_url_path(url) {
        return fs::read(&path).with_context(|| format!("read {}", path.display()));
    }
    require_https_or_localhost(url)?;
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(4))
        .call()
        .map_err(|err| anyhow!("GET {url}: {err}"))?;
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .with_context(|| format!("read response body from {url}"))?;
    Ok(bytes)
}

pub fn post_json(endpoint: &str, body: &[u8]) -> Result<()> {
    if let Some(path) = file_url_path(endpoint) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.write_all(body)?;
        file.write_all(b"\n")?;
        return Ok(());
    }
    require_https_or_localhost(endpoint)?;
    ureq::post(endpoint)
        .timeout(std::time::Duration::from_secs(2))
        .set("content-type", "application/json")
        .send_bytes(body)
        .map(|_| ())
        .map_err(|err| anyhow!("POST {endpoint}: {err}"))
}

fn file_url_path(url: &str) -> Option<PathBuf> {
    url.strip_prefix("file://").map(PathBuf::from)
}

fn require_https_or_localhost(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or_default();
        if matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Ok(());
        }
    }
    Err(anyhow!("refusing non-HTTPS endpoint: {url}"))
}
