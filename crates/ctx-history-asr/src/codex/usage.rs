use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
fn tokens(p: &Value) -> Value {
    let mut u = json!({});
    let mut i = json!({});
    let mut o = json!({});
    for (dest, key) in [
        ("total", "input_tokens"),
        ("cacheRead", "cached_input_tokens"),
        ("cacheWrite", "cache_write_input_tokens"),
    ] {
        copy(&mut i, dest, p, key);
    }
    if i.get("total").is_some() {
        i["includesCache"] = json!(true);
    }
    for (dest, key) in [
        ("total", "output_tokens"),
        ("reasoning", "reasoning_output_tokens"),
    ] {
        copy(&mut o, dest, p, key);
    }
    if i.as_object().is_some_and(|x| !x.is_empty()) {
        u["input"] = i;
    }
    if o.as_object().is_some_and(|x| !x.is_empty()) {
        u["output"] = o;
    }
    copy(&mut u, "total", p, "total_tokens");
    u
}
pub(super) fn inference(spool: &Spool, writer: &mut Writer, p: &Value, v: &Value) -> Result<()> {
    let Some(id) = p["response_id"].as_str() else {
        return writer.vendor(v, "token_usage_record");
    };
    if spool.entity(&format!("inference:{id}"))?.is_none() {
        let mut r = json!({"type":"inference","inferenceId":id});
        if p["usage"].is_object() {
            r["usage"] = tokens(&p["usage"]);
        }
        let turn_model = p["turn_id"]
            .as_str()
            .map(|t| spool.entity(&format!("model:{t}")))
            .transpose()?
            .flatten();
        let model = if p["turn_id"].is_string() {
            turn_model.as_ref().and_then(Value::as_str)
        } else {
            writer.model.as_deref()
        };
        if let Some(model) = model {
            r["model"] = json!({"requested":model,"requestedSource":"turn_config"});
        }
        writer.emit(r, v)?;
        spool.save(&format!("inference:{id}"), &json!(true))?;
    }
    for (key, scope) in [
        ("turn_token_usage", "turn"),
        ("thread_token_usage", "session"),
    ] {
        if let Some(usage) = p.get(key).filter(|v| v.is_object()) {
            writer.emit(json!({"type":"usage_summary","scope":scope,"cumulative":true,"includesChildren":null,"usage":tokens(usage)}),v)?;
        }
    }
    Ok(())
}
pub(super) fn summary(writer: &mut Writer, p: &Value, v: &Value) -> Result<()> {
    let mut r =
        json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null});
    let mut has = false;
    if let Some(usage) = p
        .pointer("/info/total_token_usage")
        .filter(|v| v.is_object())
    {
        r["usage"] = tokens(usage);
        has = true;
    }
    if let Some(size) = p
        .pointer("/info/model_context_window")
        .filter(|v| !v.is_null())
    {
        r["contextWindow"] = json!({"size":size});
        has = true;
    }
    let limits = &p["rate_limits"];
    if limits.is_object() {
        let mut quota = json!({});
        copy(&mut quota, "plan", limits, "plan_type");
        let mut windows = Vec::new();
        for key in ["primary", "secondary"] {
            let w = &limits[key];
            if let Some(percent) = w["used_percent"]
                .as_f64()
                .filter(|n| (0.0..=100.0).contains(n))
            {
                let mut window = json!({"usedPercent":percent});
                if let Some(t) = w["resets_at"]
                    .as_i64()
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                {
                    window["resetsAt"] =
                        json!(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                }
                windows.push(window);
            }
        }
        if !windows.is_empty() {
            quota["windows"] = json!(windows);
        }
        r["quota"] = quota;
        meta(&mut r, Provider::Codex, "rateLimits", limits);
        has = true;
    }
    if has {
        writer.emit(r, v)?;
    } else {
        writer.vendor(v, "token_count")?;
    }
    Ok(())
}
