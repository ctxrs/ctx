use std::collections::HashMap;

use ctx_avf_linux_runtime::SubstrateLifecycleRecord;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use serde::Serialize;

pub(super) fn export_substrate_lifecycle_metrics(
    perf: &PerfTelemetry,
    shared_substrate_lifecycle: Option<&SubstrateLifecycleRecord>,
) {
    if let Some(record) = shared_substrate_lifecycle {
        let mut base_labels = HashMap::new();
        if let Some(substrate) = serde_label(&record.substrate) {
            base_labels.insert("substrate_kind".to_string(), substrate);
        }

        perf.export_remote_metric(PerfMetric {
            name: "ctx.substrate.simulated".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "bool".to_string(),
            value: if record.simulated { 1.0 } else { 0.0 },
            labels: base_labels.clone(),
        });

        if let Some(startup_selection) = record.startup_selection.as_ref().and_then(serde_label) {
            let mut labels = base_labels.clone();
            labels.insert("startup_selection".to_string(), startup_selection);
            perf.export_remote_metric(PerfMetric {
                name: "ctx.substrate.startup_selection".to_string(),
                kind: PerfMetricKind::Gauge,
                unit: "state".to_string(),
                value: 1.0,
                labels,
            });
        }

        if let Some(startup_outcome) = record.startup_outcome.as_ref().and_then(serde_label) {
            let mut labels = base_labels.clone();
            labels.insert("startup_outcome".to_string(), startup_outcome);
            if let Some(startup_reason) = record.startup_reason.as_ref().and_then(serde_label) {
                labels.insert("startup_reason".to_string(), startup_reason);
            }
            perf.export_remote_metric(PerfMetric {
                name: "ctx.substrate.startup_outcome".to_string(),
                kind: PerfMetricKind::Gauge,
                unit: "state".to_string(),
                value: 1.0,
                labels,
            });
        }

        if let Some(shutdown_outcome) = record.shutdown_outcome.as_ref().and_then(serde_label) {
            let mut labels = base_labels;
            labels.insert("shutdown_outcome".to_string(), shutdown_outcome);
            if let Some(shutdown_reason) = record.shutdown_reason.as_ref().and_then(serde_label) {
                labels.insert("shutdown_reason".to_string(), shutdown_reason);
            }
            perf.export_remote_metric(PerfMetric {
                name: "ctx.substrate.shutdown_outcome".to_string(),
                kind: PerfMetricKind::Gauge,
                unit: "state".to_string(),
                value: 1.0,
                labels,
            });
        }
    }
}

fn serde_label<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()?
        .as_str()
        .map(ToString::to_string)
}
