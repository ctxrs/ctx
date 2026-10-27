//! Optional xAI response boundaries carry stronger evidence than ACP v1 chunks.
use super::*;

pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let u = update(v);
    let started = u["sessionUpdate"] == "response_started";
    let active = spool
        .entity("grok:response")?
        .filter(|v| v["id"].is_string());
    let mut future_id = None;
    if started && !u["message_id"].is_string() {
        let mut pos = w.pos;
        while let Some(next) = spool.next(pos)? {
            pos = next;
            let row = spool.get(pos)?;
            if jsonl::session_id(P, &row) != jsonl::session_id(P, v) {
                break;
            }
            let next = update(&row);
            if next["sessionUpdate"] == "response_completed" {
                future_id = next["message_id"].as_str().map(str::to_owned);
                break;
            }
            if matches!(
                next["sessionUpdate"].as_str(),
                Some("response_started" | "turn_completed" | "user_message_chunk")
            ) {
                break;
            }
        }
    }
    let native_id = u["message_id"].as_str().is_some() || future_id.is_some();
    let id = u["message_id"]
        .as_str()
        .map(str::to_owned)
        .or(future_id)
        .or_else(|| {
            (!started)
                .then(|| {
                    active
                        .as_ref()
                        .and_then(|v| v["id"].as_str())
                        .map(str::to_owned)
                })
                .flatten()
        })
        .unwrap_or_else(|| {
            format!(
                "response:{}",
                jsonl::event_id(v)
                    .map(str::to_owned)
                    .unwrap_or_else(|| w.pos.to_string())
            )
        });
    let key = format!("grok:inference:{id}");
    let mut r = spool
        .entity(&key)?
        .unwrap_or_else(|| json!({"type":"inference","inferenceId":id,"purpose":"turn"}));
    r.as_object_mut().map(|o| o.remove("_record"));
    if !native_id && (started || active.is_none()) {
        r["idSource"] = json!("derived");
    }
    if u["model"].is_string() {
        r["model"] = json!({"id":u["model"]});
    }
    let usage = if started { u } else { &u["usage"] };
    if usage.is_object() {
        if r.get("usage").is_none() {
            r["usage"] = json!({});
        }
        for (dest, src) in [
            ("uncached", "input_tokens"),
            ("cacheRead", "cache_read_input_tokens"),
            ("cacheWrite", "cache_creation_input_tokens"),
        ] {
            if let Some(n) = usage[src].as_u64() {
                if r["usage"].get("input").is_none() {
                    r["usage"]["input"] = json!({});
                }
                r["usage"]["input"][dest] = json!(n);
            }
        }
        for (dest, src) in [
            ("total", "output_tokens"),
            ("reasoning", "reasoning_tokens"),
        ] {
            if let Some(n) = usage[src].as_u64() {
                if r["usage"].get("output").is_none() {
                    r["usage"]["output"] = json!({});
                }
                r["usage"]["output"][dest] = json!(n);
            }
        }
    }
    copy(&mut r, "stopReason", u, "stop_reason");
    native(&mut r, v);
    w.revise(spool, &key, r, time)?;
    spool.save(
        "grok:response",
        &if started {
            json!({"id":id})
        } else {
            Value::Null
        },
    )?;
    Ok(())
}

pub(super) fn turn(w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let u = update(v);
    let mut r = json!({"type":"state","state":"idle"});
    copy(&mut r, "turnId", u, "prompt_id");
    copy(&mut r, "stopReason", u, "stop_reason");
    native(&mut r, v);
    w.emit(r, time)?;
    if u["usage"].is_object() {
        let mut r = json!({"type":"usage_summary","scope":if u["prompt_id"].is_string(){"turn"}else{"session"},"cumulative":false,"includesChildren":null});
        copy(&mut r, "turnId", u, "prompt_id");
        r["usage"] = prompt_usage(&u["usage"]);
        copy(&mut r, "callCount", &u["usage"], "model_calls");
        if let Some(cost) = cost(&u["usage"], false) {
            r["cost"] = cost;
        }
        if let Some(models) = u["usage"]["modelUsage"].as_object() {
            let incomplete = u["usage"]["usageIsIncomplete"] == true;
            r["byModel"] = json!(models
                .iter()
                .map(|(name, usage)| {
                    let mut row = json!({"model":{"id":name},"usage":prompt_usage(usage)});
                    if let Some(cost) = cost(usage, incomplete) {
                        row["cost"] = cost;
                    }
                    row
                })
                .collect::<Vec<_>>());
        }
        native(&mut r, v);
        w.emit(r, time)?;
    }
    Ok(())
}
fn prompt_usage(u: &Value) -> Value {
    let mut r = json!({"input":{},"output":{}});
    for (dest, src) in [
        ("total", "input_tokens"),
        ("cacheRead", "cached_read_tokens"),
        ("cacheWrite", "cache_creation_tokens"),
    ] {
        copy(&mut r["input"], dest, u, src);
    }
    if u["input_tokens"].is_number() {
        r["input"]["includesCache"] = json!(true);
    }
    copy(&mut r["output"], "total", u, "output_tokens");
    copy(&mut r["output"], "reasoning", u, "reasoning_tokens");
    copy(&mut r, "total", u, "total_tokens");
    r
}
fn cost(u: &Value, incomplete: bool) -> Option<Value> {
    if incomplete || u["usageIsIncomplete"] == true || u["cost_is_partial"] == true {
        return None;
    }
    Some(
        json!({"amount":u["cost_usd_ticks"].as_f64()?/10_000_000_000.0,"currency":"USD","source":"provider_reported"}),
    )
}
