use std::collections::HashMap;

use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use ctx_resource_utilization::ResourceProcesses;

pub(super) fn export_provider_metrics(
    perf: &PerfTelemetry,
    processes: &ResourceProcesses,
    provider_sessions: &HashMap<String, u64>,
) {
    let mut provider_agg: HashMap<String, ProviderAggregate> = HashMap::new();
    for proc in processes.providers.iter() {
        let label = proc.label.clone();
        let entry = provider_agg.entry(label).or_default();
        entry.cpu_pct += proc.cpu_pct as f64;
        entry.mem_bytes += proc.memory_bytes;
        entry.process_count += 1;
    }

    for (provider_id, agg) in provider_agg.iter() {
        let mut labels = HashMap::new();
        labels.insert("provider_id".to_string(), provider_id.clone());
        perf.export_remote_metric(PerfMetric {
            name: "ctx.provider.cpu_pct".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "percent".to_string(),
            value: agg.cpu_pct,
            labels: labels.clone(),
        });
        perf.export_remote_metric(PerfMetric {
            name: "ctx.provider.mem_bytes".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "bytes".to_string(),
            value: agg.mem_bytes as f64,
            labels: labels.clone(),
        });
        perf.export_remote_metric(PerfMetric {
            name: "ctx.provider.process_count".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "count".to_string(),
            value: agg.process_count as f64,
            labels: labels.clone(),
        });
    }

    for (provider_id, count) in provider_sessions.iter() {
        let mut labels = HashMap::new();
        labels.insert("provider_id".to_string(), provider_id.clone());
        perf.export_remote_metric(PerfMetric {
            name: "ctx.provider.session_count".to_string(),
            kind: PerfMetricKind::Gauge,
            unit: "count".to_string(),
            value: *count as f64,
            labels,
        });
    }
}

#[derive(Default)]
struct ProviderAggregate {
    cpu_pct: f64,
    mem_bytes: u64,
    process_count: u64,
}
