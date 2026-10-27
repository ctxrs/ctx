use crate::{
    records::{meta, timestamp, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

pub(super) fn millis(v: &Value) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(v.as_i64()?)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}
pub(super) fn unsafe_number(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.as_u64().is_some_and(|n| n > 9_007_199_254_740_991)
                || n.as_i64().is_some_and(|n| n < -9_007_199_254_740_991)
        }
        Value::Array(a) => a.iter().any(unsafe_number),
        Value::Object(o) => o.values().any(unsafe_number),
        _ => false,
    }
}
pub(super) fn retain(record: &mut Value, w: &Writer, key: &str, v: &Value) {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("_asr_folded");
        o.remove("_asr_step_key");
    }
    if unsafe_number(&v) {
        let k = format!("{}/jsonText", w.provider.domain());
        record["_meta"][k][key] = json!(v.to_string());
    } else {
        meta(record, w.provider, key, &v);
    }
}
pub(super) fn parts(v: &Value) -> Vec<Value> {
    match v {
        Value::Null => vec![],
        Value::String(text) => vec![json!({"type":"text","text":text})],
        Value::Array(a) => a
            .iter()
            .map(|p| {
                if p["type"] == "text" && p["text"].is_string() {
                    json!({"type":"text","text":p["text"]})
                } else {
                    data(p)
                }
            })
            .collect(),
        _ => vec![data(v)],
    }
}
pub(super) fn data(v: &Value) -> Value {
    if unsafe_number(v) {
        json!({"type":"opaque","format":"application/json","data":v.to_string()})
    } else {
        json!({"type":"data","data":v})
    }
}
pub(super) fn message(
    w: &Writer,
    v: &Value,
    id: Option<&str>,
    role: Option<&str>,
    body: &Value,
) -> Value {
    let mut m = json!({"type":"message","messageId":id.filter(|_|!w.capture_scope).map(str::to_owned).unwrap_or_else(||format!("message:{}",w.id())),"content":parts(body)});
    if id.is_none() || w.capture_scope {
        m["idSource"] = json!("derived");
    }
    if let Some(role) = role.filter(|r| matches!(*r, "user" | "assistant" | "system" | "developer"))
    {
        m["role"] = json!(role);
    } else {
        m["capture"]["role"] = json!({"state":"absent"});
    }
    if body.is_null() {
        m["capture"]["content"] = json!({"state":"absent"});
    }
    retain(&mut m, w, "nativeRecord", v);
    m
}
pub(super) fn emit(w: &mut Writer, mut r: Value, v: &Value) -> Result<String> {
    if let Some(ids) = v.get("_asr_folded") {
        r["assembly"] = json!({"mode":"folded","sourceIds":ids});
    }
    let ts = v["timestamp"]
        .as_str()
        .and_then(timestamp)
        .or_else(|| millis(&v["timestampMs"]))
        .or_else(|| millis(&v["metadata"]["timestamp"]))
        .or_else(|| v["createdAt"].as_str().and_then(timestamp));
    // Do not feed provider-foreign turn/lineage aliases to the shared writer.
    if let Some(ts) = ts {
        r["ts"] = json!(ts);
    }
    w.emit(r, &json!({}))
}
pub(super) fn vendor(w: &mut Writer, v: &Value, kind: &str) -> Result<()> {
    let mut r = json!({"type":format!("{}/{}",w.provider.domain(),kind)});
    retain(&mut r, w, "nativeRecord", v);
    emit(w, r, v)?;
    Ok(())
}
pub(super) fn call(
    w: &Writer,
    id: Option<&str>,
    name: Option<&str>,
    input: Option<&Value>,
) -> Value {
    let mut r = json!({"type":"tool_call","callId":id.map(str::to_owned).unwrap_or_else(||format!("call:{}",w.id())),"callIdSource":if id.is_some(){"native"}else{"derived"}});
    if let Some(name) = name {
        r["name"] = json!(name);
    } else {
        r["capture"]["name"] = json!({"state":"absent"});
    }
    match input {
        Some(Value::String(text)) => r["inputText"] = json!(text),
        Some(v) if unsafe_number(v) => r["inputText"] = json!(v.to_string()),
        Some(v) => r["input"] = v.clone(),
        None => r["capture"]["input"] = json!({"state":"absent"}),
    }
    r
}
pub(super) fn result(spool: &Spool, w: &Writer, id: Option<&str>, status: &str) -> Result<Value> {
    let mut r = json!({"type":"tool_result","status":status,"linkage":"none"});
    if let Some(id) = id {
        if spool.ambiguous_call(id)? {
            r["linkage"] = json!("ambiguous");
            retain(&mut r, w, "nativeCallId", &json!(id));
        } else {
            r["callId"] = json!(id);
            r["linkage"] = json!("exact");
        }
    }
    Ok(r)
}
pub(super) fn count(v: &Value) -> Option<u64> {
    v.as_u64().filter(|n| *n <= 9_007_199_254_740_991)
}
pub(super) fn usage(v: &Value, fields: [&str; 5]) -> Value {
    let mut u = json!({});
    for (i, path) in ["input", "output", "input", "input", ""].iter().enumerate() {
        if let Some(n) = count(&v[fields[i]]) {
            let field = ["total", "total", "cacheRead", "cacheWrite", "total"][i];
            if path.is_empty() {
                u[field] = json!(n);
            } else {
                u[*path][field] = json!(n);
            }
        }
    }
    if u.pointer("/input/total").is_some() {
        u["input"]["includesCache"] = Value::Null;
    }
    u
}
