use std::collections::HashMap;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct InterruptTelemetryContext {
    interrupt_id: String,
    requested_at: Instant,
    requested_at_utc: DateTime<Utc>,
}

impl InterruptTelemetryContext {
    pub fn new(interrupt_id: String) -> Self {
        Self {
            interrupt_id,
            requested_at: Instant::now(),
            requested_at_utc: Utc::now(),
        }
    }

    pub fn interrupt_id(&self) -> &str {
        &self.interrupt_id
    }

    pub fn requested_at_unix_ms(&self) -> i64 {
        self.requested_at_utc.timestamp_millis()
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.requested_at.elapsed().as_millis() as u64
    }
}

pub fn latency_bucket(duration_ms: u64) -> &'static str {
    match duration_ms {
        0..=249 => "lt_250ms",
        250..=999 => "250ms_to_1s",
        1000..=2999 => "1s_to_3s",
        _ => "ge_3s",
    }
}

pub fn metric_labels(
    provider_id: &str,
    model_id: &str,
    execution_environment: &str,
    session_root_kind: &str,
    event: &str,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert("provider_id".to_string(), provider_id.to_string());
    labels.insert("model_id".to_string(), model_id.to_string());
    labels.insert(
        "execution_environment".to_string(),
        execution_environment.to_string(),
    );
    labels.insert(
        "session_root_kind".to_string(),
        session_root_kind.to_string(),
    );
    labels.insert("event".to_string(), event.to_string());
    labels
}

pub fn payload_fields(ctx: &InterruptTelemetryContext) -> Value {
    json!({
        "interrupt_id": ctx.interrupt_id,
        "requested_at_ms": ctx.requested_at_unix_ms(),
    })
}

#[cfg(test)]
mod tests {
    use super::latency_bucket;

    #[test]
    fn interrupt_latency_buckets_are_stable() {
        assert_eq!(latency_bucket(0), "lt_250ms");
        assert_eq!(latency_bucket(249), "lt_250ms");
        assert_eq!(latency_bucket(250), "250ms_to_1s");
        assert_eq!(latency_bucket(999), "250ms_to_1s");
        assert_eq!(latency_bucket(1000), "1s_to_3s");
        assert_eq!(latency_bucket(2999), "1s_to_3s");
        assert_eq!(latency_bucket(3000), "ge_3s");
    }
}
