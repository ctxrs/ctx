use crate::{
    content,
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
use std::io::Write;
pub(super) const SAFE: u64 = 9_007_199_254_740_991;
pub(super) fn safe(v: &Value) -> bool {
    match v {
        Value::Number(n) => n
            .as_f64()
            .is_some_and(|f| f.is_finite() && f.abs() <= SAFE as f64),
        Value::Array(a) => a.iter().all(safe),
        Value::Object(o) => o.values().all(safe),
        _ => true,
    }
}
pub(super) fn parse(raw: &str) -> Option<Value> {
    // Inspect number tokens before f64 parsing: overflowing integers must never be rounded.
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if bytes[i].is_ascii_digit() || bytes[i] == b'-' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || b".eE+-".contains(&bytes[i])) {
                i += 1;
            }
            if raw[start..i]
                .parse::<f64>()
                .ok()
                .is_none_or(|n| !n.is_finite() || n.abs() > SAFE as f64)
            {
                return None;
            }
        } else {
            i += 1;
        }
    }
    ctx_history_capture_model::exact_json_value(raw).filter(safe)
}
pub(super) fn decoded(v: &Value) -> Value {
    v.as_str().and_then(parse).unwrap_or_else(|| v.clone())
}
pub(super) fn id(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}
pub(super) fn extra(v: &mut Value, p: Provider, key: &str, raw: &Value) {
    if safe(raw) {
        meta(v, p, key, raw);
    } else {
        meta(v, p, &format!("{key}JsonText"), &json!(raw.to_string()));
    }
}
pub(super) fn native(v: &mut Value, p: Provider, s: &Spool, key: &str, raw: &Value) -> Result<()> {
    let mut raw = raw.clone();
    if let Some(o) = raw.as_object_mut() {
        for k in ["_sessionId", "_namespace", "_copySession", "_forkOwner"] {
            o.remove(k);
        }
    }
    let bytes = serde_json::to_vec(&raw)?;
    if bytes.len() > 64 * 1024 {
        let mut file = tempfile::NamedTempFile::new_in(&s.bundle)?;
        file.write_all(&bytes)?;
        meta(v, p, &format!("{key}Blob"), &s.blob(file)?);
    } else {
        extra(v, p, key, &raw);
    }
    Ok(())
}
pub(super) fn emit(s: &Spool, w: &mut Writer, mut v: Value, n: &Value) -> Result<String> {
    native(&mut v, w.provider, s, "source", n)?;
    if v.get("copiedFrom").is_none() {
        if let Some(copy) = s.entity("native_copy")?.filter(Value::is_object) {
            v["copiedFrom"] = copy;
        }
    }
    // Named projections prevent an optional earlier field from renumbering later
    // records. A unique native call/result also survives a snapshot's row movement.
    let original_scope = w.capture_scope;
    let kind = v["type"].as_str().unwrap_or("");
    let mut basis = w.native_identity.clone().unwrap_or_default();
    if let Some(id) = v["callId"].as_str() {
        let table = match kind {
            "tool_call" => Some("native_calls"),
            "tool_result" => Some("tool_results"),
            _ => None,
        };
        if let Some(table) = table {
            let count: u64 = s.db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE call_id=?1"),
                [id],
                |r| r.get(0),
            )?;
            if count == 1 {
                basis = format!("{kind}:{id}");
                w.capture_scope = false;
            }
        }
    }
    let qualifier = ["inferenceId", "counter", "turnId"]
        .into_iter()
        .filter_map(|k| v.get(k).map(|v| (k, v)))
        .collect::<Vec<_>>();
    let projection = json!([basis, kind, qualifier]).to_string();
    let occurrence = w.native_emissions.entry(projection.clone()).or_default();
    w.native_projection = Some(format!("{projection}:{occurrence}"));
    *occurrence += 1;
    let result = w.emit(v, &json!({}));
    w.native_projection = None;
    w.capture_scope = original_scope;
    result
}
pub(super) fn vendor(s: &Spool, w: &mut Writer, kind: &str, n: &Value) -> Result<()> {
    emit(
        s,
        w,
        json!({"type":format!("{}/{kind}",w.provider.domain())}),
        n,
    )?;
    Ok(())
}
pub(super) fn time(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    crate::records::timestamp(s).or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|n| {
                n.and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            })
    })
}
pub(super) fn epoch(v: &Value) -> Option<String> {
    let text = v.as_number()?.to_string();
    let (seconds, fraction) = text.split_once('.').unwrap_or((&text, ""));
    if fraction.len() > 9 || fraction.bytes().any(|b| !b.is_ascii_digit()) {
        return None;
    }
    let nanos = format!("{fraction:0<9}").parse::<u32>().ok()?;
    let seconds = seconds.parse::<i64>().ok()?;
    if seconds < 0 && nanos > 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(seconds, nanos)
        .map(|n| n.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}
pub(super) fn timestamp(v: &mut Value, n: &Value, key: &str, epoch_seconds: bool) {
    if let Some(t) = if epoch_seconds {
        epoch(&n[key])
    } else {
        time(&n[key])
    } {
        v["ts"] = json!(t);
    }
}
pub(super) fn parts(v: &Value, p: Provider) -> Vec<Value> {
    content::parts(v, p)
}
pub(super) fn binaries(s: &Spool, parts: &mut [Value]) -> Result<()> {
    use base64::Engine;
    for p in parts {
        if matches!(p["type"].as_str(), Some("image" | "audio")) {
            if let Some(data) = p["data"].as_str().filter(|d| d.len() > 87_380) {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    if bytes.len() > 65_536 {
                        let mut file = tempfile::NamedTempFile::new_in(&s.bundle)?;
                        file.write_all(&bytes)?;
                        let mut blob = s.blob(file)?;
                        blob["mimeType"] = p["mimeType"].clone();
                        p["blob"] = blob;
                        p.as_object_mut().unwrap().remove("data");
                    }
                }
            }
        }
    }
    Ok(())
}
pub(super) fn message(s: &Spool, w: &Writer, id: &str, role: &str, body: &Value) -> Result<Value> {
    let mut parts = parts(body, w.provider);
    binaries(s, &mut parts)?;
    let mut v = json!({"type":"message","messageId":id,"role":role,"content":parts});
    if body.is_null() {
        v["capture"] = json!({"content":{"state":"absent"}});
    }
    Ok(v)
}
pub(super) fn index_calls(s: &Spool, calls: &Value) -> Result<()> {
    for c in calls.as_array().into_iter().flatten() {
        if let Some(id) = c["id"].as_str() {
            s.db.execute(
                "INSERT INTO native_calls VALUES(?1,(SELECT count(*)+1 FROM native_calls))",
                [id],
            )?;
        }
    }
    Ok(())
}
pub(super) fn index_result(s: &Spool, id: Option<&str>) -> Result<()> {
    if let Some(id) = id {
        s.db.execute(
            "INSERT INTO tool_results VALUES(?1,(SELECT count(*)+1 FROM tool_results))",
            [id],
        )?;
    }
    Ok(())
}
pub(super) fn call(
    s: &Spool,
    w: &mut Writer,
    n: &Value,
    id: Option<&str>,
    name: Option<&str>,
    args: &Value,
) -> Result<()> {
    let mut v = json!({"type":"tool_call","callId":id.map(str::to_owned).unwrap_or_else(||w.id()),"callIdSource":if id.is_some(){"native"}else{"derived"},"requestedBy":"model"});
    if let Some(name) = name {
        v["name"] = json!(name);
    } else {
        v["capture"] = json!({"name":{"state":"absent"}});
    }
    if let Some(raw) = args.as_str() {
        if let Some(parsed) = parse(raw) {
            v["input"] = parsed;
        } else {
            v["inputText"] = json!(raw);
        }
    } else if !args.is_null() {
        if safe(args) {
            v["input"] = args.clone();
        } else {
            v["inputText"] = json!(args.to_string());
        }
    }
    emit(s, w, v, n)?;
    Ok(())
}
pub(super) fn result(s: &Spool, w: &Writer, id: Option<&str>, body: &Value) -> Result<Value> {
    let mut parts = parts(body, w.provider);
    binaries(s, &mut parts)?;
    let mut v = json!({"type":"tool_result","linkage":"none","status":"unknown","content":parts});
    if let Some(id) = id {
        if s.ambiguous_call(id)? {
            v["linkage"] = json!("ambiguous");
            extra(&mut v, w.provider, "callId", &json!(id));
        } else {
            v["linkage"] = json!("exact");
            v["callId"] = json!(id);
        }
    }
    if body.is_null() {
        v["capture"] = json!({"content":{"state":"absent"}});
    }
    Ok(v)
}
pub(super) fn usage(n: &Value, uncached: bool) -> Value {
    let mut usage = json!({});
    for (key, group, field) in [
        (
            "input_tokens",
            "input",
            if uncached { "uncached" } else { "total" },
        ),
        ("output_tokens", "output", "total"),
        ("cache_read_tokens", "input", "cacheRead"),
        ("cache_write_tokens", "input", "cacheWrite"),
        ("cache_read_input_tokens", "input", "cacheRead"),
        ("cache_creation_input_tokens", "input", "cacheWrite"),
        ("reasoning_tokens", "output", "reasoning"),
    ] {
        if let Some(n) = n[key].as_u64().filter(|n| *n <= SAFE) {
            usage[group][field] = json!(n);
        }
    }
    if usage["input"].get("total").is_some() {
        usage["input"]["includesCache"] = Value::Null;
    }
    usage
}
pub(super) fn summary(n: &Value, uncached: bool, counter: &str) -> Option<Value> {
    let usage = usage(n, uncached);
    let mut v = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":counter});
    if usage.as_object().is_some_and(|o| !o.is_empty()) {
        v["usage"] = usage;
    }
    if let Some(c) = n["api_call_count"].as_u64().filter(|n| *n <= SAFE) {
        v["callCount"] = json!(c);
    }
    // Hermes's per-model table defaults both cost columns to zero even when
    // cost is unknown. Its nullable session columns have no such placeholder.
    let model_counter = counter.starts_with("session_model_usage:");
    let known = |field: &str, statuses: &[&str]| {
        n[field].as_f64().is_some_and(|amount| {
            amount >= 0.0
                && (!model_counter
                    || amount > 0.0
                    || n["cost_status"]
                        .as_str()
                        .is_some_and(|s| statuses.contains(&s)))
        })
    };
    if known("estimated_cost_usd", &["estimated", "included"]) {
        v["cost"] =
            json!({"amount":n["estimated_cost_usd"],"currency":"USD","source":"harness_estimated"});
    }
    if known("actual_cost_usd", &["actual"]) {
        v["cost"] = json!({"amount":n["actual_cost_usd"],"currency":"USD"});
        if matches!(
            n["cost_source"].as_str(),
            Some("provider_cost_api" | "provider_generation_api")
        ) {
            v["cost"]["source"] = json!("provider_reported");
        }
    }
    if v.get("cost").is_some() {
        copy(&mut v["cost"], "pricingVersion", n, "pricing_version");
        if n["billing_mode"] == "subscription_included" {
            v["cost"]["billing"] = json!("subscription");
        }
    }
    (v.get("usage").is_some() || v.get("cost").is_some() || v.get("callCount").is_some())
        .then_some(v)
}
pub(super) fn session_info(s: &Spool, w: &mut Writer, n: &Value, title: &str) -> Result<()> {
    let mut v = json!({"type":"session_info"});
    copy(&mut v, "title", n, title);
    let flags: Vec<_> = ["archived", "pinned", "deleted"]
        .into_iter()
        .filter(|k| n[*k] == true || n[*k] == 1)
        .collect();
    if !flags.is_empty() {
        v["flags"] = json!(flags);
    }
    emit(s, w, v, n)?;
    Ok(())
}
