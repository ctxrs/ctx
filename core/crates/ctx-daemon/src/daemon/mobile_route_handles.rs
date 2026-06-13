use ctx_observability::telemetry::Telemetry;
use ctx_store::Store;
use ctx_transport_runtime::mobile_tunnel::MobileTunnelManager;

use super::HealthHandle;

#[derive(Clone)]
pub struct MobileRuntimeHandle {
    store: Store,
    mobile_tunnel: MobileTunnelManager,
    daemon_url: String,
    auth_token_configured: bool,
}

impl MobileRuntimeHandle {
    pub(in crate::daemon) fn new(
        store: Store,
        mobile_tunnel: MobileTunnelManager,
        daemon_url: String,
        auth_token_configured: bool,
    ) -> Self {
        Self {
            store,
            mobile_tunnel,
            daemon_url,
            auth_token_configured,
        }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }

    pub(in crate::daemon) fn mobile_tunnel(&self) -> &MobileTunnelManager {
        &self.mobile_tunnel
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn auth_token_configured(&self) -> bool {
        self.auth_token_configured
    }
}

#[derive(Clone)]
pub struct MobileSecureProxyHandle {
    store: Store,
    health: HealthHandle,
    telemetry: Telemetry,
}

impl MobileSecureProxyHandle {
    pub(in crate::daemon) fn new(store: Store, health: HealthHandle, telemetry: Telemetry) -> Self {
        Self {
            store,
            health,
            telemetry,
        }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }

    pub(in crate::daemon) fn health(&self) -> &HealthHandle {
        &self.health
    }

    pub(in crate::daemon) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }
}
