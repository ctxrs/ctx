use std::collections::HashMap;

use serde_json::{json, Value};

use crate::daemon::scheduler::host::TurnEventLoopHost;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};

use super::super::helpers::read_codex_context_window_metrics;
use super::failure::TurnFailurePayload;
use super::TurnEventLoop;

pub(super) async fn record_first_provider_event_metric(
    ctx: &TurnEventLoop,
    host: &TurnEventLoopHost,
) {
    let first_ms = ctx.run_started_at.elapsed().as_millis() as u64;
    let mut first_labels = HashMap::new();
    first_labels.insert("provider_id".to_string(), ctx.provider_id.clone());
    first_labels.insert("model_id".to_string(), ctx.model_id.clone());
    first_labels.insert(
        "execution_environment".to_string(),
        ctx.execution_environment_label.clone(),
    );
    first_labels.insert(
        "session_root_kind".to_string(),
        ctx.session_root_kind.clone(),
    );
    first_labels.insert("event".to_string(), "first_event".to_string());
    let first_metric = PerfMetric {
        name: "provider.first_event_ms".to_string(),
        kind: PerfMetricKind::Histogram,
        unit: "ms".to_string(),
        value: first_ms as f64,
        labels: first_labels,
    };
    host.record_perf_metric(first_metric, ctx.perf_run_id.clone())
        .await;
}

pub(super) async fn claim_init_provider_session_ref(
    ctx: &mut TurnEventLoop,
    host: &TurnEventLoopHost,
    payload: &mut Value,
) -> Option<TurnFailurePayload> {
    if payload.get("crp_session_id").is_some() {
        host.emit_compat_payload_reject_counter("scheduler.init_event", "crp_session_id", None)
            .await;
    }

    let provider_session_id = payload
        .get("provider_session_id")
        .and_then(Value::as_str)
        .map(str::to_string)?;

    match ctx
        .store
        .claim_session_provider_session_ref(
            ctx.session_id,
            provider_session_id.clone(),
            "scheduler.init_event",
        )
        .await
    {
        Ok(()) => {
            ctx.provider_session_ref = Some(provider_session_id);
            None
        }
        Err(err) => Some(TurnFailurePayload {
            error_message: err.to_string(),
            details: Some(json!({
                    "provider_session_id": provider_session_id,
                    "provider_id": ctx.provider_id.clone(),
            })),
            kind: Some(json!("provider_session_ref_claim_failed")),
        }),
    }
}

pub(super) fn enrich_done_payload(ctx: &TurnEventLoop, payload: &mut Value) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };

    if obj.get("context_window").is_none() {
        let metrics = if ctx.provider_id == "codex" {
            ctx.codex_home
                .as_deref()
                .and_then(|home| {
                    ctx.provider_session_ref.as_deref().and_then(|session_ref| {
                        read_codex_context_window_metrics(home, session_ref)
                    })
                })
                .or_else(|| ctx.context_window_metrics.clone())
        } else {
            ctx.context_window_metrics.clone()
        };
        if let Some(metrics) = metrics {
            obj.entry("context_window").or_insert(metrics);
        }
    }

    obj.entry("status").or_insert(json!("completed"));
}
