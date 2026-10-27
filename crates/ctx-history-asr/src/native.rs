//! Mixed-store capture boundary. Each SQLite connection owns an independent read
//! transaction; serialization is by source position and then current-state tail.
mod sqlite;
use crate::{
    records::{self, Writer},
    source::Source,
    spool::Spool,
    Error, Input, Provider, Result,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
pub(crate) use sqlite::*;
use std::{io::Write, path::Path};

pub(crate) use crate::records::stable_id;
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
pub(crate) fn parse_json(text: &str) -> Option<Value> {
    ctx_history_asr_core::numbers::decode(text)
}
pub(crate) fn retain(r: &mut Value, p: Provider, key: &str, v: &Value) {
    records::meta(r, p, key, v);
}
fn numbers(s: &Spool, p: Provider, r: &mut Value) -> Result<()> {
    ctx_history_asr_core::registry::normalize(r, p);
    ctx_history_asr_core::blobs::numbers(&s.bundle, r, p)?;
    ctx_history_asr_core::blobs::externalize(&s.bundle, r)
}

pub(crate) fn lane_fact(
    db: &Connection,
    p: Provider,
    lane: &str,
    table: &str,
    key: &str,
    row: &Value,
) -> Result<()> {
    let value = json!({"lane":lane,"source":{"table":table,"key":key},"record":row});
    db.execute(
        "INSERT OR REPLACE INTO lane_facts SELECT ?1,?2,?3 WHERE NOT EXISTS (SELECT 1 FROM assigned_facts WHERE provider=?1 AND key=?2)",
        params![
            p.domain(),
            json!([lane, table, key]).to_string(),
            ctx_history_asr_core::numbers::text(&value)
        ],
    )?;
    Ok(())
}
pub(crate) fn assigned_fact(
    db: &Connection,
    p: Provider,
    lane: &str,
    table: &str,
    key: &str,
) -> Result<()> {
    let key = json!([lane, table, key]).to_string();
    db.execute(
        "INSERT OR IGNORE INTO assigned_facts VALUES(?1,?2)",
        params![p.domain(), key],
    )?;
    db.execute(
        "DELETE FROM lane_facts WHERE provider=?1 AND key=?2",
        params![p.domain(), key],
    )?;
    Ok(())
}
pub(crate) fn manifest_facts(
    db: &Connection,
    out: &mut impl Write,
    comma: &mut bool,
    bundle: &Path,
) -> Result<()> {
    let mut stmt = db.prepare("SELECT provider,value FROM lane_facts ORDER BY provider,key")?;
    let mut rows = stmt.query([])?;
    let mut provider = String::new();
    let mut any = false;
    while let Some(row) = rows.next()? {
        let p: String = row.get(0)?;
        if p != provider {
            if !any {
                if *comma {
                    out.write_all(b",")?;
                }
                *comma = true;
            } else {
                out.write_all(b"],")?;
            }
            serde_json::to_writer(&mut *out, &format!("{p}/unassigned"))?;
            out.write_all(b":[")?;
            provider = p;
            any = true;
        } else {
            out.write_all(b",")?;
        }
        let raw: String = row.get(1)?;
        let mut value = ctx_history_asr_core::numbers::decode(&raw)
            .ok_or_else(|| Error::Invalid("invalid staged lane fact".into()))?;
        if !ctx_history_asr_core::numbers::safe(&value["record"]) {
            value["blob"] = ctx_history_asr_core::blobs::store(
                bundle,
                ctx_history_asr_core::numbers::text(&value["record"]).as_bytes(),
            )?;
            value.as_object_mut().unwrap().remove("record");
        }
        serde_json::to_writer(&mut *out, &value)?;
    }
    if any {
        out.write_all(b"]")?;
    }
    Ok(())
}
