use super::{messages, push, sqlite};
use crate::{records::copy, spool::Spool, Error, Provider, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rmpv::Value as Packed;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub(super) fn ingest(spool: &mut Spool, db: &Connection, selector: &Value) -> Result<()> {
    let id = selector["nativeSession"].as_str().unwrap_or("");

    // Only writes.messages is chat. Checkpoint channel_values and other channels
    // are preserved as audit state, never recursively searched for messages.
    sqlite::rows(
        db,
        "writes",
        "WHERE thread_id=?2",
        "checkpoint_ns,checkpoint_id,task_id,idx",
        &[id],
        spool.limit,
        &mut |mut row| {
            let key = json!([
                "writes",
                id,
                row["checkpoint_ns"],
                row["checkpoint_id"],
                row["task_id"],
                row["idx"]
            ]);
            if row["_oversized"] == true {
                return super::gap(
                    spool,
                    json!(["writes", id, row["_rowid"]]),
                    &row,
                    "conversion_loss",
                );
            }
            let decoded = if row["type"] == "msgpack" {
                sqlite::blob(&row, "value").and_then(|b| decode(&b))
            } else {
                Err(Error::Invalid("unsupported LangGraph serialization".into()))
            };
            let value = match decoded {
                Ok(v) => v,
                Err(_) => {
                    return super::gap(spool, key, &row, "unparseable");
                }
            };
            row["value"] = value.clone();
            row.as_object_mut().map(|o| o.remove("_rowid"));
            if row["channel"] != "messages" || row["checkpoint_ns"] != "" {
                return push(
                    spool,
                    key,
                    json!({"type":"vendor","kind":"write","native":row}),
                    false,
                );
            }
            let items = value
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(std::slice::from_ref(&value));
            for (index, item) in items.iter().enumerate() {
                let body = message_body(item);
                let mut v = match body {
                    Some(body) => normalize(body),
                    None => json!({"type":"gap","reason":"unparseable"}),
                };
                let mut origin = row.clone();
                origin["value"] = item.clone();
                v["native"] = origin;
                let record_key = json!([key, index]);
                push(spool, record_key, v, false)?;
            }
            Ok(())
        },
    )?;
    sqlite::rows(
        db,
        "checkpoints",
        "WHERE thread_id=?2",
        "checkpoint_ns,checkpoint_id",
        &[id],
        spool.limit,
        &mut |mut row| {
            let key = json!([
                "checkpoints",
                id,
                row["checkpoint_ns"],
                row["checkpoint_id"],
                row["_rowid"]
            ]);
            for field in ["checkpoint", "metadata"] {
                if let Ok(bytes) = sqlite::blob(&row, field) {
                    let decoded = if field == "metadata" {
                        std::str::from_utf8(&bytes)
                            .ok()
                            .and_then(ctx_history_asr_core::numbers::decode)
                    } else if row["type"] == "msgpack" {
                        decode(&bytes).ok()
                    } else {
                        None
                    };
                    if let Some(value) = decoded {
                        row[field] = value;
                    }
                }
            }
            row.as_object_mut().map(|o| o.remove("_rowid"));
            if row["_oversized"] == true {
                return super::gap(spool, key, &row, "conversion_loss");
            }
            push(
                spool,
                key,
                json!({"type":"vendor","kind":"checkpoint","native":row}),
                false,
            )
        },
    )?;
    Ok(())
}

fn message_body(item: &Value) -> Option<&Value> {
    if item["msgpackExt"].as_i64() == Some(5) {
        let args = item["value"].as_array()?;
        // JsonPlusSerializer EXT_PYDANTIC_V2: module, class, fields, method.
        let module = args.first()?.as_str()?;
        let class = args.get(1)?.as_str()?;
        if !module.starts_with("langchain_core.messages")
            || !matches!(
                class,
                "HumanMessage" | "AIMessage" | "ToolMessage" | "SystemMessage" | "RemoveMessage"
            )
        {
            return None;
        }
        return args.get(2).filter(|v| v.is_object());
    }
    item.as_object().map(|_| item)
}
fn normalize(m: &Value) -> Value {
    let role = m["type"]
        .as_str()
        .or_else(|| m["role"].as_str())
        .unwrap_or("");
    if role == "remove" {
        return json!({"type":"deepRemove","body":m});
    }
    let role = match role {
        "human" => "user",
        "ai" => "assistant",
        other => other,
    };
    let mut v = json!({"type":"message","role":role,"body":m,"calls":messages::calls(Provider::DeepAgents,m)});
    for (to, from) in [
        ("messageId", "id"),
        ("callId", "tool_call_id"),
        ("content", "content"),
    ] {
        copy(&mut v, to, m, from);
    }
    if role == "tool" {
        if let Some(status) = m["status"].as_str() {
            match status {
                "success" => v["status"] = json!("completed"),
                "error" => v["status"] = json!("failed"),
                _ => {}
            }
        }
        copy(&mut v, "raw", m, "artifact");
    }
    if role == "assistant" {
        copy(&mut v, "usage", m, "usage_metadata");
        copy(&mut v, "model", &m["response_metadata"], "model_name");
        copy(
            &mut v,
            "stopReason",
            &m["response_metadata"],
            "finish_reason",
        );
    }
    v
}
fn decode(bytes: &[u8]) -> Result<Value> {
    let mut cursor = std::io::Cursor::new(bytes);
    let value = rmpv::decode::read_value(&mut cursor)
        .map_err(|_| Error::Invalid("invalid MessagePack".into()))?;
    if cursor.position() != bytes.len() as u64 {
        return Err(Error::Invalid("trailing MessagePack bytes".into()));
    }
    unpack(value, 0)
}
fn unpack(v: Packed, depth: u32) -> Result<Value> {
    if depth > 100 {
        return Err(Error::Invalid("nested MessagePack limit".into()));
    }
    Ok(match v {
        Packed::Nil => Value::Null,
        Packed::Boolean(v) => json!(v),
        Packed::Integer(v) => {
            if let Some(n) = v.as_i64() {
                json!(n)
            } else {
                json!(v.as_u64())
            }
        }
        Packed::F32(v) if v.is_finite() => json!(v),
        Packed::F64(v) if v.is_finite() => json!(v),
        Packed::String(v) => json!(v
            .as_str()
            .ok_or_else(|| Error::Invalid("invalid MessagePack string".into()))?),
        Packed::Binary(v) => json!({"msgpackBinary":STANDARD.encode(v)}),
        Packed::Array(v) => Value::Array(
            v.into_iter()
                .map(|v| unpack(v, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Packed::Map(v) => {
            let mut map = Map::new();
            for (k, v) in v {
                let key = k
                    .as_str()
                    .ok_or_else(|| Error::Invalid("nonstring MessagePack key".into()))?;
                if map.insert(key.into(), unpack(v, depth + 1)?).is_some() {
                    return Err(Error::Invalid("duplicate MessagePack key".into()));
                }
            }
            Value::Object(map)
        }
        Packed::Ext(tag, bytes) => {
            let mut cursor = std::io::Cursor::new(&bytes);
            let nested = rmpv::decode::read_value(&mut cursor)
                .ok()
                .filter(|_| cursor.position() == bytes.len() as u64);
            match nested {
                Some(v) => json!({"msgpackExt":tag,"value":unpack(v,depth+1)?}),
                None => json!({"msgpackExt":tag,"base64":STANDARD.encode(bytes)}),
            }
        }
        _ => return Err(Error::Invalid("nonfinite MessagePack number".into())),
    })
}

/// A messages-channel removal changes context; the durable writes remain audit
/// evidence. This is pruning, not an assertion that SQLite history was deleted.
pub(super) fn remove(spool: &Spool, writer: &mut crate::records::Writer, v: &Value) -> Result<()> {
    let id = v["body"]["id"].as_str().unwrap_or("");
    let mut stmt = spool.db.prepare(
        "SELECT key,value FROM entities WHERE key LIKE 'batch8:deepTarget:%' ORDER BY key",
    )?;
    let mut rows = stmt.query([])?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let mut value: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        if value["active"] == true && (id == "__remove_all__" || value["message"] == id) {
            if id != "__remove_all__" {
                targets.push(value["record"].clone());
            }
            value["active"] = json!(false);
            spool.save(&key, &value)?;
        }
    }
    let mut r = json!({"type":"compaction","compactionId":format!("{}-prune", writer.id()),"idSource":"derived","kind":"prune",
        "replaces":if id == "__remove_all__" {writer.tip.as_ref().map(|tip|json!({"throughId":tip})).unwrap_or_else(||json!({"ids":[]}))}else if targets.is_empty() {json!({"state":"unknown"})}else{json!({"ids":targets})}});
    super::values::preserve(spool, &mut r, writer.provider, "source", &v["native"])?;
    writer.emit(r, &json!({}))?;
    Ok(())
}
