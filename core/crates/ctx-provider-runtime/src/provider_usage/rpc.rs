use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use ctx_core::provider_policy::CODEX_APP_SERVER_ARGS;
use ctx_provider_accounts as provider_accounts;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn fetch_codex_usage_rpc(
    env: &HashMap<String, String>,
) -> Result<serde_json::Value> {
    let _continuity_lock = provider_accounts::acquire_codex_runtime_continuity_lock_from_env(env)?;
    let mut child = spawn_codex_app_server(env)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("codex app-server stdout unavailable"))?;
    let mut reader = BufReader::new(stdout).lines();
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("codex app-server stdin unavailable"))?;

    let init_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "ctx",
                "title": "ctx",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    send_jsonrpc(&mut stdin, &init_request).await?;
    wait_for_response(&mut reader, 1, CODEX_RPC_TIMEOUT).await?;
    send_jsonrpc(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized"}),
    )
    .await?;

    let rate_limits_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "account/rateLimits/read"
    });
    send_jsonrpc(&mut stdin, &rate_limits_request).await?;
    let response = wait_for_response(&mut reader, 2, CODEX_RPC_TIMEOUT).await?;

    let payload = response
        .get("result")
        .and_then(|v| v.get("rateLimits"))
        .ok_or_else(|| anyhow!("codex rpc rate limits missing result"))?;
    let normalized = normalize_codex_rpc_rate_limits(payload)?;

    let _ = child.kill().await;
    Ok(normalized)
}

fn spawn_codex_app_server(env: &HashMap<String, String>) -> Result<Child> {
    let codex_bin = env
        .get("CTX_CODEX_BIN_PATH")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("CTX_CODEX_BIN_PATH must be set to an absolute codex-cli path"))?;
    if !Path::new(codex_bin).is_absolute() {
        anyhow::bail!("CTX_CODEX_BIN_PATH must be absolute, got `{codex_bin}`");
    }
    let mut cmd = Command::new(codex_bin);
    cmd.kill_on_drop(true);
    cmd.args(CODEX_APP_SERVER_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "CTX_PROVIDER_SESSION_REF",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "CLAUDE_THREAD_ID",
        "GEMINI_SESSION_ID",
        "GEMINI_THREAD_ID",
        "ACP_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.spawn()
        .with_context(|| format!("spawning codex app-server via `{codex_bin}`"))
}

async fn send_jsonrpc(
    stdin: &mut tokio::process::ChildStdin,
    value: &serde_json::Value,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

async fn wait_for_response(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    request_id: i64,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("codex rpc timeout waiting for response"));
        }
        let line = tokio::time::timeout(remaining, reader.next_line())
            .await
            .context("codex rpc read timeout")??;
        let line = line.ok_or_else(|| anyhow!("codex rpc stdout closed"))?;
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            if id == request_id {
                if value.get("error").is_some() {
                    return Err(anyhow!("codex rpc error: {value}"));
                }
                return Ok(value);
            }
        }
    }
}

fn normalize_codex_rpc_rate_limits(rate_limits: &serde_json::Value) -> Result<serde_json::Value> {
    let plan_type = rate_limits
        .get("planType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let primary = normalize_codex_rpc_window(rate_limits.get("primary"));
    let secondary = normalize_codex_rpc_window(rate_limits.get("secondary"));
    let credits = normalize_codex_rpc_credits(rate_limits.get("credits"));
    Ok(json!({
        "plan_type": plan_type,
        "rate_limit": {
            "primary_window": primary,
            "secondary_window": secondary,
        },
        "credits": credits,
    }))
}

fn normalize_codex_rpc_window(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let used_percent = value.get("usedPercent").and_then(|v| v.as_i64())?;
    let window_minutes = value.get("windowDurationMins").and_then(|v| v.as_i64());
    let resets_at = value.get("resetsAt").and_then(|v| v.as_i64());
    let limit_window_seconds = window_minutes.map(|mins| mins * 60);
    let reset_after_seconds = resets_at.map(|ts| (ts - Utc::now().timestamp()).max(0));
    Some(json!({
        "used_percent": used_percent,
        "limit_window_seconds": limit_window_seconds,
        "reset_after_seconds": reset_after_seconds,
        "reset_at": resets_at,
    }))
}

fn normalize_codex_rpc_credits(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    Some(json!({
        "used_usd": value.get("usedUsd").and_then(|v| v.as_f64()),
        "remaining_usd": value.get("remainingUsd").and_then(|v| v.as_f64()),
        "total_usd": value.get("totalUsd").and_then(|v| v.as_f64()),
    }))
}
