//! Message usage reports and the ledger have no native join. Prefer the ledger when present.
use super::*;
pub(super) fn message(spool: &Spool, row: &Value, metadata: &Value) -> Result<Option<Value>> {
    if spool.entity("goose:has-ledger")?.is_some() || !metadata["usage"].is_object() {
        return Ok(None);
    }
    let native = &metadata["usage"];
    let mut out = json!({"type":"inference","inferenceId":format!("message-usage:{}",row["_key"].as_str().unwrap_or_default()),"idSource":"derived","usage":usage(native,"inputTokens","outputTokens","totalTokens")});
    if native["isCompaction"] == true {
        out["purpose"] = json!("compaction");
    }
    for (source, target) in [
        ("cacheReadTokens", "cacheRead"),
        ("cacheWriteTokens", "cacheWrite"),
    ] {
        if let Some(n) = native[source]
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
        {
            if !out["usage"]["input"].is_object() {
                out["usage"]["input"] = json!({});
            }
            out["usage"]["input"][target] = json!(n);
        }
    }
    let inference = &metadata["inference"];
    for (source, target) in [
        ("provider", "provider"),
        ("requestedModel", "requested"),
        ("resolvedModel", "id"),
    ] {
        if let Some(value) = inference[source].as_str() {
            if !out["model"].is_object() {
                out["model"] = json!({});
            }
            out["model"][target] = json!(value);
        }
    }
    for (source, target) in [
        ("elapsedMs", "durationMs"),
        ("timeToFirstTokenMs", "ttftMs"),
    ] {
        if let Some(value) = native[source]
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
        {
            if !out["latency"].is_object() {
                out["latency"] = json!({});
            }
            out["latency"][target] = json!(value);
        }
    }
    if native["cost"].as_f64().is_some_and(|v| v >= 0.0) && safe(&native["cost"]) {
        out["cost"] = json!({"amount":native["cost"],"currency":"USD"});
        match native["costSource"].as_str() {
            Some("provider_reported") => out["cost"]["source"] = json!("provider_reported"),
            Some("estimated") => out["cost"]["source"] = json!("harness_estimated"),
            _ => {}
        }
    }
    part_meta(spool, &mut out, P, metadata)?;
    Ok(Some(out))
}
