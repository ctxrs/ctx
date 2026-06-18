use std::path::PathBuf;
use std::sync::Arc;

use ctx_observability::ops_events::OpsEvents;
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_store::Store;
use ctx_transport_runtime::web_sessions::WebSessionManager;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use crate::daemon::plugins::PluginInventoryRuntime;
use crate::daemon::scheduler::SessionSchedulerWorkerHost;
use crate::daemon::state::{ProtectedWorkspaceStoreLookup, SessionRuntime};

pub(super) struct TaskRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) scheduler_worker_host: Arc<SessionSchedulerWorkerHost>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) plugins: Arc<PluginInventoryRuntime>,
    pub(super) web_sessions: Arc<WebSessionManager>,
    pub(super) telemetry: Telemetry,
    pub(super) ops_events: OpsEvents,
    pub(super) perf_telemetry: PerfTelemetry,
}

#[derive(Clone)]
pub(super) struct TaskRouteDeps {
    pub(super) data_root: PathBuf,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) scheduler_worker_host: Arc<SessionSchedulerWorkerHost>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) plugins: Arc<PluginInventoryRuntime>,
    pub(super) web_sessions: Arc<WebSessionManager>,
    pub(super) telemetry: Telemetry,
    pub(super) ops_events: OpsEvents,
    pub(super) perf_telemetry: PerfTelemetry,
}

impl TaskRouteDeps {
    pub(super) fn new(parts: TaskRouteDepsParts) -> Self {
        Self {
            data_root: parts.data_root,
            global_store: parts.global_store,
            workspace_stores: parts.workspace_stores,
            active_snapshot: parts.active_snapshot,
            sessions: parts.sessions,
            scheduler_worker_host: parts.scheduler_worker_host,
            providers: parts.providers,
            plugins: parts.plugins,
            web_sessions: parts.web_sessions,
            telemetry: parts.telemetry,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
        }
    }

    pub(super) fn workspace_store_lookup(&self) -> ProtectedWorkspaceStoreLookup {
        self.workspace_stores.clone()
    }
}
