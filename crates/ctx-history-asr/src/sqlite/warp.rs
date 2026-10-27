mod proto;
mod tools;
use super::{common::*, store};
use crate::{
    records::{meta, Writer},
    spool::Spool,
    Provider, Result,
};
use proto::{hex, unhex, Fields};
use serde_json::{json, Value};
const P: Provider = Provider::Warp;
fn message_arm(n: u32) -> bool {
    matches!(n,2..=6|9|10|15..=28)
}
pub(super) fn ingest(spool: &mut Spool, pos: &mut u64, mut row: Value) -> Result<()> {
    let task = row["task_id"].as_str().unwrap_or_default().to_owned();
    row["_key"] = json!(format!("task:{task}"));
    if row["_gap"].is_string() {
        row["_key"] = json!(format!("task:position:{}", row["_rowid"]));
        row["_capture"] = json!(true);
        return store::push(spool, pos, row);
    }
    let bytes = unhex(row["task"]["sqliteBlobHex"].as_str().unwrap_or_default())?;
    let native = blob(spool, &bytes, "application/x-protobuf")?;
    row["task"] = json!({"blob":native});
    let fields = match Fields::parse(&bytes) {
        Ok(f) => f,
        Err(_) => {
            row["_gap"] = json!("unparseable");
            return store::push(spool, pos, row);
        }
    };
    // Preserve the entire binary, including all fields outside this projection.
    let mut audit = row.clone();
    audit["_table"] = json!("warp_task");
    store::push(spool, pos, audit)?;
    let mut ids = std::collections::HashMap::<String, usize>::new();
    for message in fields.all(5) {
        if let Ok(f) = Fields::parse(message) {
            if let Ok(Some(id)) = f.text(1) {
                *ids.entry(id.into()).or_default() += 1;
            }
        }
    }
    for (ordinal, bytes) in fields.all(5).enumerate() {
        let decoded = Fields::parse(bytes);
        let mut out = json!({"_table":"warp_message","_rowid":row["_rowid"],"_key":format!("task:{task}:position:{ordinal}"),"_capture":true,"task_id":task,"ordinal":ordinal,"protobuf":hex(bytes)});
        if let Ok(f) = decoded {
            if let Ok(Some(id)) = f.text(1) {
                out["message_id"] = json!(id);
                if !id.is_empty() && ids.get(id) == Some(&1) {
                    out["_key"] = json!(format!("task:{task}:message:{id}"));
                    out["_capture"] = json!(false);
                }
            }
            if let Some((4, payload)) = f.oneof(message_arm) {
                if let Ok(call) = Fields::parse(&payload) {
                    if let Ok(Some(id)) = call.text(1) {
                        index_call(spool, id)?;
                    }
                }
            }
        } else {
            out["_gap"] = json!("unparseable");
        }
        store::push(spool, pos, out)?;
    }
    Ok(())
}
pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    if row["_table"] == "ai_queries" {
        let mut out = json!({"type":"dev.warp.agent/query"});
        row_meta(spool, &mut out, P, row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    }
    if row["_table"] == "agent_conversations" {
        return conversation(spool, writer, row);
    }
    if row["_table"] == "warp_task" {
        let mut out = json!({"type":"dev.warp.agent/task","native":{"format":"warp-task-protobuf","blob":row["task"]["blob"]}});
        row_meta(spool, &mut out, P, row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    }
    let bytes = unhex(row["protobuf"].as_str().unwrap_or_default())?;
    let parsed = project(spool, writer, row, &bytes);
    let mut out = match parsed {
        Ok(out) => out,
        Err(_) => json!({"type":"gap","reason":"unparseable","count":1}),
    };
    out["native"] = json!({"format":"warp-message-protobuf","blob":blob(spool,&bytes,"application/x-protobuf")?});
    meta(
        &mut out,
        P,
        "source",
        &json!({"table":"agent_tasks","taskId":row["task_id"],"messageOrdinal":row["ordinal"]}),
    );
    writer.emit(out, &Value::Null)?;
    Ok(())
}
fn project(spool: &Spool, writer: &mut Writer, row: &Value, bytes: &[u8]) -> Result<Value> {
    let fields = Fields::parse(bytes)?;
    let Some((kind, payload)) = fields.oneof(message_arm) else {
        return Ok(json!({"type":"dev.warp.agent/message"}));
    };
    let f = Fields::parse(&payload)?;
    let id = fields.text(1)?.filter(|s| !s.is_empty());
    let mut out = match kind {
        2 | 3 | 15 => {
            let text = f.text(1)?;
            let mut out = message(
                spool,
                &json!({"_key":row["_key"]}),
                P,
                id,
                Some(if kind == 2 { "user" } else { "assistant" }),
                Vec::new(),
            )?;
            out.as_object_mut().map(|o| o.remove("_meta"));
            if let Some(text) = text {
                if kind == 2 {
                    out["content"] = json!([{"type":"text","text":text}]);
                } else {
                    out["displayContent"] = if kind == 15 {
                        json!([{"type":"reasoning","visibility":"full","text":text}])
                    } else {
                        json!([{"type":"text","text":text}])
                    };
                    out["capture"] = json!({"content":{"state":"absent"}});
                }
            } else {
                out["capture"] = json!({"content":{"state":"absent"}});
            }
            if kind != 2 {
                out["author"] = json!("model");
                out["audience"] = json!("user");
            }
            if kind == 2 {
                if let Some(context) = f.merged(2) {
                    meta(
                        &mut out,
                        P,
                        "inputContext",
                        &blob(spool, &context, "application/x-protobuf")?,
                    );
                }
            }
            out
        }
        4 => tools::call(spool, writer, row, &f)?,
        5 => tools::result(spool, &f)?,
        16 => {
            let mut out = json!({"type":"compaction","compactionId":id.map(str::to_owned).unwrap_or_else(||writer.id()),"kind":"summary","replaces":{"state":"unknown"}});
            if id.is_none() {
                out["idSource"] = json!("derived");
            }
            if let Some((1, b)) = f.oneof(|n| n == 1 || n == 3) {
                let summary = Fields::parse(&b)?;
                if let Some(text) = summary.text(1)? {
                    out["summary"] = json!([{"type":"text","text":text}]);
                }
                if let Some(n) = summary.int(2).filter(|n| *n <= 9_007_199_254_740_991) {
                    out["tokens"] = json!({"after":n});
                }
            }
            out
        }
        _ => json!({"type":"dev.warp.agent/message","variant":kind}),
    };
    if let Some(request) = fields.text(13)? {
        meta(&mut out, P, "requestId", &json!(request));
    }
    // The pinned schema also has a per-message timestamp; never substitute the task's modified time.
    if let Some(ts) = fields.merged(14) {
        let ts = Fields::parse(&ts)?;
        let seconds = ts.int(1).unwrap_or(0) as i64;
        let nanos = ts.int(2).unwrap_or(0);
        if nanos < 1_000_000_000 && (-62_135_596_800..=253_402_300_799).contains(&seconds) {
            if let Some(dt) = chrono::DateTime::from_timestamp(seconds, nanos as u32) {
                out["ts"] = json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
                out["tsPrecision"] = json!("ns");
                out["tsSource"] = json!("native");
            }
        }
    }
    Ok(out)
}
fn conversation(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    let mut safe_row = row.clone();
    if let Some(mut data) = parsed(row, "conversation_data") {
        if let Some(o) = data.as_object_mut() {
            for key in [
                "server_conversation_token",
                "forked_from_server_conversation_token",
            ] {
                if o.remove(key).is_some() {
                    o.insert(format!("{key}_present"), json!(true));
                }
            }
        }
        safe_row["conversation_data"] = json!(data.to_string());
    }
    let mut info = json!({"type":"session_info"});
    if let Some(data) = parsed(&safe_row, "conversation_data") {
        if data["agent_name"].is_string() {
            info["title"] = data["agent_name"].clone();
        }
        if data["pinned"] == true {
            info["flags"] = json!(["pinned"]);
        }
    }
    row_meta(spool, &mut info, P, &safe_row)?;
    writer.emit(info, &Value::Null)?;
    Ok(())
}
