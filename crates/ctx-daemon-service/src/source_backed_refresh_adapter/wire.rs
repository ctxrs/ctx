use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_refresh::{
    AdmissionResponseBarrier, RefreshEngine, RefreshIntent, RefreshRequest, RefreshRequestTrigger,
    RefreshStatus,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::compact_json;
use crate::source_backed_refresh_coordinator::CoreRefreshEngine;

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";

#[derive(Debug)]
pub(crate) struct WireResponse {
    value: Value,
    response_barrier: Option<AdmissionResponseBarrier>,
}

pub(crate) fn handle_ipc_request(
    engine: &RefreshEngine,
    data_root: &Path,
    request: &Value,
) -> Result<Option<WireResponse>> {
    match request.get("op").and_then(Value::as_str) {
        Some(SOURCE_REFRESH_REQUEST_OP) => {
            let response = match refresh_action(request)? {
                WireRefreshAction::MaintenanceWake { request_id } => WireResponse {
                    value: render_status(&engine.maintenance_wake(data_root, request_id)?),
                    response_barrier: None,
                },
                WireRefreshAction::Submit(request) => {
                    let admission = engine.submit(data_root, request)?;
                    let (status, response_barrier) = admission.into_parts();
                    WireResponse {
                        value: render_status(&status),
                        response_barrier,
                    }
                }
            };
            Ok(Some(response))
        }
        Some(SOURCE_REFRESH_STATUS_OP) => {
            let request_id = request
                .get("request_id")
                .and_then(Value::as_str)
                .filter(|request_id| !request_id.is_empty())
                .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
            let status = engine.status(request_id);
            let value = status
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

impl WireResponse {
    pub(crate) fn into_parts(self) -> (Value, Option<AdmissionResponseBarrier>) {
        (self.value, self.response_barrier)
    }
}

pub(crate) fn finish_source_refresh_response(
    barrier: Option<AdmissionResponseBarrier>,
    engine: &CoreRefreshEngine,
    signal_scheduler: impl FnOnce(),
) {
    if let Some(barrier) = barrier {
        barrier.release(engine);
    }
    if engine.has_pending_request() {
        signal_scheduler();
    }
}

#[cfg(test)]
pub(crate) fn finish_wire_response_for_test(
    response: WireResponse,
    engine: &CoreRefreshEngine,
    signal_scheduler: impl FnOnce(),
) -> Value {
    let WireResponse {
        value,
        response_barrier,
    } = response;
    finish_source_refresh_response(response_barrier, engine, signal_scheduler);
    value
}

#[cfg(test)]
pub(crate) fn handle_ipc_request_for_test(
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

#[derive(Debug)]
enum WireRefreshAction {
    MaintenanceWake { request_id: String },
    Submit(RefreshRequest),
}

fn refresh_action(request: &Value) -> Result<WireRefreshAction> {
    let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
    if !matches!(mode, "background" | "wait") {
        return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
    }
    let intent_json = request
        .get("refresh_intent")
        .ok_or_else(|| anyhow!("daemon source refresh intent is missing"))?;
    for retired_field in [
        "operation",
        "refresh_selector",
        "explicit_source_catalog",
        "fresh_after_admitted_snapshot",
        "refresh_scope",
    ] {
        if request.get(retired_field).is_some() {
            bail!("canonical daemon source refresh request carries retired `{retired_field}`");
        }
    }
    let intent = RefreshIntent::from_json(intent_json)
        .context("parse canonical daemon source refresh intent")?;
    let trigger = request
        .get("trigger")
        .and_then(Value::as_str)
        .map(str::parse::<RefreshRequestTrigger>)
        .transpose()?
        .unwrap_or(match &intent {
            RefreshIntent::AutomaticMaintenance => RefreshRequestTrigger::Search,
            RefreshIntent::SelectedImport(_) => RefreshRequestTrigger::Import,
        });
    if !matches!(
        (&intent, trigger),
        (
            RefreshIntent::AutomaticMaintenance,
            RefreshRequestTrigger::Setup
                | RefreshRequestTrigger::Search
                | RefreshRequestTrigger::Import
        ) | (
            RefreshIntent::SelectedImport(_),
            RefreshRequestTrigger::Import
        )
    ) {
        bail!("daemon source refresh trigger does not match its intent");
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
    if mode == "background" {
        if intent != RefreshIntent::AutomaticMaintenance {
            bail!("selected import requires daemon refresh mode `wait`");
        }
        return Ok(WireRefreshAction::MaintenanceWake { request_id });
    }
    Ok(WireRefreshAction::Submit(RefreshRequest::new(
        request_id, intent, trigger,
    )))
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
        // This request's durable terminal outcome is no longer observable
        // after a daemon restart.  Re-enqueuing equivalent work would create
        // a new request, not recover this one.
        "retryable": false,
        "error": "source refresh request outcome is no longer observable after daemon restart",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_request_requires_a_canonical_intent() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::CONFIG);
        let missing = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({"op": SOURCE_REFRESH_REQUEST_OP, "mode": "wait"}),
        )
        .unwrap_err();
        assert!(format!("{missing:#}").contains("refresh intent is missing"));

        let invalid = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "refresh_intent": {"kind": "strict_import"},
            }),
        )
        .unwrap_err();
        assert!(format!("{invalid:#}").contains("refresh intent `strict_import` is malformed"));
        assert!(!engine.has_pending_request());
    }

    #[test]
    fn job_records_source_refresh_only_search_autostart_provenance() {
        let temp = tempfile::tempdir().unwrap();
        crate::paths_status::write_daemon_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "running",
                "start_mode": "auto",
                "trigger_command": "search",
            }),
        )
        .unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::SOURCE_REFRESH_CONFIG);

        let response = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "refresh_intent": {"kind": "automatic_maintenance"},
            }),
        )
        .unwrap()
        .expect("source refresh response");
        let job = crate::paths_status::read_daemon_job_status(
            &crate::paths_status::daemon_source_backed_refresh_job_path(temp.path()),
        )
        .expect("persisted source refresh job");

        assert_eq!(response.value["daemon_mode"], "source-refresh-only");
        assert_eq!(response.value["trigger"], "search");
        assert_eq!(response.value["trigger_provenance"], "autostart");
        assert_eq!(job["daemon_mode"], "source-refresh-only");
        assert_eq!(job["trigger"], "search");
        assert_eq!(job["trigger_provenance"], "autostart");
    }

    #[test]
    fn setup_request_records_typed_setup_trigger_on_engine_job() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::SOURCE_REFRESH_CONFIG);

        let response = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "trigger": "setup",
                "refresh_intent": {"kind": "automatic_maintenance"},
            }),
        )
        .unwrap()
        .expect("source refresh response");
        let job = crate::paths_status::read_daemon_job_status(
            &crate::paths_status::daemon_source_backed_refresh_job_path(temp.path()),
        )
        .expect("persisted source refresh job");

        assert_eq!(response.value["trigger"], "setup");
        assert_eq!(response.value["trigger_provenance"], "setup_command");
        assert_eq!(job["trigger"], "setup");
        assert_eq!(job["trigger_provenance"], "setup_command");
    }

    #[test]
    fn background_wire_request_decodes_as_maintenance_wake() {
        let request_id = "019fcaaa-0000-7000-8000-000000000513";
        let action = refresh_action(&json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": request_id,
            "mode": "background",
            "trigger": "search",
            "refresh_intent": {"kind": "automatic_maintenance"},
        }))
        .unwrap();

        assert!(matches!(
            action,
            WireRefreshAction::MaintenanceWake {
                request_id: decoded
            } if decoded == request_id
        ));
    }

    #[test]
    fn canonical_request_rejects_retired_physical_scope() {
        let error = refresh_action(&json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "refresh_intent": {"kind": "automatic_maintenance"},
            "refresh_scope": {
                "kind": "exact",
                "routes": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            },
        }))
        .unwrap_err();

        assert!(format!("{error:#}").contains("carries retired `refresh_scope`"));
    }
}
