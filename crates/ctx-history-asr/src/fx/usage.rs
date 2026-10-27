use serde_json::{json, Value};

pub(super) fn usage(n: &Value, input: &str, output: &str) -> Value {
    let mut result = json!({});
    if let Some(v) = n[input].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
        result["input"] = json!({"total":v,"includesCache":null});
    }
    if let Some(v) = n[output].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
        result["output"] = json!({"total":v});
    }
    result
}
pub(super) fn summary(n: &Value) -> Value {
    let mut r = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":"billing","usage":usage(n,"input_tokens","output_tokens")});
    for (src, dst) in [
        ("cache_read_tokens", "cacheRead"),
        ("cache_write_tokens", "cacheWrite"),
    ] {
        if let Some(v) = n[src].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
            r["usage"]["input"][dst] = json!(v);
        }
    }
    if let Some(cost) = n["total_cost"]
        .as_f64()
        .filter(|_| !crate::capture::unsafe_numbers(&n["total_cost"]))
    {
        r["cost"] = json!({"amount":cost,"currency":"USD"});
    }
    if let Some(models) = n["models"].as_array() {
        r["byModel"] = json!(models
            .iter()
            .filter_map(|m| {
                let model = m["model"].as_str()?;
                let mut item = summary(m);
                for field in ["type", "scope", "cumulative", "includesChildren", "counter"] {
                    item.as_object_mut().unwrap().remove(field);
                }
                item["model"] = json!({"id":model});
                Some(item)
            })
            .collect::<Vec<_>>());
    }
    r
}
