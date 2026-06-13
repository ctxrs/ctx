use std::collections::HashMap;

use ctx_execution_runtime::{HarnessSetupPhase, RuntimeMetricsSink};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};

pub struct CtxRuntimeMetricsSink {
    inner: PerfTelemetry,
}

impl CtxRuntimeMetricsSink {
    pub fn new(inner: PerfTelemetry) -> Self {
        Self { inner }
    }

    fn record_histogram(&self, name: &'static str, value_ms: u64, labels: HashMap<String, String>) {
        let perf = self.inner.clone();
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Histogram,
            unit: "ms".to_string(),
            value: value_ms as f64,
            labels,
        };
        tokio::spawn(async move {
            perf.record_metric(metric, None, None, None).await;
        });
    }
}

fn harness_setup_phase_label(phase: HarnessSetupPhase) -> &'static str {
    match phase {
        HarnessSetupPhase::ArtifactDownload => "artifact_download",
        HarnessSetupPhase::MachineCheck => "machine_check",
        HarnessSetupPhase::MachineStartOrInit => "machine_start_or_init",
        HarnessSetupPhase::ImageCheck => "image_check",
        HarnessSetupPhase::ImageLoad => "image_load",
        HarnessSetupPhase::ContainerCheck => "container_check",
        HarnessSetupPhase::ContainerStartOrCreate => "container_start_or_create",
        HarnessSetupPhase::RuntimeNetworkSetup => "runtime_network_setup",
        HarnessSetupPhase::Ready => "ready",
    }
}

impl RuntimeMetricsSink for CtxRuntimeMetricsSink {
    fn record_phase_duration(
        &self,
        phase: HarnessSetupPhase,
        elapsed_ms: u64,
        result: &'static str,
    ) {
        let mut labels = HashMap::new();
        labels.insert(
            "phase".to_string(),
            harness_setup_phase_label(phase).to_string(),
        );
        labels.insert("result".to_string(), result.to_string());
        self.record_histogram("execution.launch.phase_duration_ms", elapsed_ms, labels);
    }

    fn record_launch_duration(&self, elapsed_ms: u64, result: &'static str) {
        let mut labels = HashMap::new();
        labels.insert("result".to_string(), result.to_string());
        self.record_histogram("execution.launch.total_duration_ms", elapsed_ms, labels);
    }
}

#[cfg(test)]
mod tests {
    use super::harness_setup_phase_label;
    use ctx_execution_runtime::HarnessSetupPhase;

    #[test]
    fn phase_labels_remain_stable_snake_case_values() {
        assert_eq!(
            harness_setup_phase_label(HarnessSetupPhase::MachineStartOrInit),
            "machine_start_or_init",
        );
        assert_eq!(
            harness_setup_phase_label(HarnessSetupPhase::ContainerStartOrCreate),
            "container_start_or_create",
        );
        assert_eq!(
            harness_setup_phase_label(HarnessSetupPhase::RuntimeNetworkSetup),
            "runtime_network_setup",
        );
    }
}
