//! Mixed-store capture boundary. Each SQLite connection owns an independent read
//! transaction; serialization is by source position and then current-state tail.
mod sqlite;
use crate::{
    records::{self, Writer},
    source::Source,
    spool::Spool,
    Input, Provider, Result,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub(crate) use sqlite::*;
use std::{io::Write, path::Path};

pub(crate) fn stable_id(identity: &str) -> String {
    format!("n{:x}", Sha256::digest(identity.as_bytes()))
}
pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let input = Input {
        provider: input.provider,
        path: std::fs::canonicalize(&input.path).unwrap_or_else(|_| input.path.clone()),
    };
    match input.provider {
        Provider::OpenClaw => crate::openclaw::visit(&input, limit, visitor, unidentified),
        Provider::NanoClaw => crate::nanoclaw::visit(&input, limit, visitor, unidentified),
        _ => Ok(()),
    }
}
pub(crate) fn ingest(
    s: &mut Spool,
    p: Provider,
    path: &Path,
    id: &str,
    h: &mut Value,
    bundle: &Connection,
) -> Result<()> {
    match p {
        Provider::OpenClaw => crate::openclaw::ingest(s, path, id, h, bundle),
        Provider::NanoClaw => crate::nanoclaw::ingest(s, path, id, h),
        _ => Ok(()),
    }
}
pub(crate) fn header(p: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":p.agent()},"sessionId":id,
        "producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":"ctx-native-sqlite-v1",
        "source":{"format":p.format()},"lossy":["order","revision_history"]},"profiles":["core"],"contentLevel":"full"});
    if let Some(fields) = n.get("asr").and_then(Value::as_object) {
        for (key, value) in fields {
            h[key] = value.clone();
        }
    }
    if let Some(version) = n.get("schemaVersion") {
        h["producer"]["source"]["formatVersion"] = version.clone();
    }
    h
}
pub(crate) fn loss(s: &Spool, value: &str) -> Result<()> {
    s.save(&format!("loss:{value}"), &json!(value))
}
pub(crate) fn finish_header(s: &Spool, _p: Provider, h: &mut Value) -> Result<()> {
    let mut stmt =
        s.db.prepare("SELECT value FROM entities WHERE key LIKE 'loss:%' ORDER BY key")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let value: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
        if let Some(losses) = h["producer"]["lossy"].as_array_mut() {
            if !losses.contains(&value) {
                losses.push(value);
            }
        }
    }
    let mut stmt =
        s.db.prepare("SELECT value FROM entities WHERE key LIKE 'participant:%' ORDER BY key")?;
    let mut rows = stmt.query([])?;
    let mut people = Vec::new();
    while let Some(row) = rows.next()? {
        people.push(serde_json::from_str::<Value>(&row.get::<_, String>(0)?)?);
    }
    if !people.is_empty() {
        h["participants"] = json!(people);
    }
    Ok(())
}
pub(crate) fn convert(s: &mut Spool, w: &mut Writer, h: &mut Value, v: &Value) -> Result<()> {
    w.turn = None;
    if v["table"] == "_gap" {
        emit(
            s,
            w,
            json!({"type":"gap","reason":v["row"]["reason"],"count":1}),
            v,
        )?;
        return Ok(());
    }
    match w.provider {
        Provider::OpenClaw => crate::openclaw::convert(s, w, h, v),
        Provider::NanoClaw => crate::nanoclaw::convert(s, w, h, v),
        _ => Ok(()),
    }
}
pub(crate) fn stage(
    s: &mut Spool,
    table: &str,
    key: &str,
    mut row: Value,
    snapshot: bool,
) -> Result<()> {
    if let Some(reason) = row["_gap"].as_str() {
        return s.push_native(
            &json!({"table":"_gap", "identity":format!("gap:{}",s.source_bytes),
            "positional":true,"row":{"reason":reason},"snapshot":snapshot}),
        );
    }
    let positional = row
        .as_object_mut()
        .and_then(|o| o.remove("_captureIdentity"))
        == Some(json!(true));
    s.push_native(
        &json!({"table":table,"identity":json!([table,key]).to_string(),"row":row,"snapshot":snapshot,"positional":positional}),
    )
}
pub(crate) fn emit(s: &Spool, w: &mut Writer, mut record: Value, n: &Value) -> Result<String> {
    if n["positional"] == true {
        record["idScope"] = json!("capture");
    }
    if record["type"] == "gap" {
        loss(s, "_rs.ctx/native_gap")?;
    }
    if n["snapshot"] == true {
        record["assembly"] = json!({"mode":"snapshot"});
    }
    numbers(s, w.provider, &mut record)?;
    w.emit(record, &json!({}))
}
pub(crate) fn vendor(s: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let mut record =
        json!({"type":format!("{}/{}",w.provider.domain(),n["table"].as_str().unwrap_or("row"))});
    retain(&mut record, w.provider, "row", &n["row"]);
    emit(s, w, record, n)?;
    Ok(())
}
pub(crate) fn time(v: &Value) -> Option<String> {
    if let Some(text) = v.as_str() {
        return records::timestamp(text);
    }
    chrono::DateTime::from_timestamp_millis(v.as_i64()?)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}
pub(crate) fn set_time(r: &mut Value, key: &str, v: &Value) {
    if let Some(t) = time(v) {
        r[key] = json!(t);
    }
}
pub(crate) fn parse(v: &Value) -> Value {
    v.as_str().and_then(parse_json).unwrap_or(Value::Null)
}
// Reject integer literals that serde_json would round through f64. Native
// signed/unsigned 64-bit integers remain exact until ASR JSON-text projection.
pub(crate) fn parse_json(text: &str) -> Option<Value> {
    let value = ctx_history_capture_model::exact_json_value(text)?;
    let mut quoted = false;
    let mut escaped = false;
    let mut token = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        if c.is_ascii_digit()
            || c == '-'
            || (!token.is_empty() && matches!(c, '+' | '.' | 'e' | 'E'))
        {
            token.push(c);
            continue;
        }
        if !token.is_empty() {
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return None;
            }
            token.clear();
        }
        if c == '"' {
            quoted = true;
        }
    }
    Some(value)
}
fn large_binary(v: &Value) -> bool {
    match v {
        Value::Object(o) => {
            let binary = v["mimeType"].is_string()
                || v["mime_type"].is_string()
                || v["encoding"] == "base64"
                || matches!(v["type"].as_str(), Some("image" | "audio"));
            (binary
                && ["data", "blob"]
                    .iter()
                    .any(|k| v[*k].as_str().is_some_and(|s| s.len() > 87380)))
                || o.values().any(large_binary)
        }
        Value::Array(a) => a.iter().any(large_binary),
        _ => false,
    }
}
pub(crate) fn unsafe_number(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.as_i64()
                .is_some_and(|x| !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&x))
                || n.as_u64().is_some_and(|x| x > 9_007_199_254_740_991)
                || n.as_f64()
                    .is_some_and(|x| x.fract() == 0.0 && x.abs() > 9_007_199_254_740_991.0)
        }
        Value::Array(a) => a.iter().any(unsafe_number),
        Value::Object(o) => o.values().any(unsafe_number),
        _ => false,
    }
}
pub(crate) fn retain(r: &mut Value, p: Provider, key: &str, v: &Value) {
    if unsafe_number(v) {
        records::meta(r, p, &format!("{key}JsonText"), &json!(v.to_string()));
    } else {
        records::meta(r, p, key, v);
    }
}
fn numbers(s: &Spool, p: Provider, r: &mut Value) -> Result<()> {
    if unsafe_number(&r["input"]) {
        r["inputText"] = json!(r["input"].to_string());
        r.as_object_mut().map(|o| o.remove("input"));
    }
    for key in ["raw", "content", "displayContent"] {
        if unsafe_number(&r[key]) || large_binary(&r[key]) {
            let mut f = tempfile::NamedTempFile::new_in(&s.bundle)?;
            serde_json::to_writer(&mut f, &r[key])?;
            let blob = s.blob(f)?;
            r.as_object_mut().map(|o| o.remove(key));
            if key == "content" && r["type"] == "message" {
                r[key] = json!([]);
            }
            r["capture"][key] = json!({"state":"external","blob":blob});
        }
    }
    if let Some(meta) = r.get_mut("_meta").and_then(Value::as_object_mut) {
        let keys: Vec<_> = meta
            .iter()
            .filter(|(_, v)| unsafe_number(v) || large_binary(v))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(v) = meta.remove(&key) {
                if large_binary(&v) {
                    let mut file = tempfile::NamedTempFile::new_in(&s.bundle)?;
                    serde_json::to_writer(&mut file, &v)?;
                    meta.insert(format!("{key}JsonBlob"), s.blob(file)?);
                } else {
                    meta.insert(format!("{key}JsonText"), json!(v.to_string()));
                }
            }
        }
    }
    if unsafe_number(r) {
        // Known normalized fields cannot carry unsafe counts. Preserve the whole
        // affected member, without converting a native integer into a string in situ.
        let keys: Vec<_> = r
            .as_object()
            .into_iter()
            .flat_map(|o| o.iter())
            .filter(|(_, v)| unsafe_number(v))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(v) = r.as_object_mut().and_then(|o| o.remove(&key)) {
                retain(r, p, &format!("{key}JsonText"), &json!(v.to_string()));
            }
        }
    }
    Ok(())
}
pub(crate) fn lane_fact(
    db: &Connection,
    p: Provider,
    lane: &str,
    table: &str,
    key: &str,
    row: &Value,
) -> Result<()> {
    let mut value = json!({"conversationKey":lane,"source":{"table":table,"key":key}});
    if unsafe_number(row) {
        value["jsonText"] = json!(row.to_string());
    } else {
        value["record"] = row.clone();
    }
    db.execute(
        "INSERT OR REPLACE INTO lane_facts VALUES(?1,?2,?3)",
        params![
            p.domain(),
            json!([lane, table, key]).to_string(),
            value.to_string()
        ],
    )?;
    Ok(())
}
pub(crate) fn manifest_facts(db: &Connection, out: &mut impl Write) -> Result<()> {
    let mut stmt = db.prepare("SELECT provider,value FROM lane_facts ORDER BY provider,key")?;
    let mut rows = stmt.query([])?;
    let mut provider = String::new();
    let mut any = false;
    while let Some(row) = rows.next()? {
        let p: String = row.get(0)?;
        if p != provider {
            if !any {
                out.write_all(b",\"_meta\":{")?;
            } else {
                out.write_all(b"],")?;
            }
            serde_json::to_writer(&mut *out, &format!("{p}/unassignedLaneFacts"))?;
            out.write_all(b":[")?;
            provider = p;
            any = true;
        } else {
            out.write_all(b",")?;
        }
        out.write_all(row.get::<_, String>(1)?.as_bytes())?;
    }
    if any {
        out.write_all(b"]}")?;
    }
    Ok(())
}
