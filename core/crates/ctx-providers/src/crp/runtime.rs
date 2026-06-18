use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::time::Duration;
use uuid::Uuid;

use crate::container_exec::{build_container_exec_command, container_exec_spec};

use super::protocol::{CrpCommand, CrpCommandEnvelope, CrpEventEnvelope};
use super::{CODEX_CRP_DUMP_CODEX_EVENTS_ENV, CODEX_CRP_DUMP_CRP_EVENTS_ENV};

#[path = "runtime/path_rewrite.rs"]
mod path_rewrite;

#[cfg(test)]
use self::path_rewrite::rewrite_container_args_for_linux;
pub(crate) use self::path_rewrite::{
    resolve_explicit_command_path, rewrite_bundled_path_for_linux,
    rewrite_container_command_for_linux,
};

const AMBIENT_PROVIDER_SESSION_ENV_DENYLIST: &[&str] = &[
    "CTX_PROVIDER_SESSION_REF",
    "CTX_PROVIDER_MODE",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDE_THREAD_ID",
    "GEMINI_SESSION_ID",
    "GEMINI_THREAD_ID",
    "ACP_SESSION_ID",
];
pub(super) const CRP_EVENT_BROADCAST_CAPACITY: usize = 16_384;
pub(super) const CRP_STDERR_BROADCAST_CAPACITY: usize = 256;

fn scrub_ambient_provider_session_env(cmd: &mut Command) {
    for key in AMBIENT_PROVIDER_SESSION_ENV_DENYLIST {
        cmd.env_remove(key);
    }
}

#[derive(Debug, Clone)]
pub(super) struct CrpAgentConfig {
    pub(super) provider_id: String,
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) spawn_cwd: Option<PathBuf>,
    pub(super) env: HashMap<String, String>,
}

pub(super) struct CrpProcess {
    agent: CrpAgentConfig,
    child: Mutex<Child>,
    pid: AtomicU32,
    write_tx: mpsc::UnboundedSender<String>,
    pub(super) events: broadcast::Sender<CrpEventEnvelope>,
    pub(super) stderr_lines: broadcast::Sender<String>,
    pub(super) shutdown: watch::Sender<Option<String>>,
}

struct CrpLogPaths {
    codex_events: PathBuf,
    crp_events: PathBuf,
    raw_stdout: PathBuf,
    stderr: PathBuf,
}

struct PreparedCrpSpawnEnv {
    env: HashMap<String, String>,
    raw_stdout_log_path: Option<PathBuf>,
    stderr_log_path: Option<PathBuf>,
}

impl CrpProcess {
    pub(super) async fn spawn(
        agent: &CrpAgentConfig,
        workdir: &PathBuf,
        env: &HashMap<String, String>,
    ) -> Result<Arc<Self>> {
        let mut merged_env = env.clone();
        for (key, value) in &agent.env {
            merged_env.insert(key.clone(), value.clone());
        }
        let prepared = prepare_crp_spawn_env(&merged_env, &agent.provider_id);
        let spawn_cwd = agent.spawn_cwd.as_ref().unwrap_or(workdir);
        let mut cmd = if let Some(spec) = container_exec_spec(&prepared.env) {
            let (container_command, container_args) =
                rewrite_container_command_for_linux(&agent.command, &agent.args, &prepared.env)?;
            build_container_exec_command(
                &spec,
                spawn_cwd,
                &prepared.env,
                &container_command,
                &container_args,
            )?
        } else {
            let mut cmd = Command::new(&agent.command);
            cmd.args(&agent.args);
            cmd.current_dir(spawn_cwd);
            cmd
        };
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        scrub_ambient_provider_session_env(&mut cmd);
        apply_outer_process_env(&mut cmd, &prepared.env);

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);

        let stdin = child.stdin.take().context("capturing CRP stdin")?;
        let stdout = child.stdout.take().context("capturing CRP stdout")?;
        let stderr = child.stderr.take().context("capturing CRP stderr")?;

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
        let _writer = tokio::spawn(async move {
            let mut stdin = tokio::io::BufWriter::new(stdin);
            while let Some(line) = write_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        let (events, _) = broadcast::channel(CRP_EVENT_BROADCAST_CAPACITY);
        let (stderr_lines, _) = broadcast::channel(CRP_STDERR_BROADCAST_CAPACITY);
        let (shutdown, _) = watch::channel::<Option<String>>(None);
        let process = Arc::new(Self {
            agent: agent.clone(),
            child: Mutex::new(child),
            pid: AtomicU32::new(pid),
            write_tx,
            events,
            stderr_lines,
            shutdown,
        });

        let stdout_process = Arc::clone(&process);
        let raw_stdout_log_path = prepared.raw_stdout_log_path;
        tokio::spawn(async move {
            stdout_pump(stdout_process, stdout, raw_stdout_log_path).await;
        });
        let stderr_process = Arc::clone(&process);
        let stderr_log_path = prepared.stderr_log_path;
        tokio::spawn(async move {
            stderr_pump(stderr_process, stderr, stderr_log_path).await;
        });

        let monitor_process = Arc::clone(&process);
        tokio::spawn(async move {
            monitor_crp_child_exit(monitor_process).await;
        });

        Ok(process)
    }

    pub(super) async fn pid(&self) -> Option<u32> {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid != 0 {
            return Some(pid);
        }
        let child = self.child.lock().await;
        child.id()
    }

    pub(super) async fn send(&self, command: CrpCommand) -> Result<()> {
        let envelope = CrpCommandEnvelope {
            v: Some(super::CRP_VERSION),
            command,
        };
        let line = serde_json::to_string(&envelope)?;
        self.write_tx
            .send(line)
            .map_err(|_| anyhow::anyhow!("crp runtime stdin closed"))?;
        Ok(())
    }

    pub(super) fn signal_shutdown(&self, reason: &str) {
        let next = reason.to_string();
        let prefer_over_stdout_close =
            next.starts_with("crp_runtime_exited:") || next.starts_with("crp_runtime_wait_failed:");

        // Avoid clobbering an existing shutdown reason (e.g. drain/restart), which is
        // user-visible via TurnInterrupted. The only exception is upgrading a generic
        // stdout-close reason to a more specific exit/wait failure.
        let _ = self.shutdown.send_if_modified(|current| {
            let should_replace = match current.as_deref() {
                None => true,
                Some("crp_runtime_stdout_closed") => prefer_over_stdout_close,
                Some(_) => false,
            };

            if should_replace {
                *current = Some(next.clone());
            }

            should_replace
        });
    }

    pub(super) async fn shutdown(&self, reason: &str) {
        self.signal_shutdown(reason);
        let mut child = self.child.lock().await;
        if let Err(err) = child.kill().await {
            tracing::debug!(
                provider_id = %self.agent.provider_id,
                "crp shutdown failed ({reason}): {err}"
            );
        }
        let _ = child.wait().await;
        self.pid.store(0, Ordering::Relaxed);
    }
}

async fn monitor_crp_child_exit(process: Arc<CrpProcess>) {
    let mut shutdown_rx = process.shutdown.subscribe();
    loop {
        if shutdown_rx.borrow().is_some() {
            return;
        }

        let status = {
            let mut child = process.child.lock().await;
            child.try_wait()
        };

        match status {
            Ok(Some(status)) => {
                process.pid.store(0, Ordering::Relaxed);
                process.signal_shutdown(&format!("crp_runtime_exited: {status}"));
                return;
            }
            Ok(None) => {}
            Err(err) => {
                process.pid.store(0, Ordering::Relaxed);
                process.signal_shutdown(&format!("crp_runtime_wait_failed: {err}"));
                return;
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {},
            changed = shutdown_rx.changed() => {
                if changed.is_err() || shutdown_rx.borrow().is_some() {
                    return;
                }
            }
        }
    }
}

fn prepare_crp_spawn_env(env: &HashMap<String, String>, provider_id: &str) -> PreparedCrpSpawnEnv {
    let mut prepared = PreparedCrpSpawnEnv {
        env: env.clone(),
        raw_stdout_log_path: None,
        stderr_log_path: None,
    };

    let Some(paths) = crp_log_paths(env, provider_id) else {
        return prepared;
    };
    let Some(parent) = paths.stderr.parent() else {
        return prepared;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return prepared;
    }

    if let Some(parent) = paths.codex_events.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let codex_dump_path = paths.codex_events.to_string_lossy().to_string();
    let crp_dump_path = paths.crp_events.to_string_lossy().to_string();
    prepared
        .env
        .entry(CODEX_CRP_DUMP_CODEX_EVENTS_ENV.to_string())
        .or_insert(codex_dump_path);
    prepared
        .env
        .entry(CODEX_CRP_DUMP_CRP_EVENTS_ENV.to_string())
        .or_insert(crp_dump_path);
    prepared.raw_stdout_log_path = Some(paths.raw_stdout);
    prepared.stderr_log_path = Some(paths.stderr);
    prepared
}

fn crp_log_paths(env: &HashMap<String, String>, provider_id: &str) -> Option<CrpLogPaths> {
    let host_data_root = crate::env::data_root_for_host(env)?;
    let child_data_root = crate::env::data_root_for_child(env)?;
    let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
    let suffix = Uuid::new_v4().simple().to_string();
    let base = format!("crp-{provider_id}-{timestamp}-{suffix}");
    let host_dir = Path::new(&host_data_root).join("logs").join("providers");
    let child_dir = Path::new(&child_data_root).join("logs").join("providers");
    Some(CrpLogPaths {
        codex_events: child_dir.join(format!("{base}.codex-events.jsonl")),
        crp_events: child_dir.join(format!("{base}.crp-events.jsonl")),
        raw_stdout: host_dir.join(format!("{base}.stdout.log")),
        stderr: host_dir.join(format!("{base}.stderr.log")),
    })
}

async fn stdout_pump(
    process: Arc<CrpProcess>,
    stdout: impl tokio::io::AsyncRead + Unpin,
    log_path: Option<PathBuf>,
) {
    // Debugging aid: when set, dump raw CRP stdout lines from the runtime to this file.
    // This lets us confirm what the runtime emitted without involving storage/UI layers.
    let dump_path = std::env::var("CTX_CRP_DUMP_EVENTS_PATH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dump_file_path = log_path
        .as_deref()
        .or_else(|| dump_path.as_deref().map(Path::new));
    let mut dump_file = dump_file_path.and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });

    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(f) = dump_file.as_mut() {
                    // Best-effort only; never fail the pump because debug dumping failed.
                    let _ = writeln!(f, "{}", redact_sensitive(trimmed));
                }
                match serde_json::from_str::<CrpEventEnvelope>(trimmed) {
                    Ok(env) => {
                        let _ = process.events.send(env);
                    }
                    Err(err) => {
                        tracing::warn!(
                            provider_id = %process.agent.provider_id,
                            error = %err,
                            "failed to parse CRP event"
                        );
                    }
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(
                    provider_id = %process.agent.provider_id,
                    error = %err,
                    "failed to read CRP stdout"
                );
                break;
            }
        }
    }

    process.signal_shutdown("crp_runtime_stdout_closed");
}

async fn stderr_pump(
    process: Arc<CrpProcess>,
    stderr: impl tokio::io::AsyncRead + Unpin,
    log_path: Option<PathBuf>,
) {
    let mut log_file = match log_path {
        Some(path) => tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .ok(),
        None => None,
    };
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(file) = log_file.as_mut() {
            let redacted = redact_sensitive(trimmed);
            if file.write_all(redacted.as_bytes()).await.is_err() {
                log_file = None;
            } else {
                let _ = file.write_all(b"\n").await;
                let _ = file.flush().await;
            }
        }
        let _ = process.stderr_lines.send(redact_sensitive(trimmed));
        tracing::debug!(
            provider_id = %process.agent.provider_id,
            "crp stderr: {}",
            trimmed
        );
    }
}

fn redact_sensitive(input: &str) -> String {
    fn redact_after_marker(mut s: String, marker: &str) -> String {
        let redacted = "[REDACTED]";
        let mut search_from = 0usize;
        while let Some(rel) = s[search_from..].find(marker) {
            let marker_start = search_from + rel;
            let start = marker_start + marker.len();
            if start >= s.len() {
                break;
            }
            if s[start..].starts_with(redacted) {
                search_from = start + redacted.len();
                continue;
            }

            let mut end = s.len();
            for (i, ch) in s[start..].char_indices() {
                if ch.is_whitespace() || ch == '"' || ch == '\'' || ch == '&' {
                    end = start + i;
                    break;
                }
            }

            s.replace_range(start..end, redacted);
            search_from = start + redacted.len();
        }
        s
    }

    let mut out = input.to_string();
    out = redact_after_marker(out, "Bearer ");
    out = redact_after_marker(out, "bearer ");
    out = redact_after_marker(out, "Authorization: Bearer ");
    out = redact_after_marker(out, "authorization: Bearer ");
    out = redact_after_marker(out, "token=");
    out = redact_after_marker(out, "TOKEN=");
    out = redact_after_marker(out, "CTX_AUTH_TOKEN=");
    out = redact_after_marker(out, "CTX_MCP_TOKEN=");
    out = redact_after_marker(out, "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN=");
    out = redact_after_marker(out, "ctxAuthToken\":\"");
    out = redact_after_marker(out, "ctx_auth_token\":\"");
    out = redact_after_marker(out, "\"CTX_MCP_TOKEN\":\"");
    out = redact_after_marker(out, "\"CTX_MCP_TOKEN\": \"");
    out = redact_after_marker(out, "\"CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN\":\"");
    out = redact_after_marker(out, "\"CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN\": \"");
    out = redact_after_marker(out, "\"ctx_mcp_token\":\"");
    out = redact_after_marker(out, "\"ctx_mcp_token\": \"");
    out
}

pub(super) fn apply_outer_process_env(cmd: &mut Command, env: &HashMap<String, String>) {
    for key in [
        "CTX_AUTH_TOKEN",
        "CTX_MCP_TOKEN",
        "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN",
        "CTX_SESSION_ID",
        "CTX_PROVIDER_SESSION_REF",
        "CTX_MCP_DISABLED",
        "CTX_MCP_COMMAND",
    ] {
        cmd.env_remove(key);
    }
    let is_container_exec = container_exec_spec(env).is_some();
    for (key, value) in env {
        if should_skip_outer_process_env_key(key, is_container_exec) {
            continue;
        }
        cmd.env(key, value);
    }
}

pub(super) fn should_skip_outer_process_env_key(key: &str, is_container_exec: bool) -> bool {
    if matches!(
        key,
        "CTX_AUTH_TOKEN" | "CTX_MCP_TOKEN" | "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN"
    ) {
        return true;
    }
    if !is_container_exec {
        return false;
    }
    matches!(key, "HOME" | "TMPDIR" | "TMP" | "TEMP") || key.starts_with("XDG_")
}

#[cfg(test)]
mod tests;
