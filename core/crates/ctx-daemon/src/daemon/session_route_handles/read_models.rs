use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_store::{Store, StoreManager};
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use crate::daemon::state::SessionStoreLookup;

#[derive(Clone)]
pub struct SessionReadModelsHandle {
    global_store: Store,
    session_stores: SessionStoreLookup,
    stores: StoreManager,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    tool_output_spool_dir: PathBuf,
    perf_telemetry: PerfTelemetry,
}

impl SessionReadModelsHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        session_stores: SessionStoreLookup,
        stores: StoreManager,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
        tool_output_spool_dir: PathBuf,
        perf_telemetry: PerfTelemetry,
    ) -> Self {
        Self {
            global_store,
            session_stores,
            stores,
            active_snapshot,
            tool_output_spool_dir,
            perf_telemetry,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn session_stores(&self) -> &SessionStoreLookup {
        &self.session_stores
    }

    pub(in crate::daemon) fn stores(&self) -> &StoreManager {
        &self.stores
    }

    pub(in crate::daemon) fn active_snapshot(&self) -> &WorkspaceActiveSnapshotHub {
        self.active_snapshot.as_ref()
    }

    pub(in crate::daemon) fn tool_output_spool_dir(&self) -> &Path {
        &self.tool_output_spool_dir
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }
}
