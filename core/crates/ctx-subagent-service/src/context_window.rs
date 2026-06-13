#[derive(Debug, Clone, PartialEq)]
pub struct SubagentContextWindowSummary {
    pub total: u64,
    pub used: u64,
    pub remaining: u64,
    pub utilization: f64,
}

fn parse_u64(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(num) => num.as_u64(),
        serde_json::Value::String(raw) => raw.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn parse_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(num) => num.as_f64(),
        serde_json::Value::String(raw) => raw.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub fn summarize_context_window(
    metrics: &serde_json::Value,
) -> Option<SubagentContextWindowSummary> {
    let obj = metrics.as_object()?;
    let total = obj.get("context_window_tokens").and_then(parse_u64)?;
    let mut used = obj.get("context_tokens_estimate").and_then(parse_u64);
    let mut remaining = obj.get("remaining_tokens_estimate").and_then(parse_u64);

    if used.is_none() {
        if let Some(rem) = remaining {
            used = Some(total.saturating_sub(rem));
        }
    }
    if remaining.is_none() {
        if let Some(used) = used {
            remaining = Some(total.saturating_sub(used));
        }
    }
    let used = used.unwrap_or(0);
    let remaining = remaining.unwrap_or_else(|| total.saturating_sub(used));
    let utilization = obj
        .get("remaining_fraction")
        .and_then(parse_f64)
        .map(|fraction| (1.0 - fraction).clamp(0.0, 1.0))
        .unwrap_or_else(|| {
            if total == 0 {
                0.0
            } else {
                (used as f64 / total as f64).clamp(0.0, 1.0)
            }
        });

    Some(SubagentContextWindowSummary {
        total,
        used,
        remaining,
        utilization,
    })
}

pub fn legacy_context_window_metric_key(metrics: &serde_json::Value) -> Option<&'static str> {
    let obj = metrics.as_object()?;
    if obj.contains_key("context_window") {
        return Some("context_window");
    }
    if obj.contains_key("window_tokens") {
        return Some("window_tokens");
    }
    if obj.contains_key("total_tokens") {
        return Some("total_tokens");
    }
    if obj.contains_key("used_tokens") {
        return Some("used_tokens");
    }
    if obj.contains_key("remaining_tokens") {
        return Some("remaining_tokens");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_context_window_accepts_canonical_metrics() {
        let metrics = serde_json::json!({
            "context_tokens_estimate": 40,
            "context_window_tokens": 100,
            "remaining_tokens_estimate": 60,
            "remaining_fraction": 0.6,
        });

        let summary =
            summarize_context_window(&metrics).expect("expected canonical metrics to parse");
        assert_eq!(summary.total, 100);
        assert_eq!(summary.used, 40);
        assert_eq!(summary.remaining, 60);
        assert!((summary.utilization - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn summarize_context_window_rejects_legacy_alias_metrics() {
        let legacy = serde_json::json!({
            "context_window": 100,
            "total_tokens": 40,
            "remaining_tokens": 60,
        });
        assert!(summarize_context_window(&legacy).is_none());
    }

    #[test]
    fn legacy_context_window_metric_key_detects_first_legacy_key() {
        let legacy = serde_json::json!({
            "context_window": 100,
            "remaining_tokens": 60,
        });
        assert_eq!(
            legacy_context_window_metric_key(&legacy),
            Some("context_window")
        );
    }

    #[test]
    fn legacy_context_window_metric_key_returns_none_for_canonical_shape() {
        let canonical = serde_json::json!({
            "context_tokens_estimate": 40,
            "context_window_tokens": 100,
            "remaining_tokens_estimate": 60,
        });
        assert_eq!(legacy_context_window_metric_key(&canonical), None);
    }
}
