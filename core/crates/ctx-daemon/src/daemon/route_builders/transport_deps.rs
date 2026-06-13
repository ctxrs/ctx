use std::path::PathBuf;
use std::sync::Arc;

use ctx_observability::ops_events::OpsEvents;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_store::Store;
use ctx_transport_runtime::mobile_tunnel::MobileTunnelManager;
use ctx_transport_runtime::terminals::TerminalManager;
use ctx_transport_runtime::web_sessions::WebSessionManager;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::route_handles::HealthHandle;
use crate::daemon::state::ProtectedWorkspaceStoreLookup;

pub(super) struct TransportRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) auth_token_configured: bool,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) mobile_tunnel: MobileTunnelManager,
    pub(super) terminals: Arc<TerminalManager>,
    pub(super) web_sessions: Arc<WebSessionManager>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) health: HealthHandle,
    pub(super) telemetry: Telemetry,
    pub(super) ops_events: OpsEvents,
}

#[derive(Clone)]
pub(super) struct TransportRouteDeps {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) auth_token_configured: bool,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) mobile_tunnel: MobileTunnelManager,
    pub(super) terminals: Arc<TerminalManager>,
    pub(super) web_sessions: Arc<WebSessionManager>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) health: HealthHandle,
    pub(super) telemetry: Telemetry,
    pub(super) ops_events: OpsEvents,
}

impl TransportRouteDeps {
    pub(super) fn new(parts: TransportRouteDepsParts) -> Self {
        Self {
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            auth_token_configured: parts.auth_token_configured,
            global_store: parts.global_store,
            workspace_stores: parts.workspace_stores,
            mobile_tunnel: parts.mobile_tunnel,
            terminals: parts.terminals,
            web_sessions: parts.web_sessions,
            providers: parts.providers,
            harness: parts.harness,
            health: parts.health,
            telemetry: parts.telemetry,
            ops_events: parts.ops_events,
        }
    }

    pub(super) fn workspace_store_lookup(&self) -> ProtectedWorkspaceStoreLookup {
        self.workspace_stores.clone()
    }
}
