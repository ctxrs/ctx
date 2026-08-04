use std::{collections::BTreeSet, io::Write, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::{
    AdmissionResponseBarrier, ExplicitSourceCatalogAuthority, RefreshEngine, RefreshOperation,
    RefreshScope, RefreshStatus, RefreshSubmission,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::output::compact_json;

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT: usize = 256;

#[derive(Debug)]
pub(in crate::semantic) struct WireResponse {
    value: Value,
    response_barrier: Option<AdmissionResponseBarrier>,
}

pub(in crate::semantic) fn handle_ipc_request(
    engine: &RefreshEngine,
    data_root: &Path,
    request: &Value,
) -> Result<Option<WireResponse>> {
    match request.get("op").and_then(Value::as_str) {
        Some(SOURCE_REFRESH_REQUEST_OP) => {
            let submission = refresh_submission(request)?;
            let admission = engine.submit(data_root, submission)?;
            let (status, response_barrier) = admission.into_parts();
            Ok(Some(WireResponse {
                value: render_status(&status),
                response_barrier,
            }))
        }
        Some(SOURCE_REFRESH_STATUS_OP) => {
            let request_id = request
                .get("request_id")
                .and_then(Value::as_str)
                .filter(|request_id| !request_id.is_empty())
                .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
            let value = engine
                .status(request_id)
                .as_ref()
                .map(render_status)
                .unwrap_or_else(|| unknown_refresh_request_response(request_id));
            Ok(Some(WireResponse {
                value,
                response_barrier: None,
            }))
        }
        _ => Ok(None),
    }
}

pub(in crate::semantic) fn write_response<S: Write>(
    stream: &mut S,
    engine: &RefreshEngine,
    mut response: WireResponse,
) -> Result<()> {
    let response_write = (|| -> Result<()> {
        writeln!(stream, "{}", serde_json::to_string(&response.value)?)?;
        Ok(())
    })();
    if let Some(barrier) = response.response_barrier.take() {
        barrier.release(engine);
    }
    response_write
}

#[cfg(test)]
pub(in crate::semantic) fn handle_ipc_request_for_test(
    engine: &RefreshEngine,
    data_root: &Path,
    request: &Value,
) -> Result<Option<Value>> {
    let Some(response) = handle_ipc_request(engine, data_root, request)? else {
        return Ok(None);
    };
    let WireResponse {
        value,
        response_barrier,
    } = response;
    if let Some(barrier) = response_barrier {
        barrier.release(engine);
    }
    Ok(Some(value))
}

fn refresh_submission(request: &Value) -> Result<RefreshSubmission> {
    let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
    if !matches!(mode, "background" | "wait") {
        return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
    }
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
        .and_then(str::parse)?;
    let explicit_catalog = request.get("explicit_source_catalog");
    match (operation, mode, explicit_catalog) {
        (RefreshOperation::Refresh, _, Some(_)) => {
            bail!("refresh operation cannot carry explicit source catalog authority")
        }
        (RefreshOperation::Import, "background", _) => {
            bail!("import operation requires daemon refresh mode `wait`")
        }
        (RefreshOperation::Import, _, None) => {
            bail!("import operation requires explicit source catalog authority")
        }
        _ => {}
    }
    let request_id = match request.get("request_id") {
        Some(Value::String(request_id)) if !request_id.is_empty() => {
            Uuid::parse_str(request_id)
                .context("daemon source refresh logical request ID must be a UUID")?;
            request_id.clone()
        }
        None => Uuid::now_v7().to_string(),
        Some(_) => bail!("daemon source refresh logical request ID is invalid"),
    };
    let fresh_after_admitted_snapshot = match request.get("fresh_after_admitted_snapshot") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => {
            bail!("daemon source refresh fresh-after-admitted-snapshot requirement must be boolean")
        }
    };
    if operation == RefreshOperation::Refresh
        && mode == "background"
        && fresh_after_admitted_snapshot
    {
        bail!("background source refresh cannot require a fresh admission snapshot");
    }
    let requested_catalog = explicit_catalog
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let refresh_scope = request
        .get("refresh_scope")
        .filter(|value| !value.is_null())
        .map(refresh_scope_from_json)
        .transpose()?
        .unwrap_or(RefreshScope::All);
    Ok(RefreshSubmission::new(
        request_id,
        operation,
        requested_catalog,
        refresh_scope,
        fresh_after_admitted_snapshot,
        operation == RefreshOperation::Refresh && mode == "background",
    ))
}

fn refresh_scope_from_json(value: &Value) -> Result<RefreshScope> {
    match value.get("kind").and_then(Value::as_str) {
        Some("all") => Ok(RefreshScope::All),
        Some("exact") => {
            let routes = value
                .get("routes")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("exact source refresh recovery scope has no route list"))?;
            if routes.is_empty() || routes.len() > SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT {
                bail!(
                    "exact source refresh recovery scope must contain 1..={SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT} routes"
                );
            }
            routes
                .iter()
                .map(|route| {
                    let route = route.as_str().ok_or_else(|| {
                        anyhow!("exact source refresh recovery route is not a string")
                    })?;
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
                .collect::<Result<BTreeSet<_>>>()
                .map(RefreshScope::Exact)
        }
        Some(kind) => bail!("unknown source refresh recovery scope kind `{kind}`"),
        None => bail!("source refresh recovery scope kind is missing"),
    }
}

fn render_status(status: &RefreshStatus) -> Value {
    status.schema_v1_fields().clone()
}

fn unknown_refresh_request_response(request_id: &str) -> Value {
    compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": request_id,
        "request_state": "request_unknown",
        "error_code": "source_refresh_request_unknown",
        "reason": "request_not_retained_after_restart",
        "retryable": true,
        "error": "source refresh request is not retained by this daemon process",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_request_requires_a_typed_operation() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine();
        let missing = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({"op": SOURCE_REFRESH_REQUEST_OP, "mode": "wait"}),
        )
        .unwrap_err();
        assert!(format!("{missing:#}").contains("request operation is missing"));

        let invalid = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "strict_import",
            }),
        )
        .unwrap_err();
        assert!(format!("{invalid:#}").contains("invalid source refresh operation"));
        assert!(!engine.has_pending_request());
    }

    #[test]
    fn job_records_source_refresh_only_search_autostart_provenance() {
        let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(crate::config::CONFIG_FILE),
            "[daemon]\nmode = \"source-refresh-only\"\n",
        )
        .unwrap();
        crate::semantic::paths_status::write_daemon_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "running",
                "start_mode": "auto",
                "trigger_command": "search",
            }),
        )
        .unwrap();
        let engine = super::super::refresh_engine();

        let response = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "refresh",
            }),
        )
        .unwrap()
        .expect("source refresh response");
        let job = crate::semantic::paths_status::read_daemon_job_status(
            &crate::semantic::paths_status::daemon_source_backed_refresh_job_path(temp.path()),
        )
        .expect("persisted source refresh job");

        assert_eq!(response.value["daemon_mode"], "source-refresh-only");
        assert_eq!(response.value["trigger"], "search");
        assert_eq!(response.value["trigger_provenance"], "autostart");
        assert_eq!(job["daemon_mode"], "source-refresh-only");
        assert_eq!(job["trigger"], "search");
        assert_eq!(job["trigger_provenance"], "autostart");
    }
}
