use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::header;
use reqwest::StatusCode;
use tokio::time::sleep;

const RETRY_MAX_ATTEMPTS: usize = 3;
const RETRY_BASE_MS: u64 = 500;
const RETRY_MAX_MS: u64 = 5_000;

pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("ctx-docs-mirror/0.1")
        .build()?)
}

pub(crate) async fn send_with_retries(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if !should_retry(status) || attempt >= RETRY_MAX_ATTEMPTS {
            return Err(anyhow!("HTTP status {status} for url ({url})"));
        }
        let delay = retry_delay(&resp, attempt);
        sleep(delay).await;
    }
}

fn should_retry(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

fn retry_delay(resp: &reqwest::Response, attempt: usize) -> Duration {
    if let Some(value) = resp.headers().get(header::RETRY_AFTER) {
        if let Ok(text) = value.to_str() {
            if let Ok(seconds) = text.trim().parse::<u64>() {
                return Duration::from_secs(seconds);
            }
        }
    }
    let shift = attempt.saturating_sub(1) as u32;
    let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RETRY_BASE_MS.saturating_mul(multiplier);
    Duration::from_millis(backoff.min(RETRY_MAX_MS))
}

pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = send_with_retries(client, url).await?;
    Ok(resp.text().await?)
}

pub(crate) async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = send_with_retries(client, url).await?;
    Ok(resp.bytes().await?.to_vec())
}
