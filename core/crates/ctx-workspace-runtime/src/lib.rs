use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_execution_runtime::{
    ContainerExecutionSettings, ContainerRuntimeKind, ExecutionMode, ExecutionSettings,
    NoopRuntimeEventSink, RuntimeEventSink,
};
use url::Url;

mod manager;
mod manager_container;
mod manager_native_machine;
mod materialization;

struct AvfDaemonGatewayProxy {
    gateway_addr: String,
    backend_addr: String,
    handle: tokio::task::JoinHandle<()>,
}

static AVF_DAEMON_GATEWAY_PROXIES: OnceLock<StdMutex<HashMap<u16, AvfDaemonGatewayProxy>>> =
    OnceLock::new();

pub use self::materialization::{
    materialize_sandbox_binding, materialize_sandbox_worktree,
    sandbox_binding_from_materialization, MaterializeSandboxBindingParams,
    SandboxWorktreeMaterialization,
};
pub(crate) use ctx_avf_linux_runtime::SharedVmLifecycleOrchestrator;
use ctx_avf_linux_runtime::AVF_LINUX_HELPER_PATH_ENV;
use ctx_avf_linux_runtime::{
    helper_path as avf_linux_helper_path,
    workspace_vm_data_root as avf_linux_workspace_vm_data_root,
};
pub(crate) use ctx_harness_runtime::{
    sandbox_engine_ready, selected_sandbox_command_backend, selected_sandbox_command_mode,
    HarnessExecutionPlan, HarnessRuntimeKind, HarnessRuntimeStats, SandboxCommandBackend,
    CTX_AVF_HOST_DATA_ROOT_ENV, CTX_AVF_HOST_WORKTREE_ROOT_ENV, CTX_AVF_WORKSPACE_ID_ENV,
    CTX_AVF_WORKTREE_ID_ENV, CTX_HARNESS_LINUX_SANDBOX_ENV, CTX_HARNESS_RUNTIME_KIND_ENV,
};
pub(crate) use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;
pub use ctx_sandbox_container_runtime::{
    bundled_default_container_image_tar, command_output_message, command_output_with_timeout,
    default_container_image, is_default_container_image, sandbox_cli_env_for_data_root,
    sandbox_cli_invocation, ContainerImageStatus, SHARED_VM_SANDBOX_CLI_GUEST_BIN,
};
pub(crate) use ctx_sandbox_contract::UbuntuSandboxSubstrate;
#[cfg(feature = "test-support")]
use ctx_workspace_container::WorkspaceContainer;
use ctx_workspace_container::{rewrite_daemon_url_for_avf_guest, WorkspaceContainerOwner};
use ctx_workspace_container::{
    WorkspaceContainerStatus as HarnessContainerStatus, AVF_GUEST_HOST_GATEWAY,
};

pub(crate) const CTX_AVF_REAL_GUEST_EXEC_ENV: &str = "CTX_AVF_REAL_GUEST_EXEC";

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
    let gateway_addr_for_task = gateway_addr.clone();
    let backend_addr_for_task = backend_addr.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut inbound, peer_addr) = match listener.accept().await {
                Ok(parts) => parts,
                Err(err) => {
                    tracing::warn!(
                        gateway_addr = gateway_addr_for_task,
                        backend_addr = backend_addr_for_task,
                        "AVF daemon gateway proxy accept failed: {err}"
                    );
                    break;
                }
            };
            let backend_addr = backend_addr_for_task.clone();
            let gateway_addr = gateway_addr_for_task.clone();
            tokio::spawn(async move {
                match tokio::net::TcpStream::connect(&backend_addr).await {
                    Ok(mut outbound) => {
                        if let Err(err) =
                            tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await
                        {
                            tracing::debug!(
                                gateway_addr,
                                backend_addr,
                                %peer_addr,
                                "AVF daemon gateway proxy relay closed with error: {err}"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            gateway_addr,
                            backend_addr,
                            %peer_addr,
                            "AVF daemon gateway proxy could not connect to backend: {err}"
                        );
                    }
                }
            });
        }
    });
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

async fn resolve_daemon_url_for_avf_guest(daemon_url: &str) -> Result<String> {
    let Ok(url) = Url::parse(daemon_url) else {
        return Ok(daemon_url.to_string());
    };
    let Some(host) = url.host_str() else {
        return Ok(daemon_url.to_string());
    };
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Ok(daemon_url.to_string());
    }
    let Some(port) = url.port_or_known_default() else {
        return Ok(daemon_url.to_string());
    };
    let gateway_addr = format!("{AVF_GUEST_HOST_GATEWAY}:{port}");
    match tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(&gateway_addr),
    )
    .await
    {
        Ok(Ok(_)) => Ok(rewrite_daemon_url_for_avf_guest(daemon_url)),
        Ok(Err(_)) | Err(_) => {
            let backend_host = match host {
                "localhost" | "::1" => "127.0.0.1",
                other => other,
            };
            let backend_addr = format!("{backend_host}:{port}");
            ensure_avf_guest_gateway_proxy(&gateway_addr, &backend_addr, port).await?;
            Ok(rewrite_daemon_url_for_avf_guest(daemon_url))
        }
    }
}

pub use ctx_harness_setup::{
    HarnessSetupDownloadStatus, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    HarnessSetupProgressUpdate,
};

pub struct HarnessRuntimeManager {
    data_root: PathBuf,
    workspace_containers: WorkspaceContainerOwner,
    last_activity: StdMutex<Instant>,
    active_runtime_operations: AtomicUsize,
    active_prewarm_artifact_operations: AtomicUsize,
    event_sink: Arc<dyn RuntimeEventSink>,
}

impl HarnessRuntimeManager {
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    #[cfg(feature = "test-support")]
    pub fn set_last_activity_for_test(&self, instant: Instant) {
        let mut last_activity = match self.last_activity.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *last_activity = instant;
    }

    pub fn runtime_operation_count(&self) -> usize {
        self.active_runtime_operations.load(Ordering::SeqCst)
    }

    pub fn prewarm_artifact_operation_count(&self) -> usize {
        self.active_prewarm_artifact_operations
            .load(Ordering::SeqCst)
    }

    #[cfg(feature = "test-support")]
    pub async fn put_cached_workspace_container_for_test(
        &self,
        workspace_id: WorkspaceId,
        container: WorkspaceContainer,
    ) {
        self.workspace_containers
            .put_cached_container_for_test(workspace_id, container)
            .await;
    }
}
