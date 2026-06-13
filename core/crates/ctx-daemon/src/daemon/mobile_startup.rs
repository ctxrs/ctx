use std::sync::Arc;

use super::DaemonState;

pub(super) fn spawn_saved_mobile_tunnel_reconnect(state: Arc<DaemonState>) {
    tokio::spawn(async move {
        if state.core.auth_token.is_none() {
            return;
        }
        let cfg = match state.global_store().get_mobile_access_config().await {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("failed to read saved mobile access config: {err:#}");
                return;
            }
        };
        let Some(cfg) = cfg else {
            return;
        };
        if !cfg.enabled {
            return;
        }

        let start_cfg = ctx_transport_runtime::mobile_tunnel::StartMobileTunnelConfig {
            relay_base_url: cfg.relay_base_url,
            tunnel_id: cfg.tunnel_id,
            tunnel_secret: cfg.tunnel_secret,
            public_base_url: cfg.public_base_url.trim_end_matches('/').to_string(),
            local_daemon_url: state.core.daemon_url.trim_end_matches('/').to_string(),
        };
        if let Err(err) = state.transport.mobile_tunnel.start(start_cfg).await {
            tracing::warn!("failed to start saved mobile tunnel: {err:#}");
        }
    });
}
