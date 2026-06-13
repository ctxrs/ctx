use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_http_auth::daemon as daemon_auth;
use ctx_store::{Store, StoreManager, StoreManagerConfig};
use directories::BaseDirs;
use tokio::net::TcpListener;

use super::*;

#[path = "serve/background.rs"]
mod background;
#[cfg(test)]
pub(in crate::daemon) use background::spawn_startup_provider_status_refresh;

pub struct DaemonRuntime {
    pub _daemon_lock: std::fs::File,
    pub route_handles: DaemonRouteHandles,
    pub shutdown_signal: DaemonShutdownSignal,
    pub listeners: Vec<TcpListener>,
    pub daemon_url: String,
}

pub async fn bootstrap_daemon_runtime(
    bind: Vec<String>,
    data_dir: Option<String>,
) -> Result<DaemonRuntime> {
    let data_root = match data_dir {
        Some(p) => PathBuf::from(p),
        None => {
            let base = BaseDirs::new().context("resolving home dir")?;
            base.home_dir().join(".ctx")
        }
    };
    let data_root = daemon_auth::prepare_daemon_data_root(data_root)?;

    let daemon_lock = daemon_auth::acquire_daemon_lock(&data_root)?;

    let global_db_path = data_root.join("db").join("db.sqlite");
    let bootstrap_store = Store::open_sqlite(&global_db_path, None).await?;
    let settings_data = ctx_settings_service::load_settings(&bootstrap_store).await?;
    bootstrap_store.close().await;
    let store_config = settings_data
        .storage
        .as_ref()
        .map(|storage| StoreManagerConfig {
            max_connections: storage.max_connections,
            workspace_max_connections: storage.max_connections,
            ..StoreManagerConfig::default()
        })
        .unwrap_or_default();
    let stores = StoreManager::open_with_config(&data_root, store_config).await?;

    retention::spawn_archived_session_data_pruner(stores.clone());

    let agent_cfg =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_or_err(
            &data_root,
        )
        .await?;

    let providers = ctx_provider_runtime::provider_adapters::build_startup_provider_adapters(
        &data_root, &agent_cfg,
    );

    let bound = listener::bind_daemon_listeners(bind).await?;
    let daemon_url = bound.daemon_url.clone();
    let requested_binds = bound.requested_binds.clone();
    let listeners = bound.listeners;
    let public_base_url = listener::daemon_public_base_url_from_env()?;

    let mut auth = daemon_auth::load_or_init_daemon_auth(&data_root)?;
    let auth_token = Some(auth.token.clone());

    auth.daemon_url = Some(daemon_url.clone());
    daemon_auth::write_daemon_auth_file(&daemon_auth::daemon_auth_path(&data_root), &auth)?;

    let state = Arc::new(DaemonState::new_with_public_base_url(
        data_root,
        stores,
        providers,
        daemon_url.clone(),
        public_base_url,
        auth_token,
    ));
    state.transport.web_sessions.clone().start_reaper().await;
    state.transport.terminals.clone().start_reaper().await;
    lifecycle::spawn_cache_sweeper(state.clone());
    lifecycle::spawn_provider_worker_sweeper(state.clone());
    lifecycle::spawn_endpoint_model_catalog_sweeper(state.clone());
    if let Err(err) = reconcile_running_turns(&state).await {
        tracing::warn!(err = %err, "failed to reconcile running turns on startup");
    }
    let route_handles = route_handles_from_state(&state);
    let settings = ctx_settings_service::load_settings(state.global_store()).await?;
    route_handles
        .settings
        .apply_settings_side_effects(&settings)
        .await;
    let shutdown_signal = DaemonShutdownSignal::new(state.core.shutdown_tx.clone());
    background::spawn_daemon_background_services(
        state,
        requested_binds,
        route_handles.provider_status.clone(),
        route_handles.provider_usage.clone(),
    );
    Ok(DaemonRuntime {
        _daemon_lock: daemon_lock,
        route_handles,
        shutdown_signal,
        listeners,
        daemon_url,
    })
}
