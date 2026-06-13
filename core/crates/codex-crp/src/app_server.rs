use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

#[path = "app_server/auth_lock.rs"]
mod auth_lock;
#[path = "app_server/types.rs"]
mod types;

use self::auth_lock::{acquire_codex_runtime_locks, CodexRuntimeLocks};
pub use self::types::*;

const CODEX_APP_SERVER_BASE_ARGS: [&str; 4] = ["-s", "danger-full-access", "-a", "never"];
const CODEX_RAW_EVENT_DUMP_ENV: &str = "CODEX_CRP_DUMP_CODEX_EVENTS_PATH";
const AMBIENT_PROVIDER_SESSION_ENV_DENYLIST: &[&str] = &[
    "CTX_PROVIDER_SESSION_REF",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDE_THREAD_ID",
    "GEMINI_SESSION_ID",
    "GEMINI_THREAD_ID",
    "ACP_SESSION_ID",
];

static APP_SERVER_EVENT_DUMP: OnceLock<StdMutex<Option<std::io::BufWriter<std::fs::File>>>> =
    OnceLock::new();
static APP_SERVER_EVENT_DUMP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct PendingResponse {
    respond_to: oneshot::Sender<Result<Value>>,
    method: String,
}

pub struct AppServerClient {
    pending: Arc<Mutex<HashMap<i64, PendingResponse>>>,
    stdin: Arc<Mutex<BufWriter<ChildStdin>>>,
    inbound_rx: mpsc::UnboundedReceiver<AppServerInbound>,
    child: Option<Child>,
    next_id: i64,
    _runtime_locks: Option<CodexRuntimeLocks>,
}

fn maybe_dump_app_server_message(direction: &str, value: &Value) {
    let Ok(path) = std::env::var(CODEX_RAW_EVENT_DUMP_ENV) else {
        return;
    };
    let writer = APP_SERVER_EVENT_DUMP
        .get_or_init(|| StdMutex::new(open_dump_writer(&path, CODEX_RAW_EVENT_DUMP_ENV)));

    let Ok(mut writer) = writer.lock() else {
        return;
    };
    let Some(writer) = writer.as_mut() else {
        return;
    };
    let event = json!({
        "i": APP_SERVER_EVENT_DUMP_SEQ.fetch_add(1, Ordering::Relaxed),
        "direction": direction,
        "event": value,
    });
    if serde_json::to_writer(&mut *writer, &event).is_ok() {
        let _ = writer.write_all(b"\n");
        let _ = writer.flush();
    }
}

fn open_dump_writer(path: &str, env_name: &str) -> Option<std::io::BufWriter<std::fs::File>> {
    if let Some(parent) = Path::new(path).parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %path,
                parent = %parent.display(),
                error = %err,
                "failed to create {env_name} parent; disabling app-server event dumps"
            );
            return None;
        }
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => Some(std::io::BufWriter::new(file)),
        Err(err) => {
            tracing::warn!(
                path = %path,
                error = %err,
                "failed to open {env_name}; disabling app-server event dumps"
            );
            None
        }
    }
}

pub fn build_codex_app_server_args(config_overrides: &[String]) -> Vec<String> {
    let mut args: Vec<String> = CODEX_APP_SERVER_BASE_ARGS
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    args.push("app-server".to_string());
    for entry in config_overrides {
        args.push("-c".to_string());
        args.push(entry.clone());
    }
    args
}

impl AppServerClient {
    pub async fn start(workdir: &Path, config_overrides: &[String]) -> Result<Self> {
        let codex_bin = std::env::var("CTX_CODEX_BIN_PATH")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!("CTX_CODEX_BIN_PATH must be set to the absolute codex-cli runtime path")
            })?;
        if !Path::new(&codex_bin).is_absolute() {
            anyhow::bail!("CTX_CODEX_BIN_PATH must be absolute, got `{codex_bin}`");
        }
        let runtime_locks = acquire_codex_runtime_locks()?;

        let mut command = Command::new(&codex_bin);
        command
            .args(build_codex_app_server_args(config_overrides))
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in AMBIENT_PROVIDER_SESSION_ENV_DENYLIST {
            command.env_remove(key);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("spawning codex app-server via `{codex_bin}`"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("codex app-server stderr unavailable"))?;

        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

        tokio::spawn(stdout_reader_task(
            stdout,
            inbound_tx.clone(),
            Arc::clone(&pending),
        ));
        tokio::spawn(stderr_reader_task(stderr));

        let mut client = Self {
            pending,
            stdin: Arc::new(Mutex::new(BufWriter::new(stdin))),
            inbound_rx,
            child: Some(child),
            next_id: 1,
            _runtime_locks: runtime_locks,
        };

        client
            .request::<Value>(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "codex-crp",
                        "title": "ctx",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "experimentalApi": true,
                        "optOutNotificationMethods": [],
                    }
                }),
            )
            .await?;
        client.notify("initialized", json!({})).await?;

        Ok(client)
    }

    pub async fn request<T>(&mut self, method: &str, params: Value) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(
            id,
            PendingResponse {
                respond_to: tx,
                method: method.to_string(),
            },
        );
        if let Err(err) = self
            .send_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
            }))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(err);
        }
        let result = rx
            .await
            .map_err(|_| anyhow!("app-server response channel closed for {method}"))??;
        serde_json::from_value(result).with_context(|| format!("decoding `{method}` response"))
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    pub async fn reject_request(&mut self, id: AppServerRequestId, message: String) -> Result<()> {
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "id": id.json_value(),
            "error": {
                "code": -32000,
                "message": message,
            }
        }))
        .await
    }

    pub async fn next_inbound(&mut self) -> Option<AppServerInbound> {
        self.inbound_rx.recv().await
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self._runtime_locks.take();
    }

    async fn send_json(&mut self, value: &Value) -> Result<()> {
        maybe_dump_app_server_message("outbound", value);
        let mut stdin = self.stdin.lock().await;
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let runtime_locks = self._runtime_locks.take();
        let _ = child.start_kill();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _runtime_locks = runtime_locks;
                    let _ = child.wait().await;
                });
            }
            Err(_) => {
                std::mem::forget(runtime_locks);
            }
        }
    }
}

async fn stdout_reader_task(
    stdout: tokio::process::ChildStdout,
    inbound_tx: mpsc::UnboundedSender<AppServerInbound>,
    pending: Arc<Mutex<HashMap<i64, PendingResponse>>>,
) {
    stdout_reader_loop(stdout, inbound_tx, pending).await;
}

async fn stdout_reader_loop(
    stdout: impl AsyncRead + Unpin,
    inbound_tx: mpsc::UnboundedSender<AppServerInbound>,
    pending: Arc<Mutex<HashMap<i64, PendingResponse>>>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                fail_pending_responses(&pending, "codex app-server stdout closed before response")
                    .await;
                break;
            }
            Err(err) => {
                fail_pending_responses(
                    &pending,
                    format!("failed to read codex app-server stdout before response: {err}"),
                )
                .await;
                break;
            }
        };
        let parsed: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(?err, "failed to decode app-server line");
                continue;
            }
        };
        maybe_dump_app_server_message("inbound", &parsed);
        let Some(object) = parsed.as_object() else {
            continue;
        };

        if let Some(id) = object.get("id") {
            if object.get("method").is_none() {
                if let Some(response_id) = response_id_from_value(id) {
                    let pending_entry = pending.lock().await.remove(&response_id);
                    if let Some(pending_entry) = pending_entry {
                        let result = if let Some(error) = object.get("error") {
                            Err(anyhow!(
                                "{}: {}",
                                pending_entry.method,
                                error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown app-server error")
                            ))
                        } else {
                            Ok(object.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = pending_entry.respond_to.send(result);
                    }
                }
                continue;
            }

            let method = object
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            if let Some(request_id) = AppServerRequestId::from_inbound_value(id) {
                let _ = inbound_tx.send(AppServerInbound::Request {
                    id: request_id,
                    method,
                    params,
                });
            }
            continue;
        }

        if let Some(method) = object.get("method").and_then(Value::as_str) {
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            let _ = inbound_tx.send(AppServerInbound::Notification {
                method: method.to_string(),
                params,
            });
        }
    }
}

async fn fail_pending_responses(
    pending: &Arc<Mutex<HashMap<i64, PendingResponse>>>,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    let mut pending = pending.lock().await;
    for (_id, pending_entry) in pending.drain() {
        let _ = pending_entry
            .respond_to
            .send(Err(anyhow!("{}: {reason}", pending_entry.method)));
    }
}

async fn stderr_reader_task(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        eprintln!("{line}");
    }
}

fn response_id_from_value(value: &Value) -> Option<i64> {
    if let Some(id) = value.as_i64() {
        return Some(id);
    }
    value.as_str().and_then(|id| id.parse::<i64>().ok())
}

#[cfg(test)]
impl AppServerClient {
    pub fn test_stub() -> Self {
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cat");
        let stdin = child.stdin.take().expect("cat stdin");
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            stdin: Arc::new(Mutex::new(BufWriter::new(stdin))),
            inbound_rx: {
                let (_tx, rx) = mpsc::unbounded_channel();
                rx
            },
            child: Some(child),
            next_id: 1,
            _runtime_locks: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_codex_app_server_args, open_dump_writer, response_id_from_value, stdout_reader_loop,
        AppServerRequestId, PendingResponse, CODEX_RAW_EVENT_DUMP_ENV,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot, Mutex};

    #[test]
    fn inbound_request_id_preserves_original_type() {
        assert_eq!(
            AppServerRequestId::from_inbound_value(&json!(42)),
            Some(AppServerRequestId::Integer(42))
        );
        assert_eq!(
            AppServerRequestId::from_inbound_value(&json!("request-42")),
            Some(AppServerRequestId::String("request-42".to_string()))
        );
    }

    #[test]
    fn inbound_request_id_serializes_without_coercion() {
        assert_eq!(AppServerRequestId::Integer(7).json_value(), json!(7));
        assert_eq!(
            AppServerRequestId::String("server-req".to_string()).json_value(),
            json!("server-req")
        );
    }

    #[test]
    fn response_id_accepts_numeric_strings_only() {
        assert_eq!(response_id_from_value(&json!(12)), Some(12));
        assert_eq!(response_id_from_value(&json!("12")), Some(12));
        assert_eq!(response_id_from_value(&json!("req-12")), None);
    }

    #[test]
    fn app_server_args_append_launch_config_to_subcommand() {
        assert_eq!(
            build_codex_app_server_args(&["stream_idle_timeout_ms=120000".to_string()]),
            vec![
                "-s",
                "danger-full-access",
                "-a",
                "never",
                "app-server",
                "-c",
                "stream_idle_timeout_ms=120000",
            ]
        );
    }

    #[test]
    fn app_server_dump_writer_creates_parent_dirs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("nested").join("codex-events.jsonl");
        let writer = open_dump_writer(
            path.to_str().expect("test path should be utf-8"),
            CODEX_RAW_EVENT_DUMP_ENV,
        )
        .expect("writer should open");
        drop(writer);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn pending_request_fails_when_app_server_stdout_closes() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (respond_to, response) = oneshot::channel();
        pending.lock().await.insert(
            7,
            PendingResponse {
                respond_to,
                method: "thread/start".to_string(),
            },
        );
        let (inbound_tx, _inbound_rx) = mpsc::unbounded_channel();

        stdout_reader_loop(tokio::io::empty(), inbound_tx, Arc::clone(&pending)).await;

        let err = response
            .await
            .expect("pending response should be completed")
            .expect_err("stdout close should fail request");
        assert!(
            err.to_string()
                .contains("thread/start: codex app-server stdout closed before response"),
            "unexpected error: {err}"
        );
        assert!(pending.lock().await.is_empty());
    }
}
