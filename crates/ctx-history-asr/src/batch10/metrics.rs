//! OpenHands' persisted Metrics ledger; joins are exclusively by response_id.
use super::*;
fn tokens(n: &Value) -> Value {
    let mut u = usage(n);
    for (field, key) in [
        ("cacheRead", "cache_read_tokens"),
        ("cacheWrite", "cache_write_tokens"),
    ] {
        if let Some(v) = n.get(key).filter(|v| v.as_u64().is_some()) {
            u["input"][field] = v.clone();
        }
    }
    if let Some(v) = n.get("reasoning_tokens").filter(|v| v.as_u64().is_some()) {
        u["output"]["reasoning"] = v.clone();
    }
    u
}
pub(super) fn state(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let Some(metrics) = row["value"]
        .pointer("/stats/usage_to_metrics")
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    let mut counts = std::collections::BTreeMap::new();
    for u in metrics
        .values()
        .flat_map(|m| m["token_usages"].as_array().into_iter().flatten())
    {
        if let Some(id) = u["response_id"].as_str() {
            *counts.entry(id).or_insert(0usize) += 1;
        }
    }
    let original = w.record_base.clone();
    for (bucket, m) in metrics {
        let entries = m["token_usages"].as_array();
        for (i, u) in entries.into_iter().flatten().enumerate() {
            let response = u["response_id"].as_str().filter(|s| !s.is_empty());
            let unique = response.is_some_and(|id| counts.get(id) == Some(&1));
            // Duplicated ledger copies cannot be treated as independent model calls.
            if response.is_some() && !unique {
                continue;
            }
            let key = if unique {
                format!("metric:{bucket}:{}", response.unwrap_or_default())
            } else {
                format!("metric:{bucket}:position:{i}")
            };
            w.record_base = Some(format!("b{:x}", Sha256::digest(key.as_bytes())));
            w.position(w.pos + 1);
            let mut v = json!({"type":"inference","usage":tokens(u)});
            entity(&mut v, "inferenceId", response, w);
            if !unique {
                v["idScope"] = json!("capture");
            }
            if let Some(model) = u["model"].as_str().filter(|s| !s.is_empty()) {
                v["model"] = json!({"requested":model});
            }
            if unique {
                if let Some(id) = response {
                    if let Some(previous) = spool
                        .entity(&format!("inference:{id}"))?
                        .and_then(|v| v.as_str().map(str::to_owned))
                    {
                        v["supersedes"] = json!(previous);
                    }
                    let latency = m["response_latencies"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|l| l["response_id"] == id)
                        .collect::<Vec<_>>();
                    if latency.len() == 1 {
                        if let Some(seconds) = latency[0]["latency"].as_f64() {
                            v["latency"] = json!({"durationMs":seconds*1000.0});
                        }
                    }
                }
            }
            let metric_row = json!({"snapshot":true,"value":{"bucket":bucket,"token_usage":u,"response_latencies":m["response_latencies"].as_array().into_iter().flatten().filter(|l| l["response_id"].as_str()==response).collect::<Vec<_>>()}});
            let id = emit(spool, w, v, &metric_row)?;
            if unique {
                if let Some(response) = response {
                    spool.save(&format!("inference:{response}"), &json!(id))?;
                }
            }
        }
        w.record_base = Some(format!(
            "b{:x}",
            Sha256::digest(format!("metrics:{bucket}").as_bytes())
        ));
        w.position(w.pos + 1);
        let mut v = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":bucket});
        if let Some(u) = m.get("accumulated_token_usage").filter(|v| v.is_object()) {
            v["usage"] = tokens(u);
        }
        if let Some(c) = m.get("accumulated_cost").filter(|v| v.is_number()) {
            v["cost"] = json!({"amount":c,"currency":"USD","source":"harness_estimated"});
        }
        let metric_row = json!({"snapshot":true,"value":{"bucket":bucket,"accumulated_token_usage":m["accumulated_token_usage"],"accumulated_cost":m["accumulated_cost"]}});
        emit(spool, w, v, &metric_row)?;
    }
    w.record_base = original;
    Ok(())
}
