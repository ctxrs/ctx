use super::*;

impl DaemonState {
    pub async fn emit_cache_miss(&self, cache: &str) {
        self.emit_cache_counter("daemon.cache_miss", cache, 1, None)
            .await;
    }

    pub async fn emit_cache_rehydrate(&self, cache: &str, ok: bool) {
        let result = if ok { "ok" } else { "fail" };
        self.emit_cache_counter("daemon.cache_rehydrate", cache, 1, Some(("result", result)))
            .await;
    }

    pub(super) async fn emit_cache_evicted(&self, cache: &str, value: usize) {
        if value == 0 {
            return;
        }
        self.emit_cache_counter("daemon.cache_evicted", cache, value as u64, None)
            .await;
    }

    async fn emit_cache_counter(
        &self,
        name: &str,
        cache: &str,
        value: u64,
        extra_label: Option<(&str, &str)>,
    ) {
        if value == 0 {
            return;
        }
        let mut labels = HashMap::new();
        labels.insert("cache".to_string(), cache.to_string());
        labels.insert("source".to_string(), "daemon".to_string());
        if let Some((key, val)) = extra_label {
            labels.insert(key.to_string(), val.to_string());
        }
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: value as f64,
            labels,
        };
        self.telemetry
            .perf_telemetry
            .record_metric(metric, None, None, None)
            .await;
    }

    pub async fn emit_compat_payload_reject_counter(
        &self,
        surface: &str,
        issue: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("issue".to_string(), issue.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.emit_counter_metric("compat.payload_reject_count", labels)
            .await;
    }

    pub async fn emit_product_fallback_applied_counter(
        &self,
        surface: &str,
        fallback: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("fallback".to_string(), fallback.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.emit_counter_metric("product.fallback_applied_count", labels)
            .await;
    }

    async fn emit_counter_metric(&self, name: &str, labels: HashMap<String, String>) {
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: 1.0,
            labels,
        };
        self.telemetry
            .perf_telemetry
            .record_metric(metric, None, None, None)
            .await;
    }
}
