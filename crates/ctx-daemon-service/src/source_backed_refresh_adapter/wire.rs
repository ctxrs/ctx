use std::{collections::BTreeSet, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::{
    AdmissionResponseBarrier, ExplicitSourceCatalogAuthority, RefreshEngine, RefreshIntent,
    RefreshOperation, RefreshRequest, RefreshRequestTrigger, RefreshSelection, RefreshStatus,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::compact_json;
use crate::source_backed_refresh_coordinator::CoreRefreshEngine;

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT: usize = 256;

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum WireRefreshSelector {
    AllAutomatic,
    AutomaticProvider(CaptureProvider),
    ExplicitCatalog,
}

impl WireRefreshSelector {
    fn from_json(value: &Value) -> Result<Self> {
        let fields = value
            .as_object()
            .ok_or_else(|| anyhow!("source refresh selector is not an object"))?;
        match fields.get("kind").and_then(Value::as_str) {
            Some("all_automatic") if fields.len() == 1 => Ok(Self::AllAutomatic),
            Some("automatic_provider") if fields.len() == 2 => {
                let provider = fields
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("automatic provider selector has no provider"))?
                    .parse()
                    .context("parse automatic provider selector")?;
                if provider == CaptureProvider::Unknown {
                    bail!("automatic provider selector has an unknown provider");
                }
                Ok(Self::AutomaticProvider(provider))
            }
            Some("explicit_catalog") if fields.len() == 1 => Ok(Self::ExplicitCatalog),
            Some(kind) => bail!("source refresh selector `{kind}` is malformed"),
            None => bail!("source refresh selector kind is missing"),
        }
    }

    const fn is_scoped(self) -> bool {
        !matches!(self, Self::AllAutomatic)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum WireRefreshScope {
    All,
    Exact,
}

fn refresh_action(request: &Value) -> Result<WireRefreshAction> {
    let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
    if !matches!(mode, "background" | "wait") {
        return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
    }
    if let Some(intent_json) = request.get("refresh_intent") {
        for legacy_field in [
            "operation",
            "refresh_selector",
            "explicit_source_catalog",
            "fresh_after_admitted_snapshot",
            "refresh_scope",
        ] {
            if request.get(legacy_field).is_some() {
                bail!(
                    "canonical daemon source refresh request also carries legacy `{legacy_field}`"
                );
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
        return Ok(WireRefreshAction::Submit(RefreshRequest::new(
            request_id, intent, trigger,
        )));
    }

    // Decode the pre-canonical schema for compatibility. Legacy physical
    // scope remains rejected; it is never converted into executable authority.
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
        .and_then(str::parse)?;
    let trigger = request
        .get("trigger")
        .and_then(Value::as_str)
        .map(str::parse::<RefreshRequestTrigger>)
        .transpose()?
        .unwrap_or(match operation {
            RefreshOperation::Refresh => RefreshRequestTrigger::Search,
            RefreshOperation::Import => RefreshRequestTrigger::Import,
        });
    if !matches!(
        (operation, trigger),
        (
            RefreshOperation::Refresh,
            RefreshRequestTrigger::Setup
                | RefreshRequestTrigger::Search
                | RefreshRequestTrigger::Import
        ) | (RefreshOperation::Import, RefreshRequestTrigger::Import)
    ) {
        bail!("daemon source refresh trigger does not match its operation");
    }
    let explicit_catalog = request.get("explicit_source_catalog");
    let has_typed_selector = request.get("refresh_selector").is_some();
    let selector = match request.get("refresh_selector") {
        Some(value) => {
            WireRefreshSelector::from_json(value).context("parse daemon source refresh selector")?
        }
        None => match (operation, explicit_catalog.is_some()) {
            (RefreshOperation::Refresh, false) => WireRefreshSelector::AllAutomatic,
            // Legacy untyped import requests still name one exact source.
            // Normalize that old shape to the canonical exact-source intent.
            (RefreshOperation::Import, true) => WireRefreshSelector::ExplicitCatalog,
            _ => bail!("daemon source refresh selector is missing"),
        },
    };
    if operation == RefreshOperation::Import && mode == "background" {
        bail!("import operation requires daemon refresh mode `wait`");
    }
    if operation == RefreshOperation::Refresh && selector != WireRefreshSelector::AllAutomatic {
        bail!("refresh operation requires the all-automatic source selector");
    }
    match (selector, explicit_catalog.is_some()) {
        (WireRefreshSelector::AllAutomatic, false)
        | (WireRefreshSelector::AutomaticProvider(_), false)
        | (WireRefreshSelector::ExplicitCatalog, true) => {}
        (WireRefreshSelector::ExplicitCatalog, false) => {
            bail!("explicit-catalog source refresh selector has no catalog authority")
        }
        (WireRefreshSelector::AutomaticProvider(_), true) => {
            bail!("automatic provider source refresh selector carries explicit catalog authority")
        }
        (WireRefreshSelector::AllAutomatic, true) => {
            bail!("all-automatic source refresh selector carries explicit catalog authority")
        }
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
    if has_typed_selector && selector.is_scoped() && !fresh_after_admitted_snapshot {
        bail!("scoped source refresh selector requires a fresh admission snapshot");
    }
    let requested_catalog = explicit_catalog
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let requested_refresh_scope = request
        .get("refresh_scope")
        .filter(|value| !value.is_null());
    if selector.is_scoped() && requested_refresh_scope.is_some() {
        bail!("scoped source refresh selector cannot carry a physical refresh scope");
    }
    let refresh_scope = requested_refresh_scope
        .map(refresh_scope_from_json)
        .transpose()?
        .unwrap_or(WireRefreshScope::All);
    if refresh_scope == WireRefreshScope::Exact {
        bail!("physical exact refresh scope is reserved for engine-owned recovery");
    }

    if mode == "background" {
        return Ok(WireRefreshAction::MaintenanceWake { request_id });
    }

    let intent = match selector {
        WireRefreshSelector::AllAutomatic if fresh_after_admitted_snapshot => {
            RefreshIntent::SelectedImport(RefreshSelection::All)
        }
        WireRefreshSelector::AllAutomatic => RefreshIntent::AutomaticMaintenance,
        WireRefreshSelector::AutomaticProvider(provider) => {
            RefreshIntent::SelectedImport(RefreshSelection::Provider(provider))
        }
        WireRefreshSelector::ExplicitCatalog => {
            RefreshIntent::SelectedImport(RefreshSelection::ExactSource(
                requested_catalog.expect("validated exact-source catalog authority"),
            ))
        }
    };
    Ok(WireRefreshAction::Submit(RefreshRequest::new(
        request_id, intent, trigger,
    )))
}

fn refresh_scope_from_json(value: &Value) -> Result<WireRefreshScope> {
    match value.get("kind").and_then(Value::as_str) {
        Some("all") => Ok(WireRefreshScope::All),
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
            let _routes = routes
                .iter()
                .map(|route| {
                    let route = route.as_str().ok_or_else(|| {
                        anyhow!("exact source refresh recovery route is not a string")
                    })?;
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            Ok(WireRefreshScope::Exact)
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

    fn admitted_job(request: Value) -> Value {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::SOURCE_REFRESH_CONFIG);
        handle_ipc_request(&engine, temp.path(), &request)
            .unwrap()
            .expect("source refresh response");
        crate::paths_status::read_daemon_job_status(
            &crate::paths_status::daemon_source_backed_refresh_job_path(temp.path()),
        )
        .expect("persisted source refresh job")
    }

    #[test]
    fn refresh_request_requires_a_typed_operation() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::CONFIG);
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
                "operation": "refresh",
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
                "operation": "refresh",
                "trigger": "setup",
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
    fn legacy_automatic_import_normalizes_to_selected_import_intent() {
        let temp = tempfile::tempdir().unwrap();
        let engine = super::super::refresh_engine(&crate::test_support::SOURCE_REFRESH_CONFIG);

        let response = handle_ipc_request(
            &engine,
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "refresh",
                "trigger": "import",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("automatic import refresh response");

        assert_eq!(response.value["operation"], "import");
        assert_eq!(response.value["trigger"], "import");
        assert_eq!(response.value["trigger_provenance"], "import_command");
    }

    #[test]
    fn typed_import_selectors_remain_distinct_after_wire_admission() {
        let all = admitted_job(json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "refresh",
            "trigger": "import",
            "refresh_selector": {"kind": "all_automatic"},
            "fresh_after_admitted_snapshot": true,
        }));
        let provider = admitted_job(json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "import",
            "trigger": "import",
            "refresh_selector": {
                "kind": "automatic_provider",
                "provider": "codex",
            },
            "fresh_after_admitted_snapshot": true,
        }));
        let authority = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
        let catalog = admitted_job(json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "import",
            "trigger": "import",
            "refresh_selector": {"kind": "explicit_catalog"},
            "explicit_source_catalog": authority.to_json(),
            "fresh_after_admitted_snapshot": true,
        }));

        assert_eq!(
            all["refresh_intent"],
            json!({"kind": "selected_import", "selection": {"kind": "all"}})
        );
        assert_eq!(
            provider["refresh_intent"],
            json!({
                "kind": "selected_import",
                "selection": {"kind": "provider", "provider": "codex"},
            })
        );
        assert_ne!(provider["refresh_intent"], all["refresh_intent"]);
        assert_eq!(
            catalog["refresh_intent"]["selection"]["kind"],
            "exact_source"
        );
        assert_eq!(
            catalog["refresh_intent"]["selection"]["authority"],
            authority.to_json()
        );
    }

    #[test]
    fn selector_wire_validation_fails_closed() {
        let authority = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
        let invalid = [
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {"kind": "provider"},
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {"kind": "automatic_provider"},
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {
                    "kind": "automatic_provider",
                    "provider": "unknown",
                },
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "refresh",
                "trigger": "search",
                "refresh_selector": {
                    "kind": "automatic_provider",
                    "provider": "codex",
                },
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "background",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {
                    "kind": "automatic_provider",
                    "provider": "codex",
                },
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {"kind": "all_automatic"},
                "explicit_source_catalog": authority.to_json(),
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {"kind": "explicit_catalog"},
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {
                    "kind": "automatic_provider",
                    "provider": "codex",
                },
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {"kind": "explicit_catalog"},
                "explicit_source_catalog": authority.to_json(),
                "fresh_after_admitted_snapshot": false,
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
                "refresh_selector": {
                    "kind": "automatic_provider",
                    "provider": "codex",
                },
                "refresh_scope": {"kind": "all"},
            }),
            json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "trigger": "import",
            }),
        ];

        for (index, request) in invalid.into_iter().enumerate() {
            let temp = tempfile::tempdir().unwrap();
            let engine = super::super::refresh_engine(&crate::test_support::SOURCE_REFRESH_CONFIG);
            assert!(
                handle_ipc_request(&engine, temp.path(), &request).is_err(),
                "invalid selector request {index} was accepted"
            );
            assert!(!engine.has_pending_request());
        }
    }

    #[test]
    fn omitted_selector_normalizes_legacy_request_shapes_once() {
        let legacy_all = admitted_job(json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "refresh",
            "trigger": "import",
            "fresh_after_admitted_snapshot": true,
        }));
        let authority = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
        let legacy_catalog = admitted_job(json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "import",
            "trigger": "import",
            "explicit_source_catalog": authority.to_json(),
            "fresh_after_admitted_snapshot": true,
        }));

        assert_eq!(
            legacy_all["refresh_intent"],
            json!({"kind": "selected_import", "selection": {"kind": "all"}})
        );
        assert_eq!(
            legacy_catalog["refresh_intent"]["selection"]["kind"],
            "exact_source"
        );
        assert_eq!(
            legacy_catalog["refresh_intent"]["selection"]["authority"],
            authority.to_json()
        );
    }

    #[test]
    fn wire_fields_decode_to_one_canonical_request() {
        let authority = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
        let cases = [
            (
                json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "request_id": "019fcaaa-0000-7000-8000-000000000510",
                    "mode": "wait",
                    "operation": "refresh",
                    "trigger": "import",
                    "refresh_selector": {"kind": "all_automatic"},
                    "fresh_after_admitted_snapshot": true,
                    "refresh_scope": {"kind": "all"},
                }),
                RefreshIntent::SelectedImport(RefreshSelection::All),
            ),
            (
                json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "request_id": "019fcaaa-0000-7000-8000-000000000511",
                    "mode": "wait",
                    "operation": "import",
                    "trigger": "import",
                    "refresh_selector": {
                        "kind": "automatic_provider",
                        "provider": "codex",
                    },
                    "fresh_after_admitted_snapshot": true,
                }),
                RefreshIntent::SelectedImport(RefreshSelection::Provider(CaptureProvider::Codex)),
            ),
            (
                json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "request_id": "019fcaaa-0000-7000-8000-000000000512",
                    "mode": "wait",
                    "operation": "import",
                    "trigger": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority)),
            ),
        ];

        for (wire, expected_intent) in cases {
            let WireRefreshAction::Submit(request) = refresh_action(&wire).unwrap() else {
                panic!("wait request decoded as maintenance wake");
            };
            assert_eq!(Some(request.request_id()), wire["request_id"].as_str());
            assert_eq!(request.intent(), &expected_intent);
            assert_eq!(request.trigger(), RefreshRequestTrigger::Import);
        }
    }

    #[test]
    fn background_wire_request_decodes_as_maintenance_wake() {
        let request_id = "019fcaaa-0000-7000-8000-000000000513";
        let action = refresh_action(&json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": request_id,
            "mode": "background",
            "operation": "refresh",
            "trigger": "search",
            "refresh_selector": {"kind": "all_automatic"},
            "fresh_after_admitted_snapshot": false,
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
    fn physical_exact_scope_cannot_widen_through_ipc() {
        let error = refresh_action(&json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": "wait",
            "operation": "refresh",
            "refresh_scope": {
                "kind": "exact",
                "routes": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            },
        }))
        .unwrap_err();

        assert!(format!("{error:#}").contains("reserved for engine-owned recovery"));
    }
}
