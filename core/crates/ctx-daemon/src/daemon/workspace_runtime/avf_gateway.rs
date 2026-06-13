use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

mod relay;

use relay::spawn_avf_daemon_gateway_proxy;

struct AvfDaemonGatewayProxy {
    gateway_addr: String,
    backend_addr: String,
    handle: tokio::task::JoinHandle<()>,
}

static AVF_DAEMON_GATEWAY_PROXIES: OnceLock<StdMutex<HashMap<u16, AvfDaemonGatewayProxy>>> =
    OnceLock::new();

fn avf_daemon_gateway_proxies() -> &'static StdMutex<HashMap<u16, AvfDaemonGatewayProxy>> {
    AVF_DAEMON_GATEWAY_PROXIES.get_or_init(|| StdMutex::new(HashMap::new()))
}

async fn ensure_avf_guest_gateway_proxy(
    gateway_addr: &str,
    backend_addr: &str,
    port: u16,
) -> Result<()> {
    let mut replaced_existing_proxy = false;
    let existing_handle_to_abort = {
        let mut proxies = avf_daemon_gateway_proxies()
            .lock()
            .map_err(|_| anyhow!("AVF daemon gateway proxy mutex poisoned"))?;
        proxies.retain(|_, proxy| !proxy.handle.is_finished());
        if let Some(existing) = proxies.get(&port) {
            if existing.gateway_addr == gateway_addr && existing.backend_addr == backend_addr {
                return Ok(());
            }
        }
        let removed = proxies.remove(&port).map(|proxy| proxy.handle);
        if removed.is_some() {
            replaced_existing_proxy = true;
        }
        removed
    };

    if let Some(handle) = existing_handle_to_abort {
        handle.abort();
        tokio::task::yield_now().await;
    }

    let listener = loop {
        match tokio::net::TcpListener::bind(gateway_addr).await {
            Ok(listener) => break listener,
            Err(err)
                if replaced_existing_proxy
                    && matches!(
                        err.kind(),
                        ErrorKind::AddrInUse | ErrorKind::AddrNotAvailable
                    ) =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::AddrInUse | ErrorKind::AddrNotAvailable
                ) =>
            {
                tracing::debug!(
                    gateway_addr,
                    backend_addr,
                    "AVF daemon gateway proxy bind is unavailable; assuming a guest-reachable listener already exists"
                );
                return Ok(());
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("binding AVF guest gateway proxy at {gateway_addr} for {backend_addr}")
                });
            }
        }
    };

    let gateway_addr = gateway_addr.to_string();
    let backend_addr = backend_addr.to_string();
    let handle =
        spawn_avf_daemon_gateway_proxy(listener, gateway_addr.clone(), backend_addr.clone());
    let mut proxies = avf_daemon_gateway_proxies()
        .lock()
        .map_err(|_| anyhow!("AVF daemon gateway proxy mutex poisoned"))?;
    if let Some(existing) = proxies.get(&port) {
        if !existing.handle.is_finished() {
            handle.abort();
            return Ok(());
        }
    }
    proxies.insert(
        port,
        AvfDaemonGatewayProxy {
            gateway_addr: gateway_addr.clone(),
            backend_addr: backend_addr.clone(),
            handle,
        },
    );
    tracing::info!(
        gateway_addr,
        backend_addr,
        "started AVF daemon gateway proxy"
    );
    Ok(())
}

pub async fn ensure_avf_guest_gateway_proxy_for_test(
    gateway_addr: &str,
    backend_addr: &str,
    port: u16,
) -> Result<()> {
    ensure_avf_guest_gateway_proxy(gateway_addr, backend_addr, port).await
}
