use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_FPS: u32 = 30;
const DEFAULT_IDLE_SECS: u64 = 30 * 60;
const REAPER_INTERVAL_SECS: u64 = 60;
const WEB_SESSION_STREAM_TOKEN_TTL_SECS: i64 = 30;
pub const WEB_SESSION_WORKER_AUTH_HEADER: &str = "x-ctx-worker-auth";

mod access;
mod handle;
mod launch_policy;
mod runtime_support;
mod signal;
mod types;
mod view;
mod worker_bundle;

#[cfg(test)]
mod tests;

pub use access::{WebSessionAccessError, WebSessionViewConnectPath, WebSessionViewPage};
pub use handle::WebSessionHandle;
use handle::WebSessionRuntime;
pub use launch_policy::{
    validate_web_session_host_session, validate_web_session_host_worktree,
    validate_web_session_launch_scope, validate_web_session_url, WebSessionLaunchPolicyError,
    WebSessionLaunchPolicyErrorKind,
};
use runtime_support::{
    allocate_port, build_run_payload, build_signal_connect_path, build_stream_connect_path,
    build_stream_path, log_stream,
};
pub use signal::{
    WebSessionSignalBridgeError, WebSessionSignalUpstream, WebSessionSignalViewerGuard,
};
pub use types::{
    WebSessionCreateRequest, WebSessionInfo, WebSessionRunRequest, WebSessionRunResponse,
    WebSessionStatus, WebSessionViewport,
};
pub use view::render_web_session_view;
pub use worker_bundle::ensure_worker_bundle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSessionActionError {
    NotFound,
    Internal,
}

#[derive(Debug, Clone)]
pub struct NodeRuntimeSpec {
    pub node_bin: PathBuf,
    pub npm_cli_js: PathBuf,
}

pub struct WorkerBundle {
    pub worker_path: PathBuf,
    pub node_modules_path: PathBuf,
}

pub struct WebSessionManager {
    sessions: Mutex<HashMap<String, Arc<WebSessionHandle>>>,
    client: Client,
    next_display: Mutex<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSessionManagerStats {
    pub session_count: usize,
    pub running: usize,
    pub closed: usize,
    pub error: usize,
    pub total_viewers: u32,
    pub active_children: usize,
}

impl WebSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            client: Client::new(),
            next_display: Mutex::new(90),
        }
    }

    pub async fn stats(&self) -> WebSessionManagerStats {
        let handles = {
            let sessions = self.sessions.lock().await;
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut running = 0;
        let mut closed = 0;
        let mut error = 0;
        let mut total_viewers = 0;
        let mut active_children = 0;
        for handle in handles.iter() {
            let runtime = handle.runtime.lock().await;
            match runtime.status {
                WebSessionStatus::Running => running += 1,
                WebSessionStatus::Closed => closed += 1,
                WebSessionStatus::Error => error += 1,
            }
            total_viewers += runtime.viewers;
            if runtime.child.is_some() {
                active_children += 1;
            }
        }
        WebSessionManagerStats {
            session_count: handles.len(),
            running,
            closed,
            error,
            total_viewers,
            active_children,
        }
    }

    pub async fn list(&self) -> Vec<WebSessionInfo> {
        let handles = {
            let sessions = self.sessions.lock().await;
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut out = Vec::with_capacity(handles.len());
        for session in handles {
            out.push(session.snapshot().await);
        }
        out
    }

    pub async fn get(&self, id: &str) -> Option<Arc<WebSessionHandle>> {
        let sessions = self.sessions.lock().await;
        sessions.get(id).cloned()
    }

    pub async fn get_info(&self, id: &str) -> Option<WebSessionInfo> {
        let handle = self.get(id).await?;
        Some(handle.snapshot().await)
    }

    pub async fn create(&self, req: WebSessionCreateRequest) -> Result<Arc<WebSessionHandle>> {
        let id = Uuid::new_v4().to_string();
        let worker_auth_secret = Uuid::new_v4().to_string();
        let viewport = req.viewport.clone().unwrap_or(WebSessionViewport {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        });
        let fps = req.fps.unwrap_or(DEFAULT_FPS);
        let display = self.next_display().await?;
        let worker_port = allocate_port()?;

        let stream_path = build_stream_path(&id);
        let created_at = Utc::now();

        let info = WebSessionInfo {
            id: id.clone(),
            kind: "web".to_string(),
            session_id: req.session_id.clone(),
            worktree_id: req.worktree_id.clone(),
            status: WebSessionStatus::Running,
            created_at,
            updated_at: created_at,
            last_activity: created_at,
            url: req.url.clone(),
            viewport: viewport.clone(),
            fps,
            viewers: 0,
            stream_path,
            stream_url: None,
        };

        let runtime = WebSessionRuntime {
            status: WebSessionStatus::Running,
            updated_at: created_at,
            last_activity: created_at,
            viewers: 0,
            worker_port,
            child: None,
            work_dir: req.work_dir.clone(),
        };

        let handle = Arc::new(WebSessionHandle {
            info,
            stream_tokens: Arc::new(Mutex::new(HashMap::new())),
            worker_auth_secret,
            runtime: Arc::new(Mutex::new(runtime)),
            run_lock: Arc::new(Mutex::new(())),
        });

        self.spawn_worker(&handle, &req, worker_port, &display)
            .await?;
        self.await_worker_ready(worker_port, handle.worker_auth_secret())
            .await?;

        let mut sessions = self.sessions.lock().await;
        sessions.insert(id.clone(), handle.clone());
        Ok(handle)
    }

    pub async fn run(&self, id: &str, req: WebSessionRunRequest) -> Result<WebSessionRunResponse> {
        let handle = self.get(id).await.context("session not found")?;
        self.run_for_handle(handle, req).await
    }

    pub async fn run_action(
        &self,
        id: &str,
        req: WebSessionRunRequest,
    ) -> Result<WebSessionRunResponse, WebSessionActionError> {
        let handle = self.get(id).await.ok_or(WebSessionActionError::NotFound)?;
        self.run_for_handle(handle, req)
            .await
            .map_err(|_| WebSessionActionError::Internal)
    }

    async fn run_for_handle(
        &self,
        handle: Arc<WebSessionHandle>,
        req: WebSessionRunRequest,
    ) -> Result<WebSessionRunResponse> {
        let _guard = handle.run_lock.lock().await;
        handle.touch().await;

        let payload = build_run_payload(&handle, req).await?;
        let port = handle.worker_port().await;
        let url = format!("http://127.0.0.1:{port}/run");

        let resp = self
            .client
            .post(&url)
            .header(
                WEB_SESSION_WORKER_AUTH_HEADER,
                handle.worker_auth_secret().to_string(),
            )
            .json(&payload)
            .send()
            .await
            .context("sending run request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Ok(WebSessionRunResponse {
                ok: false,
                result: None,
                error: Some(format!("worker error: {body}")),
            });
        }

        let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(WebSessionRunResponse {
            ok: value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
            result: value.get("result").cloned(),
            error: value
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
    }

    pub async fn eval(&self, id: &str, req: WebSessionRunRequest) -> Result<WebSessionRunResponse> {
        let handle = self.get(id).await.context("session not found")?;
        self.eval_for_handle(handle, req).await
    }

    pub async fn eval_action(
        &self,
        id: &str,
        req: WebSessionRunRequest,
    ) -> Result<WebSessionRunResponse, WebSessionActionError> {
        let handle = self.get(id).await.ok_or(WebSessionActionError::NotFound)?;
        self.eval_for_handle(handle, req)
            .await
            .map_err(|_| WebSessionActionError::Internal)
    }

    async fn eval_for_handle(
        &self,
        handle: Arc<WebSessionHandle>,
        req: WebSessionRunRequest,
    ) -> Result<WebSessionRunResponse> {
        let _guard = handle.run_lock.lock().await;
        handle.touch().await;

        let payload = build_run_payload(&handle, req).await?;
        let port = handle.worker_port().await;
        let url = format!("http://127.0.0.1:{port}/eval");

        let resp = self
            .client
            .post(&url)
            .header(
                WEB_SESSION_WORKER_AUTH_HEADER,
                handle.worker_auth_secret().to_string(),
            )
            .json(&payload)
            .send()
            .await
            .context("sending eval request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Ok(WebSessionRunResponse {
                ok: false,
                result: None,
                error: Some(format!("worker error: {body}")),
            });
        }

        let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(WebSessionRunResponse {
            ok: value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
            result: value.get("result").cloned(),
            error: value
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
    }

    pub async fn close(&self, id: &str) -> Result<()> {
        let handle = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(id)
        };
        let handle = handle.context("session not found")?;
        handle.close().await?;
        Ok(())
    }

    pub async fn close_action(&self, id: &str) -> Result<(), WebSessionActionError> {
        let handle = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(id)
        }
        .ok_or(WebSessionActionError::NotFound)?;
        handle
            .close()
            .await
            .map_err(|_| WebSessionActionError::Internal)
    }

    pub async fn close_for_task(
        &self,
        session_ids: &HashSet<String>,
        worktree_ids: &HashSet<String>,
    ) -> Result<usize> {
        let handles = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .map(|(id, handle)| (id.clone(), handle.clone()))
                .collect::<Vec<_>>()
        };
        let mut to_close = Vec::new();
        for (id, handle) in handles {
            let info = &handle.info;
            let matches_session = info
                .session_id
                .as_ref()
                .map(|sid| session_ids.contains(sid))
                .unwrap_or(false);
            let matches_worktree = info
                .worktree_id
                .as_ref()
                .map(|wid| worktree_ids.contains(wid))
                .unwrap_or(false);
            if matches_session || matches_worktree {
                to_close.push(id);
            }
        }

        let mut closed = 0;
        for id in to_close {
            self.close(&id).await?;
            closed += 1;
        }
        Ok(closed)
    }

    pub async fn bump_viewers(&self, id: &str, delta: i32) -> Result<u32> {
        let handle = self.get(id).await.context("session not found")?;
        handle.touch().await;
        let mut runtime = handle.runtime.lock().await;
        let next = (runtime.viewers as i32 + delta).max(0) as u32;
        runtime.viewers = next;
        runtime.updated_at = Utc::now();
        Ok(next)
    }

    pub async fn start_reaper(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(REAPER_INTERVAL_SECS));
            loop {
                interval.tick().await;
                if let Err(err) = self.reap_idle(Duration::from_secs(DEFAULT_IDLE_SECS)).await {
                    tracing::warn!("web session reap failed: {err:#}");
                }
            }
        });
    }

    async fn reap_idle(&self, idle_for: Duration) -> Result<()> {
        let mut to_close = Vec::new();
        let handles = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .map(|(id, handle)| (id.clone(), handle.clone()))
                .collect::<Vec<_>>()
        };
        for (id, handle) in handles {
            let snapshot = handle.snapshot().await;
            if snapshot.status != WebSessionStatus::Running {
                continue;
            }
            let idle = Utc::now() - snapshot.last_activity;
            if idle.to_std().unwrap_or_default() > idle_for && snapshot.viewers == 0 {
                to_close.push(id);
            }
        }

        for id in to_close {
            let _ = self.close(&id).await;
        }
        Ok(())
    }

    async fn next_display(&self) -> Result<String> {
        let mut guard = self.next_display.lock().await;
        for _ in 0..1000 {
            let candidate = *guard;
            *guard += 1;
            let lock_path = format!("/tmp/.X{candidate}-lock");
            if !Path::new(&lock_path).exists() {
                return Ok(format!(":{candidate}"));
            }
        }
        anyhow::bail!("failed to allocate X display");
    }

    async fn spawn_worker(
        &self,
        handle: &Arc<WebSessionHandle>,
        req: &WebSessionCreateRequest,
        port: u16,
        display: &str,
    ) -> Result<()> {
        let xvfb_path = which::which("Xvfb").context("Xvfb not found in PATH")?;
        let ffmpeg_path = which::which("ffmpeg").context("ffmpeg not found in PATH")?;
        let node_path = req.node_bin.clone();
        let worker_path = req.worker_path.clone();
        let node_modules_path = req.node_modules_path.clone();

        let mut cmd = Command::new(node_path);
        cmd.arg(worker_path);
        cmd.env("PORT", port.to_string());
        cmd.env("TARGET_URL", req.url.clone());
        cmd.env(
            "WIDTH",
            req.viewport
                .as_ref()
                .map(|v| v.width)
                .unwrap_or(DEFAULT_WIDTH)
                .to_string(),
        );
        cmd.env(
            "HEIGHT",
            req.viewport
                .as_ref()
                .map(|v| v.height)
                .unwrap_or(DEFAULT_HEIGHT)
                .to_string(),
        );
        cmd.env("FPS", req.fps.unwrap_or(DEFAULT_FPS).to_string());
        cmd.env("DISPLAY", display);
        cmd.env("NODE_PATH", node_modules_path);
        cmd.env("MAP_META_TO_CTRL", "1");
        cmd.env("FFMPEG_PATH", ffmpeg_path.to_string_lossy().to_string());
        cmd.env("XVFB_PATH", xvfb_path.to_string_lossy().to_string());
        if let Some(work_dir) = &req.work_dir {
            cmd.env("WORK_DIR", work_dir.to_string_lossy().to_string());
            cmd.current_dir(work_dir);
        }

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().context("spawning web session worker")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(format!("{}\n", handle.worker_auth_secret()).as_bytes())
                .await
                .context("writing web session worker auth secret")?;
        }
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(log_stream(stdout, "web-session"));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(log_stream(stderr, "web-session"));
        }

        let mut runtime = handle.runtime.lock().await;
        runtime.child = Some(child);
        Ok(())
    }

    async fn await_worker_ready(&self, port: u16, worker_auth_secret: &str) -> Result<()> {
        let url = format!("http://127.0.0.1:{port}/health");
        for _ in 0..40 {
            let resp = self
                .client
                .get(&url)
                .header(WEB_SESSION_WORKER_AUTH_HEADER, worker_auth_secret)
                .send()
                .await;
            if let Ok(resp) = resp {
                if resp.status().is_success() {
                    let unauthenticated = self.client.get(&url).send().await;
                    match unauthenticated {
                        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                            return Ok(());
                        }
                        Ok(resp) => {
                            anyhow::bail!(
                                "worker health endpoint must reject unauthenticated loopback access (got {})",
                                resp.status()
                            );
                        }
                        Err(err) => {
                            anyhow::bail!("worker health endpoint auth verification failed: {err}");
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        anyhow::bail!("worker did not become ready");
    }
}

impl Default for WebSessionManager {
    fn default() -> Self {
        Self::new()
    }
}
