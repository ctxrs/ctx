use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
use std::io::Write;

pub(super) fn parsed(row: &Value, key: &str) -> Option<Value> {
    row[key]
        .as_str()
        .and_then(ctx_history_capture_model::exact_json_value)
        .filter(numbers_preserved)
}
// serde_json retains i64/u64 exactly. Larger integral floats may already have rounded;
// leave the entire native JSON text unprojected in that case.
fn numbers_preserved(v: &Value) -> bool {
    match v {
        Value::Number(n) if n.is_f64() => n
            .as_f64()
            .is_some_and(|v| v.abs() <= 9_007_199_254_740_991.0 || v.fract() != 0.0),
        Value::Array(a) => a.iter().all(numbers_preserved),
        Value::Object(o) => o.values().all(numbers_preserved),
        _ => true,
    }
}
pub(super) fn safe(v: &Value) -> bool {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
            .or_else(|| n.as_u64().map(|n| n <= 9_007_199_254_740_991))
            .unwrap_or_else(|| {
                n.as_f64()
                    .is_some_and(|v| v.fract() != 0.0 || v.abs() <= 9_007_199_254_740_991.0)
            }),
        Value::Array(v) => v.iter().all(safe),
        Value::Object(v) => v.values().all(safe),
        _ => true,
    }
}
pub(super) fn row_meta(
    spool: &Spool,
    out: &mut Value,
    provider: Provider,
    row: &Value,
) -> Result<()> {
    let mut native = row.clone();
    if let Some(object) = native.as_object_mut() {
        object.retain(|k, _| !k.starts_with('_'));
    }
    // JSON text preserves signed/unsigned 64-bit integers without ASR rounding.
    json_meta(spool, out, provider, "row", &native)?;
    meta(
        out,
        provider,
        "source",
        &json!({"table":row["_table"],"rowid":row["_rowid"]}),
    );
    Ok(())
}
pub(super) fn part_meta(
    spool: &Spool,
    out: &mut Value,
    provider: Provider,
    part: &Value,
) -> Result<()> {
    json_meta(spool, out, provider, "part", part)
}
fn json_meta(
    spool: &Spool,
    out: &mut Value,
    provider: Provider,
    kind: &str,
    value: &Value,
) -> Result<()> {
    let text = value.to_string();
    if text.len() > 65536 {
        meta(
            out,
            provider,
            &format!("{kind}Blob"),
            &blob(spool, text.as_bytes(), "application/json")?,
        );
    } else {
        meta(out, provider, &format!("{kind}Json"), &json!(text));
    }
    Ok(())
}
pub(super) fn unix_time(value: &Value, millis: bool) -> Option<String> {
    let n = value.as_i64()?;
    let dt = if millis {
        chrono::DateTime::from_timestamp_millis(n)
    } else {
        chrono::DateTime::from_timestamp(n, 0)
    }?;
    if !(0..=9999).contains(&chrono::Datelike::year(&dt)) {
        return None;
    }
    Some(dt.to_rfc3339_opts(
        if millis {
            chrono::SecondsFormat::Millis
        } else {
            chrono::SecondsFormat::Secs
        },
        true,
    ))
}
pub(super) fn time(out: &mut Value, row: &Value, key: &str, millis: bool) {
    if let Some(ts) = unix_time(&row[key], millis) {
        out["ts"] = json!(ts);
        out["tsPrecision"] = json!(if millis { "ms" } else { "s" });
        out["tsSource"] = json!("native");
    }
}
pub(super) fn blob(spool: &Spool, bytes: &[u8], mime: &str) -> Result<Value> {
    let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
    file.write_all(bytes)?;
    let mut value = spool.blob(file)?;
    value["mimeType"] = json!(mime);
    Ok(value)
}
pub(super) fn binary(spool: &Spool, mime: &str, data: &str) -> Result<Value> {
    use base64::Engine;
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
        if bytes.len() > 65536 {
            return Ok(json!({"type":"opaque","format":mime,"blob":blob(spool,&bytes,mime)?}));
        }
        if mime.starts_with("image/") || mime.starts_with("audio/") {
            return Ok(
                json!({"type":if mime.starts_with("image/"){"image"}else{"audio"},"mimeType":mime,"data":data}),
            );
        }
    }
    Ok(json!({"type":"opaque","format":mime,"data":data}))
}
pub(super) fn opaque(spool: &Spool, part: &Value, provider: Provider) -> Result<Value> {
    let text = part.to_string();
    let mut out = json!({"type":"opaque","format":format!("{}/json",provider.domain())});
    if text.len() > 65536 {
        out["blob"] = blob(spool, text.as_bytes(), "application/json")?;
    } else {
        out["data"] = json!(text);
    }
    Ok(out)
}
pub(super) fn result_identity(
    writer: &mut Writer,
    out: &mut Value,
    parts: &Value,
    kind: &str,
    pointer: &str,
    id: Option<&str>,
) {
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        let count = parts
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == kind && p.pointer(pointer).and_then(Value::as_str) == Some(id))
            .count();
        if count == 1 {
            writer.keyed(&format!("result:{id}"));
            return;
        }
    }
    out["idScope"] = json!("capture");
}
pub(super) fn message(
    spool: &Spool,
    row: &Value,
    provider: Provider,
    id: Option<&str>,
    role: Option<&str>,
    parts: Vec<Value>,
) -> Result<Value> {
    let mut out = json!({"type":"message","messageId":id.unwrap_or(row["_key"].as_str().unwrap_or("unknown")),"content":parts});
    if id.is_none() {
        out["idSource"] = json!("derived");
    }
    if let Some(role) = role.filter(|r| matches!(*r, "user" | "assistant" | "system" | "developer"))
    {
        out["role"] = json!(role);
    } else {
        out["capture"] = json!({"role":{"state":"absent"}});
    }
    row_meta(spool, &mut out, provider, row)?;
    Ok(out)
}
pub(super) fn call(
    spool: &Spool,
    writer: &mut Writer,
    row: &Value,
    part: &Value,
    id: Option<&str>,
    name: Option<&str>,
    input: Option<&Value>,
) -> Result<Value> {
    let ambiguous = id
        .map(|id| spool.ambiguous_call(id))
        .transpose()?
        .unwrap_or(false);
    if let Some(id) = id {
        if !ambiguous {
            writer.keyed(&format!("call:{id}"));
        }
    }
    let mut out = json!({"type":"tool_call","callId":id.map(str::to_owned).unwrap_or_else(||writer.id()),"callIdSource":if id.is_some(){"native"}else{"derived"}});
    if let Some(name) = name {
        out["name"] = json!(name);
    } else {
        out["capture"] = json!({"name":{"state":"absent"}});
    }
    if let Some(input) = input {
        if let Some(text) = input.as_str() {
            if let Some(value) = ctx_history_capture_model::exact_json_value(text).filter(safe) {
                out["input"] = value;
            } else {
                out["inputText"] = json!(text);
            }
        } else if safe(input) {
            out["input"] = input.clone();
        } else {
            out["inputText"] = json!(input.to_string());
        }
    }
    if row["role"] == "assistant" {
        out["requestedBy"] = json!("model");
    }
    if writer.provider == Provider::Crush && name.is_some_and(|n| n.starts_with("mcp_")) {
        out["provider"] = json!({"kind":"mcp","identity":"unknown"});
    }
    if id.is_none() || ambiguous {
        out["idScope"] = json!("capture");
    }
    part_meta(spool, &mut out, writer.provider, part)?;
    Ok(out)
}
pub(super) fn index_call(spool: &Spool, id: &str) -> Result<()> {
    // Occurrences, including two calls in the same message, are distinct.
    spool.db.execute(
        "INSERT INTO native_calls VALUES(?1,(SELECT coalesce(max(rowid),0)+1 FROM native_calls))",
        [id],
    )?;
    Ok(())
}
pub(super) fn link(
    spool: &Spool,
    out: &mut Value,
    id: Option<&str>,
    provider: Provider,
) -> Result<()> {
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        let count: u64 = spool.db.query_row(
            "SELECT count(*) FROM native_calls WHERE call_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if count > 1 {
            out["linkage"] = json!("ambiguous");
            meta(out, provider, "unpairedCallId", &json!(id));
        } else {
            out["callId"] = json!(id);
            out["linkage"] = json!("exact");
        }
    } else {
        out["linkage"] = json!("none");
    }
    Ok(())
}
pub(super) fn usage(row: &Value, input: &str, output: &str, total: &str) -> Value {
    let mut usage = json!({});
    for (key, parent, child) in [(input, "input", "total"), (output, "output", "total")] {
        if let Some(n) = row[key].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
            usage[parent] = json!({child:n});
            if parent == "input" {
                usage[parent]["includesCache"] = Value::Null;
            }
        }
    }
    if let Some(n) = row[total].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
        usage["total"] = json!(n);
    }
    usage
}
pub(super) fn session_tail(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    let p = writer.provider;
    let mut info = json!({"type":"session_info"});
    for key in ["title", "name"] {
        if row[key].is_string() {
            copy(&mut info, "title", row, key);
        }
    }
    if row["user_set_name"] == 1 {
        info["titleSource"] = json!("user");
    }
    if row["project_id"].is_string() {
        info["project"] = json!({"id":row["project_id"]});
    }
    if row["archived_at"].is_string() {
        info["flags"] = json!(["archived"]);
    }
    row_meta(spool, &mut info, p, row)?;
    writer.emit(info, &Value::Null)?;
    let mut config = json!({"type":"config_change","snapshot":true});
    let model = parsed(row, "model_config_json");
    if let Some(name) = model.as_ref().and_then(|m| m["model_name"].as_str()) {
        config["model"] = json!({"id":name});
        copy(&mut config["model"], "provider", row, "provider_name");
    }
    copy(&mut config, "mode", row, "goose_mode");
    if config.get("model").is_some() || config.get("mode").is_some() {
        writer.emit(config, &Value::Null)?;
    }
    if let Some(cwd) = row["working_dir"].as_str() {
        writer.emit(
            json!({"type":"workspace_state","cwd":crate::records::path(cwd,None)}),
            &Value::Null,
        )?;
    }
    Ok(())
}
