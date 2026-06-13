use std::collections::HashMap;

use ctx_avf_linux_runtime::SubstrateLifecycleRecord;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use ctx_resource_utilization::{ResourceProcesses, SystemSnapshot};

mod providers;
mod substrate;

pub(super) fn export_remote_metrics(
    perf: &PerfTelemetry,
    system: &SystemSnapshot,
    processes: &ResourceProcesses,
    provider_sessions: &HashMap<String, u64>,
    shared_substrate_lifecycle: Option<&SubstrateLifecycleRecord>,
) {
    let labels = HashMap::new();
    perf.export_remote_metric(PerfMetric {
        name: "ctx.system.cpu_pct".to_string(),
        kind: PerfMetricKind::Gauge,
        unit: "percent".to_string(),
        value: system.cpu_pct as f64,
        labels: labels.clone(),
    });
    perf.export_remote_metric(PerfMetric {
        name: "ctx.system.mem_used_bytes".to_string(),
        kind: PerfMetricKind::Gauge,
        unit: "bytes".to_string(),
        value: system.memory_used_bytes as f64,
        labels: labels.clone(),
    });
    perf.export_remote_metric(PerfMetric {
        name: "ctx.system.swap_used_bytes".to_string(),
        kind: PerfMetricKind::Gauge,
        unit: "bytes".to_string(),
        value: system.swap_used_bytes as f64,
        labels: labels.clone(),
    });

    if let Some(daemon) = processes.daemon.as_ref() {
        perf.export_remote_metric(PerfMetric {
            name: "ctx.daemon.cpu_pct".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "percent".to_string(),
            value: daemon.cpu_pct as f64,
            labels: labels.clone(),
        });
        perf.export_remote_metric(PerfMetric {
            name: "ctx.daemon.mem_bytes".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "bytes".to_string(),
            value: daemon.memory_bytes as f64,
            labels: labels.clone(),
        });
    }

    providers::export_provider_metrics(perf, processes, provider_sessions);
    substrate::export_substrate_lifecycle_metrics(perf, shared_substrate_lifecycle);
}
