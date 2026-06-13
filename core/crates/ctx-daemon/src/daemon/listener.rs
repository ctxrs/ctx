use std::collections::HashSet;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use url::Url;

pub(super) struct BoundDaemonListeners {
    pub requested_binds: Vec<String>,
    pub listeners: Vec<TcpListener>,
    pub daemon_url: String,
}

fn default_binds() -> Vec<String> {
    let binds = vec!["127.0.0.1:4399".to_string()];
    #[cfg(target_os = "macos")]
    let binds = {
        let mut binds = binds;
        binds.push(format!(
            "{}:4399",
            ctx_workspace_container::AVF_GUEST_HOST_GATEWAY
        ));
        binds
    };
    binds
}

fn optional_default_bind() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some(format!(
            "{}:4399",
            ctx_workspace_container::AVF_GUEST_HOST_GATEWAY
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Option::<String>::None
    }
}

fn requested_binds(bind: Vec<String>) -> Vec<String> {
    let default_binds = default_binds();
    if bind.is_empty() {
        return default_binds;
    }
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for entry in bind {
        let trimmed = entry.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        values.push(trimmed.to_string());
    }
    if values.is_empty() {
        default_binds
    } else {
        values
    }
}

fn daemon_url_from_listeners(listeners: &[TcpListener]) -> Result<String> {
    let primary_addr = listeners
        .iter()
        .find_map(|listener| {
            listener
                .local_addr()
                .ok()
                .filter(|addr| addr.ip().is_loopback())
        })
        .or_else(|| {
            listeners
                .first()
                .and_then(|listener| listener.local_addr().ok())
        })
        .context("daemon started without any bound listeners")?;
    let host = match primary_addr.ip() {
        std::net::IpAddr::V4(ip) if ip.octets() == [0, 0, 0, 0] => "127.0.0.1".to_string(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => "::1".to_string(),
        ip => ip.to_string(),
    };
    Ok(format!("http://{}:{}", host, primary_addr.port()))
}

pub(super) async fn bind_daemon_listeners(bind: Vec<String>) -> Result<BoundDaemonListeners> {
    let requested_binds = requested_binds(bind);
    let optional_default_bind = optional_default_bind();
    let mut listeners = Vec::with_capacity(requested_binds.len());
    for bind in &requested_binds {
        match TcpListener::bind(bind).await {
            Ok(listener) => listeners.push(listener),
            Err(err) if optional_default_bind.as_deref() == Some(bind.as_str()) => {
                tracing::warn!(
                    "failed to bind optional AVF guest gateway listener at {bind}: {err}"
                );
            }
            Err(err) => {
                return Err(err).with_context(|| format!("binding daemon listener at {bind}"));
            }
        }
    }
    if listeners.is_empty() {
        anyhow::bail!("failed to bind any daemon listeners");
    }
    let daemon_url = daemon_url_from_listeners(&listeners)?;
    Ok(BoundDaemonListeners {
        requested_binds,
        listeners,
        daemon_url,
    })
}

pub fn daemon_public_base_url_from_env() -> Result<Option<String>> {
    let Some(raw) = std::env::var("CTX_DAEMON_PUBLIC_BASE_URL").ok() else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("CTX_DAEMON_PUBLIC_BASE_URL is empty");
    }
    let parsed = Url::parse(trimmed)
        .with_context(|| format!("parsing CTX_DAEMON_PUBLIC_BASE_URL `{trimmed}`"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("CTX_DAEMON_PUBLIC_BASE_URL must use http:// or https://");
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("CTX_DAEMON_PUBLIC_BASE_URL must include a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("CTX_DAEMON_PUBLIC_BASE_URL must not embed credentials");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        anyhow::bail!("CTX_DAEMON_PUBLIC_BASE_URL must not include query or fragment");
    }
    Ok(Some(trimmed.trim_end_matches('/').to_string()))
}
