use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use super::snapshot::capture_guard_snapshot;
use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;
use ctx_core::models::SessionEventType;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};

pub(super) async fn handle_provider_guard_event(
    host: &Arc<ProviderLifecycleBackgroundHost>,
    event: &ctx_provider_runtime::provider_guard::ProviderGuardEvent,
) {
    log_guard_event(host, event).await;
    if event.stage == "max" || event.stage == "kill" {
        capture_guard_snapshot(host, event).await;
    }
    notify_sessions(host, event).await;
}

async fn log_guard_event(
    host: &ProviderLifecycleBackgroundHost,
    event: &ctx_provider_runtime::provider_guard::ProviderGuardEvent,
) {
    let mem_mb = bytes_to_mb(event.sample.memory_bytes);
    tracing::warn!(
        provider_id = %event.sample.label,
        pid = event.sample.pid,
        event = event.stage,
        memory_mb = mem_mb,
        limit_high_mb = event.limits.memory_high_mb,
        limit_max_mb = event.limits.memory_max_mb,
        "provider guard triggered"
    );

    let mut labels = HashMap::new();
    labels.insert("provider_id".to_string(), event.sample.label.clone());
    labels.insert("event".to_string(), event.stage.to_string());
    host.perf_telemetry()
        .record_metric(
            PerfMetric {
                name: "ctx.provider.guard.events".to_string(),
                kind: PerfMetricKind::Counter,
                unit: "count".to_string(),
                value: 1.0,
                labels,
            },
            None,
            None,
            None,
        )
        .await;
}

async fn notify_sessions(
    host: &Arc<ProviderLifecycleBackgroundHost>,
    event: &ctx_provider_runtime::provider_guard::ProviderGuardEvent,
) {
    let session_ids = host.sessions().list_running_sessions().await;
    for session_id in session_ids {
        let store = match host.store_for_session(session_id).await {
            Ok(store) => store,
            Err(_) => continue,
        };
        let session = store.get_session(session_id).await.ok().flatten();
        let Some(session) = session else {
            continue;
        };
        if session.provider_id != event.sample.label {
            continue;
        }
        let payload = json!({
            "provider": event.sample.label,
            "kind": event.kind,
            "stage": event.stage,
            "pid": event.sample.pid,
            "memory_mb": bytes_to_mb(event.sample.memory_bytes),
            "system_total_mb": bytes_to_mb(event.system.memory_total_bytes),
            "system_used_mb": bytes_to_mb(event.system.memory_used_bytes),
            "limit_high_mb": event.limits.memory_high_mb,
            "limit_max_mb": event.limits.memory_max_mb,
            "grace_period_ms": event.limits.grace_period.as_millis() as u64,
            "kill_at_ms": event.kill_at_ms,
            "message": match event.kind {
                "provider_guard_warning" => "Provider memory is above the guard threshold.",
                "provider_guard_kill" => "Provider process killed after exceeding memory limits.",
                _ => "Provider guard notice.",
            },
        });
        match store
            .append_session_event(session_id, None, None, SessionEventType::Notice, payload)
            .await
        {
            Ok(event) => host.publish_event(event).await,
            Err(err) => tracing::warn!(
                provider_id = %session.provider_id,
                session_id = %session_id.0,
                "provider guard failed to append session event: {err:#}"
            ),
        }
    }
}

fn bytes_to_mb(value: u64) -> u64 {
    value / (1024 * 1024)
}
