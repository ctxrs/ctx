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
            let admission = engine.submit(data_root, refresh_request(request)?)?;
            let (status, response_barrier) = admission.into_parts();
            let mut value = render_status(&status);
            if value.get("admission_durability").is_some() {
                // A retained replacement may survive restart, but it has not
                // confirmed durability. Preserve its ID and response barrier
                // without acknowledging successful admission to the caller.
                value["ok"] = json!(false);
                value["error_code"] = json!("source_refresh_admission_unconfirmed");
                value["retryable"] = json!(true);
                value["error"] = json!(
                    "source refresh admission durability is unconfirmed; retry the same request ID"
                );
            }
            Ok(Some(WireResponse {
                value,
                response_barrier,
            }))
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

fn refresh_request(request: &Value) -> Result<RefreshRequest> {
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
    if mode == "background" && intent != RefreshIntent::AutomaticMaintenance {
        bail!("selected import requires daemon refresh mode `wait`");
    }
    // Every IPC caller gets a durable identity. ID-less scheduler wakes use
    // the engine's internal enqueue path and do not cross this boundary.
    Ok(RefreshRequest::new(request_id, intent, trigger))
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
        // The old outcome is no longer observable. This exact typed response
        // lets a waiter readmit its original request once under the same ID.
        "retryable": false,
        "error": "source refresh request outcome is no longer observable after daemon restart",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_refresh::RefreshJournal;

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
    fn background_wire_request_decodes_as_durable_submission() {
        let request_id = "019fcaaa-0000-7000-8000-000000000513";
        let action = refresh_request(&json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": request_id,
            "mode": "background",
            "trigger": "search",
            "refresh_intent": {"kind": "automatic_maintenance"},
        }))
        .unwrap();

        assert_eq!(action.request_id(), request_id);
        assert_eq!(action.intent(), &RefreshIntent::AutomaticMaintenance);
        assert_eq!(action.trigger(), RefreshRequestTrigger::Search);
    }

    #[test]
    fn canonical_request_rejects_retired_physical_scope() {
        let error = refresh_request(&json!({
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

    fn background_request(request_id: &str) -> Value {
        json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": request_id,
            "mode": "background",
            "trigger": "search",
            "refresh_intent": {"kind": "automatic_maintenance"},
        })
    }

    #[test]
    fn background_acknowledgement_is_durable_before_response_and_duplicate_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::CONFIG);
        let id = "019fcaaa-0000-7000-8000-000000000514";
        let request = background_request(id);
        let journal = super::super::journal::DaemonRefreshJournal::default();

        let first = handle_ipc_request(&engine, temp.path(), &request)
            .unwrap()
            .unwrap();
        assert_eq!(first.value["ok"], true);
        assert_eq!(first.value["request_id"], id);
        assert_eq!(first.value["request_state"], "admission_pending");
        let durable = journal.load(temp.path()).unwrap().unwrap();
        assert_eq!(durable["request_id"], id);
        assert!(durable.get("admission_durability").is_none());
        assert!(!engine.prepare_next_pending_admission(temp.path()).unwrap());

        let duplicate = handle_ipc_request(&engine, temp.path(), &request)
            .unwrap()
            .unwrap();
        assert_eq!(duplicate.value, first.value);
        assert_eq!(journal.load(temp.path()).unwrap(), Some(durable.clone()));
        first.response_barrier.unwrap().release(&engine);
        assert!(!engine.prepare_next_pending_admission(temp.path()).unwrap());
        duplicate.response_barrier.unwrap().release(&engine);
        drop(engine);

        let restarted = super::super::refresh_engine(&crate::test_support::CONFIG);
        assert!(restarted
            .recover_interrupted_publication(temp.path())
            .unwrap());
        let response = handle_ipc_request(&restarted, temp.path(), &request)
            .unwrap()
            .unwrap();
        assert_eq!(response.value["ok"], true);
        assert_eq!(response.value["request_id"], id);
        assert_eq!(
            response.value["requested_at_ms"],
            durable["requested_at_ms"]
        );
        assert_eq!(response.value["coalesced_requests"], 0);
        response.response_barrier.unwrap().release(&restarted);
    }

    struct AdmissionFaultJournal(std::sync::atomic::AtomicU8);

    impl ctx_history_refresh::RefreshJournal for AdmissionFaultJournal {
        fn load(&self, root: &Path) -> Result<Option<Value>> {
            super::super::journal::DaemonRefreshJournal::default().load(root)
        }

        fn store(&self, root: &Path, value: &Value) -> Result<()> {
            super::super::journal::DaemonRefreshJournal::default().store(root, value)
        }

        fn store_before_ack(
            &self,
            root: &Path,
            value: &Value,
        ) -> ctx_history_refresh::DurableAdmissionPersistence {
            use ctx_history_refresh::DurableAdmissionPersistence as Persistence;
            let fault = self.0.swap(0, std::sync::atomic::Ordering::SeqCst);
            if fault == 1 {
                return Persistence::Failed(anyhow!("injected pre-replacement failure"));
            }
            let result = super::super::journal::DaemonRefreshJournal::default()
                .store_before_ack(root, value);
            if fault == 2 && matches!(result, Persistence::Confirmed) {
                return Persistence::Retained(anyhow!("injected durability uncertainty"));
            }
            result
        }
    }

    #[test]
    fn background_failed_or_indeterminate_persistence_is_not_successfully_acknowledged() {
        use std::sync::{atomic::AtomicU8, Arc};
        for fault in [1, 2] {
            let temp = tempfile::tempdir().unwrap();
            let engine = RefreshEngine::new(
                Arc::new(AdmissionFaultJournal(AtomicU8::new(fault))),
                Arc::new(super::super::runtime::DaemonRefreshRuntime::new(
                    &crate::test_support::CONFIG,
                )),
            );
            let id = "019fcaaa-0000-7000-8000-000000000515";
            let request = background_request(id);
            let first = handle_ipc_request(&engine, temp.path(), &request);
            if fault == 1 {
                assert!(first.is_err());
                assert!(engine.status(id).is_none());
            } else {
                let first = first.unwrap().unwrap();
                assert_eq!(first.value["ok"], false);
                assert_eq!(first.value["retryable"], true);
                assert_eq!(first.value["request_id"], id);
                assert_eq!(
                    first.value["error_code"],
                    "source_refresh_admission_unconfirmed"
                );
                assert!(engine.status(id).is_some());
                first.response_barrier.unwrap().release(&engine);
            }
            let replay = handle_ipc_request(&engine, temp.path(), &request)
                .unwrap()
                .unwrap();
            assert_eq!(replay.value["ok"], true);
            assert_eq!(replay.value["request_id"], id);
            assert!(replay.value.get("admission_durability").is_none());
            replay.response_barrier.unwrap().release(&engine);
        }
    }
}
