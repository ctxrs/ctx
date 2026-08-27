use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{anyhow, Result};
use ctx_semantic_model::{
    semantic_model_cache_available, semantic_model_key, SharedSemanticRuntime,
};
use serde_json::{json, Value};

pub(crate) use ctx_daemon_runtime::DaemonLifecycleState;

use crate::compact_json;
use crate::{
    daemon_wakeup::DaemonWakeup,
    paths_status::{daemon_source_backed_refresh_job_path, read_daemon_job_status},
    semantic_intensity::{handle_semantic_intensity_lease_request, SemanticIntensityLeaseRegistry},
    source_backed_refresh_adapter::wire,
    source_backed_refresh_coordinator::CoreRefreshEngine,
    DaemonConfigPort,
};

#[cfg(unix)]
use crate::paths_status::{daemon_query_socket_path, daemon_root_path};

use super::super::transport::{
    daemon_service_endpoint_path, DaemonIpcService, DaemonQueryEndpoint,
};
use super::{
    start_ipc_service_with_request_timeout, AuthenticatedRequest, AuthenticatedRequestHandler,
    DaemonQueryService, DaemonWakePort, HandlerOutcome, IpcEndpointPublication, IpcEndpointStore,
    IpcServiceSpec, PostWriteAction, ServiceId, DAEMON_QUERY_REQUEST_READ_TIMEOUT,
};

struct CtxIpcEndpointStore {
    endpoint_path: PathBuf,
}

impl CtxIpcEndpointStore {
    fn new(endpoint_path: PathBuf) -> Arc<Self> {
        Arc::new(Self { endpoint_path })
    }
}

impl IpcEndpointStore for CtxIpcEndpointStore {
    fn prepare(&self) -> Result<()> {
        let parent = self
            .endpoint_path
            .parent()
            .ok_or_else(|| anyhow!("IPC service endpoint has no parent directory"))?;
        crate::paths_status::create_private_dir_all(parent)
    }

    fn publish(&self, endpoint: &IpcEndpointPublication) -> Result<()> {
        #[cfg(unix)]
        let endpoint = DaemonQueryEndpoint::Unix {
            path: endpoint.unix_socket_path.clone(),
            token: endpoint.token.clone(),
        };
        #[cfg(windows)]
        let endpoint = DaemonQueryEndpoint::WindowsNamedPipe {
            pipe_name: endpoint.windows_pipe_name.clone(),
            token: endpoint.token.clone(),
        };
        super::super::transport::write_daemon_service_endpoint_at(&self.endpoint_path, &endpoint)
    }

    fn remove(&self) {
        super::super::transport::remove_daemon_service_endpoint_at(&self.endpoint_path);
    }
}

impl DaemonWakePort for DaemonWakeup {
    fn signal_ipc(&self) {
        self.signal_ipc();
    }
}

const SEMANTIC_QUERY_SERVICE_ID: &str = "semantic-query";
const SOURCE_REFRESH_SERVICE_ID: &str = "source-refresh";

fn semantic_query_service_spec(data_root: &Path) -> Result<IpcServiceSpec> {
    let service_id = ServiceId::new(SEMANTIC_QUERY_SERVICE_ID)?;
    #[cfg(unix)]
    {
        IpcServiceSpec::new(service_id, daemon_query_socket_path(data_root), true)
    }
    #[cfg(not(unix))]
    {
        let _ = data_root;
        IpcServiceSpec::new(service_id, true)
    }
}

fn source_refresh_service_spec(data_root: &Path) -> Result<IpcServiceSpec> {
    let service_id = ServiceId::new(SOURCE_REFRESH_SERVICE_ID)?;
    #[cfg(unix)]
    {
        IpcServiceSpec::new(
            service_id,
            daemon_root_path(data_root).join("source-refresh.sock"),
            false,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = data_root;
        IpcServiceSpec::new(service_id, false)
    }
}

pub(crate) fn start_daemon_query_service(
    data_root: &Path,
    handler: Arc<CtxAuthenticatedRequestHandler>,
    wakeup: Arc<DaemonWakeup>,
) -> Result<DaemonQueryService> {
    #[cfg(any(unix, windows))]
    {
        start_ipc_service_with_request_timeout(
            semantic_query_service_spec(data_root)?,
            CtxIpcEndpointStore::new(daemon_service_endpoint_path(
                data_root,
                DaemonIpcService::SemanticQuery,
            )),
            handler,
            DAEMON_QUERY_REQUEST_READ_TIMEOUT,
            Some(wakeup),
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (data_root, handler, wakeup);
        Err(anyhow!(
            "daemon query service is not supported on this platform"
        ))
    }
}

#[cfg(test)]
pub(crate) fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    handler: Arc<CtxAuthenticatedRequestHandler>,
    request_read_timeout: std::time::Duration,
) -> Result<DaemonQueryService> {
    start_ipc_service_with_request_timeout(
        semantic_query_service_spec(data_root)?,
        CtxIpcEndpointStore::new(daemon_service_endpoint_path(
            data_root,
            DaemonIpcService::SemanticQuery,
        )),
        handler,
        request_read_timeout,
        Some(Arc::new(DaemonWakeup::default())),
    )
}

pub(crate) fn start_daemon_source_refresh_service(
    data_root: &Path,
    handler: Arc<CtxAuthenticatedRequestHandler>,
    wakeup: Arc<DaemonWakeup>,
) -> Result<DaemonQueryService> {
    #[cfg(any(unix, windows))]
    {
        start_ipc_service_with_request_timeout(
            source_refresh_service_spec(data_root)?,
            CtxIpcEndpointStore::new(daemon_service_endpoint_path(
                data_root,
                DaemonIpcService::SourceRefresh,
            )),
            handler,
            DAEMON_QUERY_REQUEST_READ_TIMEOUT,
            Some(wakeup),
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (data_root, handler, wakeup);
        Err(anyhow!(
            "daemon source refresh service is not supported on this platform"
        ))
    }
}

#[cfg(test)]
pub(crate) fn start_daemon_source_refresh_service_with_request_timeout(
    data_root: &Path,
    handler: Arc<CtxAuthenticatedRequestHandler>,
    request_read_timeout: std::time::Duration,
) -> Result<DaemonQueryService> {
    start_ipc_service_with_request_timeout(
        source_refresh_service_spec(data_root)?,
        CtxIpcEndpointStore::new(daemon_service_endpoint_path(
            data_root,
            DaemonIpcService::SourceRefresh,
        )),
        handler,
        request_read_timeout,
        Some(Arc::new(DaemonWakeup::default())),
    )
}

#[cfg(all(test, unix))]
pub(crate) fn bind_daemon_query_listener(
    data_root: &Path,
) -> Result<(std::os::unix::net::UnixListener, PathBuf, Option<PathBuf>)> {
    super::bind_daemon_service_listener(&daemon_query_socket_path(data_root))
}

pub(crate) struct CtxAuthenticatedRequestHandler {
    data_root: PathBuf,
    runtime: SharedSemanticRuntime,
    source_refresh: Arc<CoreRefreshEngine>,
    wakeup: Arc<DaemonWakeup>,
    config: &'static dyn DaemonConfigPort,
    lifecycle: Arc<DaemonLifecycleState>,
    semantic_intensity_leases: Arc<SemanticIntensityLeaseRegistry>,
}

#[cfg(test)]
pub(crate) fn ctx_authenticated_request_handler(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    source_refresh: Arc<CoreRefreshEngine>,
    wakeup: Arc<DaemonWakeup>,
    config: &'static dyn DaemonConfigPort,
) -> Arc<CtxAuthenticatedRequestHandler> {
    ctx_authenticated_request_handler_with_lifecycle(
        data_root,
        runtime,
        source_refresh,
        wakeup,
        config,
        Arc::new(DaemonLifecycleState::starting()),
        Arc::new(SemanticIntensityLeaseRegistry::default()),
    )
}

pub(crate) fn ctx_authenticated_request_handler_with_lifecycle(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    source_refresh: Arc<CoreRefreshEngine>,
    wakeup: Arc<DaemonWakeup>,
    config: &'static dyn DaemonConfigPort,
    lifecycle: Arc<DaemonLifecycleState>,
    semantic_intensity_leases: Arc<SemanticIntensityLeaseRegistry>,
) -> Arc<CtxAuthenticatedRequestHandler> {
    Arc::new(CtxAuthenticatedRequestHandler {
        data_root: data_root.to_path_buf(),
        runtime,
        source_refresh,
        wakeup,
        config,
        lifecycle,
        semantic_intensity_leases,
    })
}

impl AuthenticatedRequestHandler for CtxAuthenticatedRequestHandler {
    type PostWriteAction<'a> = CtxPostWriteAction<'a>;

    fn handle<'a>(
        &'a self,
        service: &ServiceId,
        request: AuthenticatedRequest,
    ) -> HandlerOutcome<Self::PostWriteAction<'a>> {
        let request = request.into_value();
        if service.as_str() == SOURCE_REFRESH_SERVICE_ID {
            return match self.handle_source_refresh(&request) {
                Ok(SourceRefreshResponse::Wire(response)) => {
                    let (response, response_barrier) = response.into_parts();
                    HandlerOutcome::with_post_write_action(
                        Ok(response),
                        CtxPostWriteAction::source_refresh(response_barrier, self),
                    )
                }
                Ok(SourceRefreshResponse::Value {
                    response,
                    wake_daemon,
                }) => HandlerOutcome::with_post_write_action(
                    Ok(response),
                    if wake_daemon {
                        CtxPostWriteAction::wake_daemon(self)
                    } else {
                        CtxPostWriteAction::source_refresh(None, self)
                    },
                ),
                Err(error) => HandlerOutcome::with_post_write_action(
                    Err(error),
                    CtxPostWriteAction::source_refresh(None, self),
                ),
            };
        }
        if service.as_str() == SEMANTIC_QUERY_SERVICE_ID {
            return HandlerOutcome::response(self.handle_semantic_query(&request));
        }
        HandlerOutcome::response(Err(anyhow!(
            "unknown authenticated IPC service `{}`",
            service.as_str()
        )))
    }
}

/// Composition owns the concrete admission barrier and scheduler wake-up.
/// The server/transport layer carries this inline token only through its
/// neutral `PostWriteAction` contract.
pub(crate) struct CtxPostWriteAction<'a> {
    kind: CtxPostWriteActionKind<'a>,
}

enum CtxPostWriteActionKind<'a> {
    None,
    SourceRefresh {
        response_barrier: Option<ctx_history_refresh::AdmissionResponseBarrier>,
        handler: &'a CtxAuthenticatedRequestHandler,
    },
    WakeDaemon {
        handler: &'a CtxAuthenticatedRequestHandler,
    },
}

impl Default for CtxPostWriteAction<'_> {
    fn default() -> Self {
        Self {
            kind: CtxPostWriteActionKind::None,
        }
    }
}

impl<'a> CtxPostWriteAction<'a> {
    fn source_refresh(
        response_barrier: Option<ctx_history_refresh::AdmissionResponseBarrier>,
        handler: &'a CtxAuthenticatedRequestHandler,
    ) -> Self {
        Self {
            kind: CtxPostWriteActionKind::SourceRefresh {
                response_barrier,
                handler,
            },
        }
    }

    fn wake_daemon(handler: &'a CtxAuthenticatedRequestHandler) -> Self {
        Self {
            kind: CtxPostWriteActionKind::WakeDaemon { handler },
        }
    }
}

impl PostWriteAction for CtxPostWriteAction<'_> {
    fn run(self) {
        match self.kind {
            CtxPostWriteActionKind::None => {}
            CtxPostWriteActionKind::SourceRefresh {
                response_barrier,
                handler,
            } => {
                wire::finish_source_refresh_response(
                    response_barrier,
                    &handler.source_refresh,
                    || handler.wakeup.signal_ipc(),
                );
            }
            CtxPostWriteActionKind::WakeDaemon { handler } => handler.wakeup.signal_ipc(),
        }
    }
}

enum SourceRefreshResponse {
    Wire(wire::WireResponse),
    Value { response: Value, wake_daemon: bool },
}

impl CtxAuthenticatedRequestHandler {
    fn lifecycle_response(&self, key: &str, value: impl Into<Value>) -> SourceRefreshResponse {
        let mut response = json!({
            "schema_version": 1,
            "ok": true,
            "owner": "daemon",
            "service": "lifecycle",
            "pid": std::process::id(),
        });
        response[key] = value.into();
        SourceRefreshResponse::Value {
            response,
            wake_daemon: false,
        }
    }

    fn handle_source_refresh(&self, request: &Value) -> Result<SourceRefreshResponse> {
        if let Some(response) =
            wire::handle_ipc_request(&self.source_refresh, &self.data_root, request)?
        {
            return Ok(SourceRefreshResponse::Wire(response));
        }
        let op = request.get("op").and_then(Value::as_str).unwrap_or("");
        if matches!(
            op,
            "semantic_intensity_acquire"
                | "semantic_intensity_renew"
                | "semantic_intensity_release"
        ) {
            let response =
                handle_semantic_intensity_lease_request(&self.semantic_intensity_leases, request)?
                    .expect("matched semantic intensity operation must be handled");
            return Ok(SourceRefreshResponse::Value {
                response: response.value,
                wake_daemon: response.wake_daemon,
            });
        }
        if op == "lifecycle_ping" {
            return Ok(self.lifecycle_response("readiness", self.lifecycle.readiness()));
        }
        if op == "lifecycle_wakeup" {
            self.wakeup.signal_ipc();
            return Ok(self.lifecycle_response("lifecycle_wakeup", "accepted"));
        }
        if op == "upgrade_handoff" {
            self.lifecycle.mark_stopping();
            self.wakeup.signal_shutdown();
            return Ok(self.lifecycle_response("upgrade_handoff", "accepted"));
        }
        if matches!(op, "shutdown" | "supervisor_handoff") {
            let config = self.config.load(&self.data_root)?;
            if config.daemon.enabled == (op == "shutdown") {
                return Err(anyhow!(
                    "daemon {op} is not allowed by current configuration"
                ));
            }
            self.lifecycle.mark_stopping();
            self.wakeup.signal_shutdown();
            return Ok(self.lifecycle_response(op, "accepted"));
        }
        if op == "ping" {
            let published_generation =
                read_daemon_job_status(&daemon_source_backed_refresh_job_path(&self.data_root))
                    .and_then(|job| {
                        job.get("published_generation")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
            return Ok(SourceRefreshResponse::Value {
                response: compact_json(json!({
                "ok": true,
                "schema_version": 1,
                "owner": "daemon",
                "service": "source_refresh",
                "pid": std::process::id(),
                "published_generation": published_generation,
                })),
                wake_daemon: false,
            });
        }
        Err(anyhow!("unknown daemon source refresh operation `{op}`"))
    }

    fn handle_semantic_query(&self, request: &Value) -> Result<Value> {
        let op = request.get("op").and_then(Value::as_str).unwrap_or("");
        if op == "ping" {
            let (embedding_runtime, busy) = self.runtime.try_runtime_status_json()?;
            return Ok(compact_json(json!({
                "ok": true,
                "schema_version": 1,
                "model_key": semantic_model_key(),
                "embedding_runtime": embedding_runtime,
                "busy": busy,
            })));
        }
        if op != "embed_query" {
            return Err(anyhow!("unknown daemon query operation `{op}`"));
        }
        let model_key = request
            .get("model_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        if model_key != semantic_model_key() {
            return Err(anyhow!("daemon query model key mismatch"));
        }
        let text = request
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            return Err(anyhow!("daemon query text is empty"));
        }
        let started = Instant::now();
        let model_config = self.config.semantic_model_config(&self.data_root);
        if !self.runtime.is_loaded()
            && !semantic_model_cache_available(model_config.paths().model_cache_dir())
        {
            return Err(anyhow!(
                "semantic model cache is not available to daemon query service"
            ));
        }
        self.runtime.ensure_loaded_from_cache(&model_config)?;
        let (embedding, embedding_runtime) =
            self.runtime.embed_query(&model_config, text.to_owned())?;
        let query_embed_ms = started.elapsed().as_millis() as u64;
        Ok(compact_json(json!({
            "ok": true,
            "model_key": semantic_model_key(),
            "embedding_runtime": embedding_runtime.to_json(),
            "query_embed_ms": query_embed_ms,
            "embedding": embedding,
        })))
    }
}
