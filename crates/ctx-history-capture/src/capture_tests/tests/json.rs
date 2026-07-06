#[allow(unused_imports)]
use super::*;

pub(crate) fn jsonl_line(value: Value) -> String {
    serde_json::to_string(&value).unwrap() + "\n"
}

impl TimingStats {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "min_ms": rounded(self.min_ms),
            "p50_ms": rounded(self.p50_ms),
            "p95_ms": rounded(self.p95_ms),
            "max_ms": rounded(self.max_ms),
        })
    }
}
