#[path = "terminals_gateway.rs"]
mod terminals_gateway;
#[path = "terminals_handle.rs"]
mod terminals_handle;
#[path = "terminals_manager.rs"]
mod terminals_manager;
#[path = "terminals_remote.rs"]
mod terminals_remote;

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, RootCertStore, SignatureScheme};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, connect_async_tls_with_config, Connector};
use uuid::Uuid;

use ctx_core::env::DAEMON_AUTH_ENV_VARS;
use ctx_core::ids::{SessionId, TaskId, TerminalId, WorkspaceId, WorktreeId};
use ctx_core::models::{TerminalSession, TerminalStatus};
pub use ctx_route_contracts::terminals::DEFAULT_OUTPUT_TAIL_BYTES;
use terminals_gateway::connect_terminal_gateway;
use terminals_handle::build_stream_path;
use terminals_manager::push_output;
pub use terminals_manager::TerminalManagerStats;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

fn scrub_daemon_auth_env(cmd: &mut CommandBuilder) {
    for key in DAEMON_AUTH_ENV_VARS {
        cmd.env_remove(key);
    }
}
const TERMINAL_STREAM_TOKEN_TTL_SECS: i64 = 30;
const TERMINAL_REAPER_INTERVAL: Duration = Duration::from_secs(60);

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, name: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(mutex = name, "mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

fn terminal_idle_timeout() -> Option<Duration> {
    let raw = std::env::var("CTX_TERMINAL_IDLE_TIMEOUT_SECS").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let secs = trimmed.parse::<u64>().ok()?;
    if secs == 0 {
        return None;
    }
    Some(Duration::from_secs(secs))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    Resize { cols: u16, rows: u16 },
    Input { data: String },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Status {
        status: TerminalStatus,
        exit_code: Option<i32>,
    },
    Pong,
}

#[derive(Debug, Clone)]
pub struct TerminalCreateRequest {
    pub workspace_id: WorkspaceId,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: PathBuf,
    pub shell: String,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub env: HashMap<String, String>,
    pub native_container: Option<NativeContainerTerminalSpec>,
    pub shared_vm_container: Option<SharedVmContainerTerminalSpec>,
}

#[derive(Debug, Clone)]
pub struct NativeContainerTerminalSpec {
    pub cli_bin: PathBuf,
    pub cli_env: HashMap<String, String>,
    pub container_name: String,
    pub workdir: String,
    pub user: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SharedVmContainerTerminalSpec {
    pub helper_path: PathBuf,
    pub data_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub workdir: String,
    pub user: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TerminalStatusEvent {
    pub status: TerminalStatus,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct RemoteTerminalRequest {
    pub terminal_id: TerminalId,
    pub gateway_url: String,
    pub worker_id: String,
    pub token: Option<String>,
    pub gateway_ca_pem: Option<String>,
}

#[derive(Debug, Clone)]
struct TerminalRuntime {
    status: TerminalStatus,
    exit_code: Option<i32>,
    updated_at: chrono::DateTime<chrono::Utc>,
    last_activity: chrono::DateTime<chrono::Utc>,
    connected_clients: usize,
}

enum RemoteTerminalOutgoing {
    Binary(Vec<u8>),
    Text(String),
    Close,
}

enum TerminalBackend {
    Local {
        input_tx: mpsc::UnboundedSender<Vec<u8>>,
        master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
        child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    },
    Remote {
        outbound_tx: mpsc::UnboundedSender<RemoteTerminalOutgoing>,
    },
}

pub struct TerminalSessionHandle {
    info: TerminalSession,
    stream_tokens: Arc<Mutex<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    container_backed: bool,
    runtime: Arc<Mutex<TerminalRuntime>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    status_tx: broadcast::Sender<TerminalStatusEvent>,
    output_buffer: Arc<Mutex<VecDeque<u8>>>,
    backend: TerminalBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalStreamAccessError {
    NotFound,
    Unauthorized,
}

#[derive(Clone)]
pub struct TerminalStreamSession {
    handle: Arc<TerminalSessionHandle>,
}

pub struct TerminalStreamConnection {
    pub session: TerminalStreamSession,
    pub output_rx: TerminalStreamOutputReceiver,
    pub status_rx: TerminalStreamStatusReceiver,
    pub initial_snapshot: TerminalStreamInitialSnapshot,
    _client_guard: TerminalStreamClientGuard,
}

#[derive(Clone, Debug)]
pub struct TerminalStreamInitialSnapshot {
    pub status: TerminalStatus,
    pub exit_code: Option<i32>,
    pub output_tail: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TerminalStreamStatusUpdate {
    pub status: TerminalStatus,
    pub exit_code: Option<i32>,
}

pub enum TerminalStreamOutputRecv {
    Bytes(Vec<u8>),
    Lagged,
    Closed,
}

pub enum TerminalStreamStatusRecv {
    Update(TerminalStreamStatusUpdate),
    Lagged,
    Closed,
}

pub struct TerminalStreamOutputReceiver {
    inner: tokio::sync::broadcast::Receiver<Vec<u8>>,
}

pub struct TerminalStreamStatusReceiver {
    inner: tokio::sync::broadcast::Receiver<TerminalStatusEvent>,
}

struct TerminalStreamClientGuard {
    handle: Arc<TerminalSessionHandle>,
}

impl Drop for TerminalStreamClientGuard {
    fn drop(&mut self) {
        self.handle.mark_client_disconnected();
    }
}

impl TerminalStreamSession {
    fn new(handle: Arc<TerminalSessionHandle>) -> Self {
        Self { handle }
    }

    pub fn connect(&self, tail_bytes: usize) -> TerminalStreamConnection {
        self.handle.mark_client_connected();
        let output_rx = TerminalStreamOutputReceiver {
            inner: self.handle.output_receiver(),
        };
        let status_rx = TerminalStreamStatusReceiver {
            inner: self.handle.status_receiver(),
        };
        TerminalStreamConnection {
            session: self.clone(),
            output_rx,
            status_rx,
            initial_snapshot: self.initial_snapshot(tail_bytes),
            _client_guard: TerminalStreamClientGuard {
                handle: Arc::clone(&self.handle),
            },
        }
    }

    pub fn initial_snapshot(&self, tail_bytes: usize) -> TerminalStreamInitialSnapshot {
        let snapshot = self.handle.snapshot();
        TerminalStreamInitialSnapshot {
            status: snapshot.status,
            exit_code: snapshot.exit_code,
            output_tail: self.output_tail(tail_bytes),
        }
    }

    pub fn output_tail(&self, tail_bytes: usize) -> Vec<u8> {
        self.handle.output_snapshot_tail(tail_bytes)
    }

    pub fn write_input(&self, data: Vec<u8>) {
        self.handle.send_input(data);
    }

    pub fn resize_terminal(&self, cols: u16, rows: u16) -> Result<()> {
        self.handle.resize(cols, rows)
    }
}

impl TerminalStreamOutputReceiver {
    pub async fn recv(&mut self) -> TerminalStreamOutputRecv {
        match self.inner.recv().await {
            Ok(bytes) => TerminalStreamOutputRecv::Bytes(bytes),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                TerminalStreamOutputRecv::Lagged
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                TerminalStreamOutputRecv::Closed
            }
        }
    }
}

impl TerminalStreamStatusReceiver {
    pub async fn recv(&mut self) -> TerminalStreamStatusRecv {
        match self.inner.recv().await {
            Ok(event) => TerminalStreamStatusRecv::Update(TerminalStreamStatusUpdate {
                status: event.status,
                exit_code: event.exit_code,
            }),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                TerminalStreamStatusRecv::Lagged
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                TerminalStreamStatusRecv::Closed
            }
        }
    }
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: tokio::sync::Mutex<HashMap<TerminalId, Arc<TerminalSessionHandle>>>,
}

impl TerminalManager {
    pub async fn create(&self, req: TerminalCreateRequest) -> Result<Arc<TerminalSessionHandle>> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: req.rows.unwrap_or(DEFAULT_ROWS),
                cols: req.cols.unwrap_or(DEFAULT_COLS),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open pty")?;

        let mut cmd = if let Some(native_container) = &req.native_container {
            let mut cmd = CommandBuilder::new(native_container.cli_bin.clone());
            for (key, value) in &native_container.cli_env {
                cmd.env(key, value);
            }

            // Container env.
            cmd.arg("exec");
            cmd.arg("-i");
            cmd.arg("-t");
            cmd.arg("--workdir");
            cmd.arg(native_container.workdir.clone());
            cmd.arg("--env");
            cmd.arg("TERM=xterm-256color");
            if let Some(user) = native_container.user.as_ref() {
                cmd.arg("--user");
                cmd.arg(user.clone());
            }
            for (key, value) in &req.env {
                cmd.arg("--env");
                cmd.arg(format!("{key}={value}"));
            }
            cmd.arg(native_container.container_name.clone());
            cmd.arg(req.shell.clone());
            cmd
        } else if let Some(shared_vm_container) = &req.shared_vm_container {
            let mut cmd = CommandBuilder::new(shared_vm_container.helper_path.clone());
            cmd.arg("shared-vm-exec");
            cmd.arg("--data-root");
            cmd.arg(shared_vm_container.data_root.clone());
            cmd.arg("--cwd");
            cmd.arg("/");
            cmd.arg("--command");
            cmd.arg(ctx_sandbox_container_runtime::SHARED_VM_SANDBOX_CLI_GUEST_BIN);
            cmd.arg("--user");
            cmd.arg("root");
            if let Ok(sandbox_env) = ctx_sandbox_container_runtime::sandbox_cli_env_for_mode(
                &shared_vm_container.data_root,
                &ctx_sandbox_container_runtime::SandboxCommandMode::SharedVm {
                    helper_path: shared_vm_container.helper_path.clone(),
                },
            ) {
                let mut env_pairs = sandbox_env.into_iter().collect::<Vec<_>>();
                env_pairs.sort_by(|(left, _), (right, _)| left.cmp(right));
                for (key, value) in env_pairs {
                    cmd.arg("--env");
                    cmd.arg(format!("{key}={value}"));
                }
            }
            cmd.arg("--pty");
            cmd.arg("--");
            cmd.arg("exec");
            cmd.arg("-i");
            cmd.arg("-t");
            cmd.arg("--workdir");
            cmd.arg(shared_vm_container.workdir.clone());
            cmd.arg("--env");
            cmd.arg("TERM=xterm-256color");
            if let Some(user) = shared_vm_container.user.as_ref() {
                cmd.arg("--user");
                cmd.arg(user.clone());
            }
            for (key, value) in &req.env {
                cmd.arg("--env");
                cmd.arg(format!("{key}={value}"));
            }
            cmd.arg(format!(
                "ctx-harness-{}",
                shared_vm_container.workspace_id.0
            ));
            cmd.arg(req.shell.clone());
            cmd
        } else {
            let mut cmd = CommandBuilder::new(req.shell.clone());
            cmd.cwd(req.cwd.clone());
            cmd.env("TERM", "xterm-256color");
            for (key, value) in &req.env {
                cmd.env(key, value);
            }
            cmd
        };

        scrub_daemon_auth_env(&mut cmd);
        let child = pair.slave.spawn_command(cmd).context("spawn terminal")?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let mut writer = pair.master.take_writer().context("take pty writer")?;

        let (output_tx, _) = broadcast::channel(1024);
        let (status_tx, _) = broadcast::channel(16);
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let output_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));

        let now = Utc::now();
        let runtime = Arc::new(Mutex::new(TerminalRuntime {
            status: TerminalStatus::Running,
            exit_code: None,
            updated_at: now,
            last_activity: now,
            connected_clients: 0,
        }));

        let output_buffer_clone = output_buffer.clone();
        let output_tx_clone = output_tx.clone();
        let runtime_output = runtime.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let bytes = &buf[..n];
                        push_output(
                            &output_buffer_clone,
                            &output_tx_clone,
                            &runtime_output,
                            bytes,
                        );
                    }
                    Err(_) => break,
                }
            }
        });

        std::thread::spawn(move || {
            while let Some(data) = input_rx.blocking_recv() {
                if writer.write_all(&data).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });

        let runtime_clone = runtime.clone();
        let status_tx_clone = status_tx.clone();
        let child_arc = Arc::new(Mutex::new(child));
        let child_arc_clone = child_arc.clone();

        std::thread::spawn(move || loop {
            let exit: Option<portable_pty::ExitStatus> = {
                let mut child = lock_or_recover(child_arc_clone.as_ref(), "terminal child");
                child.try_wait().ok().flatten()
            };
            if let Some(status) = exit {
                let exit_code = i32::try_from(status.exit_code()).ok();
                let mut runtime = lock_or_recover(runtime_clone.as_ref(), "terminal runtime");
                runtime.status = TerminalStatus::Exited;
                runtime.exit_code = exit_code;
                runtime.updated_at = Utc::now();
                runtime.last_activity = runtime.updated_at;
                let _ = status_tx_clone.send(TerminalStatusEvent {
                    status: TerminalStatus::Exited,
                    exit_code,
                });
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        });

        let id = TerminalId::new();
        let title = PathBuf::from(&req.shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("terminal")
            .to_string();

        let info = TerminalSession {
            id,
            workspace_id: req.workspace_id,
            task_id: req.task_id,
            session_id: req.session_id,
            worktree_id: req.worktree_id,
            cwd: req.cwd.to_string_lossy().to_string(),
            shell: req.shell,
            title,
            status: TerminalStatus::Running,
            exit_code: None,
            stream_path: build_stream_path(id),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let session = Arc::new(TerminalSessionHandle {
            info,
            stream_tokens: Arc::new(Mutex::new(HashMap::new())),
            container_backed: req.native_container.is_some() || req.shared_vm_container.is_some(),
            runtime,
            output_tx,
            status_tx,
            output_buffer,
            backend: TerminalBackend::Local {
                input_tx,
                master: Arc::new(Mutex::new(pair.master)),
                child: child_arc,
            },
        });

        let mut sessions = self.sessions.lock().await;
        sessions.insert(id, session.clone());
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn scrub_daemon_auth_env_removes_sensitive_tokens_from_pty_commands() {
        let _auth = ScopedEnvVar::set("CTX_AUTH_TOKEN", "daemon-token");
        let _mcp = ScopedEnvVar::set("CTX_MCP_TOKEN", "mcp-token");
        let _shutdown = ScopedEnvVar::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "shutdown-token");
        let mut cmd = CommandBuilder::new("/bin/sh");

        scrub_daemon_auth_env(&mut cmd);

        for key in DAEMON_AUTH_ENV_VARS {
            assert_eq!(cmd.get_env(key), None, "expected {key} to be removed");
        }
    }

    #[test]
    fn terminal_stream_initial_snapshot_clamps_tail() {
        let session =
            TerminalStreamSession::new(TerminalSessionHandle::test_handle_with_output(b"abcdef"));

        assert_eq!(session.initial_snapshot(0).output_tail, b"");
        assert_eq!(session.initial_snapshot(3).output_tail, b"def");
        assert_eq!(session.initial_snapshot(64).output_tail, b"abcdef");
    }

    #[tokio::test]
    async fn terminal_stream_status_receiver_maps_runtime_events() {
        let session =
            TerminalStreamSession::new(TerminalSessionHandle::test_handle_with_output(b""));
        let mut connection = session.connect(0);

        session.handle.mark_exited(Some(7));

        match connection.status_rx.recv().await {
            TerminalStreamStatusRecv::Update(update) => {
                assert!(matches!(update.status, TerminalStatus::Exited));
                assert_eq!(update.exit_code, Some(7));
            }
            TerminalStreamStatusRecv::Lagged | TerminalStreamStatusRecv::Closed => {
                panic!("expected status update")
            }
        }
    }

    #[tokio::test]
    async fn terminal_stream_token_admission_requires_valid_one_shot_token() {
        let handle = TerminalSessionHandle::test_handle_with_output(b"");
        let terminal_id = handle.snapshot().id;
        let manager = manager_with_handle(Arc::clone(&handle));

        assert_eq!(
            stream_access_error(
                manager
                    .require_stream_access(terminal_id, "bad-token")
                    .await
            ),
            TerminalStreamAccessError::Unauthorized
        );

        let (stream_path, _) = handle.issue_stream_connect_path();
        let token = stream_path
            .split("token=")
            .nth(1)
            .expect("test stream path should contain token");
        manager
            .require_stream_access(terminal_id, token)
            .await
            .expect("fresh stream token should be accepted");
        assert_eq!(
            stream_access_error(manager.require_stream_access(terminal_id, token).await),
            TerminalStreamAccessError::Unauthorized
        );
    }

    #[tokio::test]
    async fn terminal_stream_access_reports_missing_terminal() {
        let manager = TerminalManager::default();

        assert_eq!(
            stream_access_error(
                manager
                    .require_stream_access(TerminalId::new(), "token")
                    .await
            ),
            TerminalStreamAccessError::NotFound
        );
    }

    #[tokio::test]
    async fn terminal_stream_connection_updates_client_count_until_dropped() {
        let handle = TerminalSessionHandle::test_handle_with_output(b"");
        let manager = manager_with_handle(Arc::clone(&handle));
        let session = TerminalStreamSession::new(handle);

        assert_eq!(manager.stats().await.connected_clients, 0);
        let connection = session.connect(0);
        assert_eq!(manager.stats().await.connected_clients, 1);
        drop(connection);
        assert_eq!(manager.stats().await.connected_clients, 0);
    }

    fn manager_with_handle(handle: Arc<TerminalSessionHandle>) -> TerminalManager {
        let terminal_id = handle.snapshot().id;
        TerminalManager {
            sessions: tokio::sync::Mutex::new(HashMap::from([(terminal_id, handle)])),
        }
    }

    fn stream_access_error(
        result: Result<TerminalStreamSession, TerminalStreamAccessError>,
    ) -> TerminalStreamAccessError {
        match result {
            Ok(_) => panic!("expected terminal stream access error"),
            Err(error) => error,
        }
    }
}
