use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};

pub fn post_json(endpoint: &str, body: &[u8]) -> Result<()> {
    if let Some(path) = file_url_path(endpoint)? {
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

pub fn get_bytes_limited(endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
    if let Some(path) = file_url_path(endpoint)? {
        let file = fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
        return read_limited(file, max_bytes, &format!("read {}", path.display()));
    }
    require_https_or_localhost(endpoint)?;
    let response = ureq::get(endpoint)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|err| anyhow!("GET {endpoint}: {err}"))?;
    read_limited(
        response.into_reader(),
        max_bytes,
        &format!("GET {endpoint}"),
    )
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn get_to_writer_limited(
    endpoint: &str,
    max_bytes: u64,
    timeout: Duration,
    writer: &mut impl Write,
) -> Result<u64> {
    let started = Instant::now();
    if let Some(path) = file_url_path(endpoint)? {
        let file = fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
        return copy_limited(
            file,
            writer,
            max_bytes,
            timeout,
            started,
            "read local artifact",
        );
    }
    require_https_or_localhost(endpoint)?;
    let response = ureq::get(endpoint)
        .timeout(timeout)
        .call()
        .map_err(|err| anyhow!("GET artifact: {err}"))?;
    if response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes)
    {
        return Err(anyhow!("GET artifact exceeds max bytes ({max_bytes})"));
    }
    copy_limited(
        response.into_reader(),
        writer,
        max_bytes,
        timeout,
        started,
        "GET artifact",
    )
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn copy_limited(
    mut reader: impl Read,
    writer: &mut impl Write,
    max_bytes: u64,
    timeout: Duration,
    started: Instant,
    label: &str,
) -> Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("{label} exceeded time limit"));
        }
        let count = reader
            .read(&mut buffer)
            .with_context(|| format!("{label}: read response"))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("{label} size overflow"))?;
        if total > max_bytes {
            return Err(anyhow!("{label} exceeds max bytes ({max_bytes})"));
        }
        writer
            .write_all(&buffer[..count])
            .with_context(|| format!("{label}: write destination"))?;
    }
    Ok(total)
}

pub(crate) fn read_limited(
    mut reader: impl Read,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| anyhow!("{label}: {err}"))?;
    if bytes.len() > max_bytes {
        return Err(anyhow!("{label} exceeds max bytes ({max_bytes})"));
    }
    Ok(bytes)
}

pub(crate) fn file_url_path(url: &str) -> Result<Option<PathBuf>> {
    let Some(path) = url.strip_prefix("file://") else {
        return Ok(None);
    };
    if path.is_empty() || !path.starts_with('/') {
        return Err(anyhow!("file URL must use an absolute local path: {url}"));
    }
    Ok(Some(PathBuf::from(path)))
}

pub(crate) fn require_https_or_localhost(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or_default();
        if is_localhost_authority(host) {
            return Ok(());
        }
    }
    Err(anyhow!("refusing non-HTTPS endpoint: {url}"))
}

pub(crate) fn is_localhost_authority(authority: &str) -> bool {
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(rest) = authority.strip_prefix("[::1]") {
        if rest.is_empty() || rest.starts_with(':') {
            "[::1]"
        } else {
            return false;
        }
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_must_be_absolute_local_paths() {
        assert_eq!(
            file_url_path("file:///tmp/ctx-release-metadata.env")
                .unwrap()
                .unwrap(),
            PathBuf::from("/tmp/ctx-release-metadata.env")
        );
        assert!(file_url_path("file://relative/path").is_err());
        assert!(file_url_path("file://").is_err());
        assert!(file_url_path("https://example.com").unwrap().is_none());
    }

    #[test]
    fn endpoint_validation_allows_https_and_localhost_http_only() {
        require_https_or_localhost("https://example.com/releases").unwrap();
        require_https_or_localhost("http://localhost:8080/events").unwrap();
        require_https_or_localhost("http://127.0.0.1/events").unwrap();
        require_https_or_localhost("http://[::1]:8080/events").unwrap();
        assert!(require_https_or_localhost("http://example.com/events").is_err());
        assert!(require_https_or_localhost("http://example.com@localhost/events").is_err());
    }

    #[test]
    fn get_bytes_limited_rejects_oversized_file_urls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized.bin");
        fs::write(&path, b"12345").unwrap();
        let err = get_bytes_limited(&format!("file://{}", path.display()), 4).unwrap_err();
        assert!(err.to_string().contains("exceeds max bytes (4)"));
    }

    #[test]
    fn streaming_get_enforces_compressed_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"12345").unwrap();
        let mut output = Vec::new();
        let error = get_to_writer_limited(
            &format!("file://{}", path.display()),
            4,
            Duration::from_secs(1),
            &mut output,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds max bytes"));
    }
}
