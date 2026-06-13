use anyhow::{Context, Result};
use ctx_daemon::daemon;
use serde_json::json;

use crate::api;

pub async fn serve(bind: Vec<String>, data_dir: Option<String>) -> Result<()> {
    let runtime = daemon::bootstrap_daemon_runtime(bind, data_dir).await?;
    serve_runtime(runtime).await
}

async fn serve_runtime(runtime: daemon::DaemonRuntime) -> Result<()> {
    let daemon::DaemonRuntime {
        _daemon_lock,
        route_handles,
        shutdown_signal,
        listeners,
        daemon_url,
    } = runtime;
    let app = api::router(api::RouteHandles::from_daemon_route_handles(route_handles));
    let bound_addrs = listeners
        .iter()
        .filter_map(|listener| listener.local_addr().ok())
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>();
    tracing::info!("ctx daemon listening on {daemon_url} (binds={bound_addrs:?})");
    println!("{}", json!({"event":"listening","url": daemon_url}));
    let mut servers = tokio::task::JoinSet::new();
    for listener in listeners {
        let app = app.clone();
        let mut shutdown_rx = shutdown_signal.subscribe();
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.recv().await;
                })
                .await
        });
    }
    while let Some(result) = servers.join_next().await {
        result.context("daemon listener task panicked")??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ctx_daemon::daemon::DaemonRuntime;

    use super::*;
    use crate::test_support::TestDaemonFixture;

    #[tokio::test]
    async fn serve_runtime_exits_when_shutdown_signal_fires() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let daemon_url = format!("http://{}", listener.local_addr().expect("listener addr"));
        let fixture = TestDaemonFixture::new(daemon_url.clone()).await;
        let runtime = DaemonRuntime {
            _daemon_lock: tempfile::tempfile().expect("daemon lock file"),
            route_handles: fixture.daemon().route_handles(),
            shutdown_signal: fixture.daemon().shutdown_signal(),
            listeners: vec![listener],
            daemon_url: daemon_url.clone(),
        };

        let server = tokio::spawn(serve_runtime(runtime));
        wait_for_health(&daemon_url).await;
        fixture.daemon().emit_shutdown_for_test();

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server should stop after shutdown signal")
            .expect("server task should not panic")
            .expect("server should exit cleanly");
    }

    async fn wait_for_health(daemon_url: &str) {
        let health_url = format!("{daemon_url}/api/health");
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut last_error = "no attempt made".to_string();
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(250), client.get(&health_url).send())
                .await
            {
                Ok(Ok(response)) if response.status().is_success() => return,
                Ok(Ok(response)) => {
                    last_error = format!("health returned {}", response.status());
                }
                Ok(Err(error)) => {
                    last_error = error.to_string();
                }
                Err(_) => {
                    last_error = "health request timed out".to_string();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("server did not answer /api/health before shutdown test: {last_error}");
    }
}
