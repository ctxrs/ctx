use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ctx_daemon::test_support::TestDaemon;
use ctx_providers::adapters::ProviderAdapter;

pub(crate) async fn bind_loopback_listener() -> Result<(tokio::net::TcpListener, String)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind daemon test listener")?;
    let addr = listener.local_addr().context("read daemon listener addr")?;
    Ok((listener, format!("http://{addr}")))
}

pub(crate) fn spawn_router_for_daemon(listener: tokio::net::TcpListener, daemon: &TestDaemon) {
    let app = ctx_http::api::router(ctx_http::api::RouteHandles::from_daemon_route_handles(
        daemon.route_handles(),
    ));
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("daemon test router failed");
    });
}

pub(crate) fn live_provider_adapter(provider_id: &str) -> Result<Arc<dyn ProviderAdapter>> {
    match provider_id {
        "codex" => Ok(Arc::new(ctx_providers::crp::Tier1CrpAdapter::codex())),
        "claude" | "claude-crp" => Ok(Arc::new(ctx_providers::crp::Tier1CrpAdapter::claude())),
        other => bail!("live subagent canary does not support provider {other}"),
    }
}
