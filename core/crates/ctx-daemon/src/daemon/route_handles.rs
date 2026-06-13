use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_execution_runtime::ExecutionSetupCoordinator;
use ctx_mcp_auth::McpAuthRegistry;
use ctx_observability::ops_events::{OpsEvent, OpsEvents};
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_storage_admission::{StorageGuardRuntime, StorageGuardStatus};
use ctx_store::Store;

use super::state::TelemetryRuntime;

#[derive(Clone)]
pub struct TelemetryHandle {
    data_root: PathBuf,
    perf_telemetry: PerfTelemetry,
    telemetry: Telemetry,
}

impl TelemetryHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf, runtime: &TelemetryRuntime) -> Self {
        Self {
            data_root,
            perf_telemetry: runtime.perf_telemetry.clone(),
            telemetry: runtime.telemetry.clone(),
        }
    }

    pub fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub async fn read_perf_telemetry_export_for_date(
        &self,
        date: &str,
    ) -> Result<Vec<u8>, ctx_route_contracts::telemetry::TelemetryExportError> {
        let path = ctx_observability::perf_telemetry::perf_log_path_for_date(&self.data_root, date);
        tokio::fs::read(&path)
            .await
            .map_err(|_| ctx_route_contracts::telemetry::TelemetryExportError::not_found())
    }
}

#[derive(Clone)]
pub struct AuthHandle {
    auth_token: Option<String>,
    mcp_auth: Arc<McpAuthRegistry>,
    store: Store,
    ops_events: OpsEvents,
}

impl AuthHandle {
    pub(in crate::daemon) fn new(
        auth_token: Option<String>,
        mcp_auth: Arc<McpAuthRegistry>,
        store: Store,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            auth_token,
            mcp_auth,
            store,
            ops_events,
        }
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    pub fn has_auth_token(&self) -> bool {
        self.auth_token.is_some()
    }

    pub async fn verify_mcp_auth_token(&self, token: &str) -> Option<ctx_mcp_auth::McpAuthContext> {
        self.mcp_auth.verify_token(token).await
    }

    pub fn emit_mcp_token_denied(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        method: &str,
        path: &str,
        reason: &str,
    ) {
        let mut event = OpsEvent::new("warn", "mcp_token_denied");
        event.session_id = Some(mcp_auth.session_id.0.to_string());
        event.worktree_id = Some(mcp_auth.worktree_id.0.to_string());
        event.meta = Some(serde_json::json!({
            "workspace_id": mcp_auth.workspace_id.0.to_string(),
            "capabilities": mcp_auth.capabilities.names(),
            "detail": {
                "method": method,
                "path": path,
                "reason": reason,
            },
        }));
        self.ops_events.emit(event);
    }

    pub async fn verify_mobile_api_token_hash(
        &self,
        hash: &str,
    ) -> Result<
        Option<ctx_mobile_access_service::MobileAuthContext>,
        ctx_mobile_access_service::MobileAuthContextError,
    > {
        ctx_mobile_access_service::verify_mobile_api_token_hash(&self.store, hash).await
    }
}

#[derive(Clone)]
pub struct HealthHandle {
    data_root: PathBuf,
    daemon_url: String,
    auth_token: Option<String>,
    storage_guard: Arc<StorageGuardRuntime>,
}

impl HealthHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        auth_token: Option<String>,
        storage_guard: Arc<StorageGuardRuntime>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            auth_token,
            storage_guard,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    pub(in crate::daemon) fn auth_required(&self) -> bool {
        self.auth_token.is_some()
    }

    pub(in crate::daemon) fn storage_guard_snapshot(&self) -> StorageGuardStatus {
        self.storage_guard.snapshot()
    }
}

#[derive(Clone)]
pub struct DiagnosticsHandle {
    health: HealthHandle,
    data_root: PathBuf,
    execution_setup: Arc<ExecutionSetupCoordinator>,
    providers: Arc<ProviderRuntime>,
}

impl DiagnosticsHandle {
    pub(in crate::daemon) fn new(
        health: HealthHandle,
        data_root: PathBuf,
        execution_setup: Arc<ExecutionSetupCoordinator>,
        providers: Arc<ProviderRuntime>,
    ) -> Self {
        Self {
            health,
            data_root,
            execution_setup,
            providers,
        }
    }

    pub(in crate::daemon) fn health(&self) -> &HealthHandle {
        &self.health
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn execution_setup(&self) -> &ExecutionSetupCoordinator {
        &self.execution_setup
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        &self.providers
    }
}

#[derive(Clone)]
pub struct RequestBaseHandle {
    daemon_url: String,
    public_base_url: Option<String>,
}

impl RequestBaseHandle {
    pub(in crate::daemon) fn new(daemon_url: String, public_base_url: Option<String>) -> Self {
        Self {
            daemon_url,
            public_base_url,
        }
    }

    pub fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub fn public_base_url(&self) -> Option<&str> {
        self.public_base_url.as_deref()
    }
}

#[derive(Clone)]
pub struct LogsHandle {
    data_root: PathBuf,
}

impl LogsHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[derive(Clone)]
pub struct OrgPolicyHandle {
    store: Store,
}

impl OrgPolicyHandle {
    pub(in crate::daemon) fn new(store: Store) -> Self {
        Self { store }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }
}

#[derive(Clone)]
pub struct DictationHandle {
    store: Store,
}

impl DictationHandle {
    pub(in crate::daemon) fn new(store: Store) -> Self {
        Self { store }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }
}

#[derive(Clone)]
pub struct UpdateReleaseHandle {
    data_root: PathBuf,
}

impl UpdateReleaseHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[derive(Clone)]
pub struct MobileStoreHandle {
    store: Store,
}

impl MobileStoreHandle {
    pub(in crate::daemon) fn new(store: Store) -> Self {
        Self { store }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }
}

#[derive(Clone)]
pub struct RepoOnboardingHandle {
    data_root: PathBuf,
}

impl RepoOnboardingHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }
}
