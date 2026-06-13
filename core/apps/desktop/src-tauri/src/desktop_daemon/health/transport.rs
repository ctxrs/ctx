use super::*;
#[cfg(test)]
use std::cell::Cell;

type DaemonHealthClientCache =
    std::sync::Mutex<std::collections::HashMap<u64, reqwest::blocking::Client>>;

fn daemon_health_clients() -> &'static DaemonHealthClientCache {
    static DAEMON_HEALTH_CLIENTS: std::sync::OnceLock<DaemonHealthClientCache> =
        std::sync::OnceLock::new();
    DAEMON_HEALTH_CLIENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn daemon_health_with_auth(
    base_url: &str,
    auth_token: Option<&str>,
) -> Result<DaemonHealthSummary> {
    daemon_health_with_timeout_auth(base_url, auth_token, daemon_health_timeout())
}

#[cfg(test)]
thread_local! {
    static DAEMON_HEALTH_CLIENT_BUILD_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_daemon_health_client_build_count() {
    if let Ok(mut clients) = daemon_health_clients().lock() {
        clients.clear();
    }
    DAEMON_HEALTH_CLIENT_BUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn daemon_health_client_build_count() -> usize {
    DAEMON_HEALTH_CLIENT_BUILD_COUNT.with(Cell::get)
}

fn daemon_health_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    let timeout_key = timeout.as_millis() as u64;
    let mut guard = daemon_health_clients()
        .lock()
        .map_err(|err| anyhow!("daemon health client cache poisoned: {err}"))?;
    if let Some(existing) = guard.get(&timeout_key) {
        return Ok(existing.clone());
    }
    #[cfg(test)]
    DAEMON_HEALTH_CLIENT_BUILD_COUNT.with(|count| count.set(count.get() + 1));
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("building http client")?;
    guard.insert(timeout_key, client.clone());
    Ok(client)
}

fn validate_authenticated_daemon_session(
    client: &reqwest::blocking::Client,
    base_url: &str,
    auth_token: Option<&str>,
) -> Result<()> {
    let Some(token) = auth_token.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    let url = format!("{}/api/workspaces", base_url.trim_end_matches('/'));
    let res = client
        .get(url)
        .bearer_auth(token)
        .send()
        .context("requesting authenticated /api/workspaces")?;
    let _ = res
        .error_for_status()
        .context("authenticated daemon session status")?;
    Ok(())
}

#[cfg(test)]
pub(super) fn daemon_health_with_timeout(
    base_url: &str,
    timeout: Duration,
) -> Result<DaemonHealthSummary> {
    daemon_health_with_timeout_auth(base_url, None, timeout)
}

pub(super) fn daemon_health_with_timeout_auth(
    base_url: &str,
    auth_token: Option<&str>,
    timeout: Duration,
) -> Result<DaemonHealthSummary> {
    let url = format!("{}/api/health", base_url.trim_end_matches('/'));
    let client = daemon_health_client(timeout)?;
    let request = client.get(url);
    let request = match auth_token {
        Some(token) if !token.trim().is_empty() => request.bearer_auth(token),
        _ => request,
    };
    let res = request.send().context("requesting /api/health")?;
    let res = res.error_for_status().context("health status")?;
    let health = res
        .json::<DaemonHealthSummary>()
        .context("parsing /api/health response")?;
    validate_authenticated_daemon_session(&client, base_url, auth_token)?;
    Ok(health)
}

fn daemon_health_timeout() -> Duration {
    const DEFAULT_MS: u64 = 5000;
    const MIN_MS: u64 = 100;
    const MAX_MS: u64 = 30000;
    let raw = std::env::var("CTX_DESKTOP_DAEMON_HEALTH_TIMEOUT_MS").unwrap_or_default();
    let parsed = raw.trim().parse::<u64>().ok().unwrap_or(DEFAULT_MS);
    let bounded = parsed.clamp(MIN_MS, MAX_MS);
    Duration::from_millis(bounded)
}
