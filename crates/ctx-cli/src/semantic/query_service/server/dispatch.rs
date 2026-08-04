use std::{path::Path, time::Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::output::compact_json;
use crate::semantic::{
    daemon_wakeup::DaemonWakeup,
    health_search::{semantic_model_cache_available, semantic_worker_cache_dir},
    model_contract::semantic_model_key,
    model_runtime::SharedSemanticRuntime,
    paths_status::{daemon_source_backed_refresh_job_path, read_daemon_job_status},
    source_backed_refresh_coordinator::CoreRefreshEngine,
};

use super::super::transport::DaemonIpcService;

pub(in crate::semantic) struct DaemonQueryDispatch<'a> {
    data_root: &'a Path,
    runtime: &'a SharedSemanticRuntime,
    source_refresh: &'a CoreRefreshEngine,
    service: DaemonIpcService,
    token: &'a str,
    wakeup: Option<&'a DaemonWakeup>,
}

// The Unix and Windows transport loops own this callback ABI and pass the
// stream plus request by value. Keep that boundary stable while grouping the
// cohesive service state used by the actual dispatcher below.
#[allow(clippy::too_many_arguments)]
pub(in crate::semantic) fn handle_daemon_query_stream<S: std::io::Write>(
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
    source_refresh: &CoreRefreshEngine,
    service: DaemonIpcService,
    token: &str,
    mut stream: S,
    request: Result<String>,
    wakeup: Option<&DaemonWakeup>,
) {
    let result = request.and_then(|body| {
        handle_daemon_query_stream_inner(
            DaemonQueryDispatch {
                data_root,
                runtime,
                source_refresh,
                service,
                token,
                wakeup,
            },
            &mut stream,
            &body,
        )
    });
    if let Err(error) = result {
        let _ = writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": false,
                "error": format!("{error:#}"),
            })))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"query failed\"}".to_owned())
        );
    }
}

pub(in crate::semantic) fn handle_daemon_query_stream_inner<S: std::io::Write>(
    dispatch: DaemonQueryDispatch<'_>,
    stream: &mut S,
    body: &str,
) -> Result<()> {
    let DaemonQueryDispatch {
        data_root,
        runtime,
        source_refresh,
        service,
        token,
        wakeup,
    } = dispatch;
    let request: Value = serde_json::from_str(body).context("parse daemon query request")?;
    if request.get("token").and_then(Value::as_str) != Some(token) {
        return Err(anyhow!("daemon query authentication failed"));
    }
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    if service == DaemonIpcService::SourceRefresh {
        if let Some(response) = source_refresh.handle_listener_ipc_request(data_root, &request)? {
            let response_request_id = response
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let response_write = (|| -> Result<()> {
                writeln!(stream, "{}", serde_json::to_string(&response)?)?;
                Ok(())
            })();
            if let Some(request_id) = response_request_id.as_deref() {
                source_refresh.finish_listener_admission_response(request_id);
            }
            response_write?;
            return Ok(());
        }
        if op == "ping" {
            let published_generation = read_daemon_job_status(
                &daemon_source_backed_refresh_job_path(data_root),
            )
            .and_then(|job| {
                job.get("published_generation")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "pid": std::process::id(),
                    "published_generation": published_generation,
                })))?
            )?;
            return Ok(());
        }
        if op == "shutdown" {
            let config = crate::config::AppConfig::load(data_root)?;
            if config.daemon.enabled {
                return Err(anyhow!("daemon shutdown requires [daemon] enabled = false"));
            }
            let wakeup = wakeup.ok_or_else(|| anyhow!("daemon shutdown wakeup is unavailable"))?;
            wakeup.signal_shutdown();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "shutdown": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        if op == "lifecycle_wakeup" {
            let wakeup = wakeup.ok_or_else(|| anyhow!("daemon lifecycle wakeup is unavailable"))?;
            wakeup.signal_ipc();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "lifecycle_wakeup": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        if op == "supervisor_handoff" {
            let config = crate::config::AppConfig::load(data_root)?;
            if !config.daemon.enabled {
                return Err(anyhow!(
                    "native-supervisor handoff requires an enabled daemon"
                ));
            }
            let wakeup =
                wakeup.ok_or_else(|| anyhow!("daemon supervisor handoff wakeup is unavailable"))?;
            wakeup.signal_shutdown();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "supervisor_handoff": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        return Err(anyhow!("unknown daemon source refresh operation `{op}`"));
    }
    if op == "ping" {
        let (embedding_runtime, busy) = runtime.try_runtime_status_json()?;
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": true,
                "schema_version": 1,
                "model_key": semantic_model_key(),
                "embedding_runtime": embedding_runtime,
                "busy": busy,
            })))?
        )?;
        return Ok(());
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
    let cache_dir = semantic_worker_cache_dir(data_root);
    if !runtime.is_loaded() && !semantic_model_cache_available(&cache_dir) {
        return Err(anyhow!(
            "semantic model cache is not available to daemon query service"
        ));
    }
    runtime.ensure_loaded_from_cache(&cache_dir)?;
    let (embedding, embedding_runtime) = runtime.embed_query(&cache_dir, text.to_owned())?;
    let query_embed_ms = started.elapsed().as_millis() as u64;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "ok": true,
            "model_key": semantic_model_key(),
            "embedding_runtime": embedding_runtime.to_json(),
            "query_embed_ms": query_embed_ms,
            "embedding": embedding,
        })))?
    )?;
    Ok(())
}
