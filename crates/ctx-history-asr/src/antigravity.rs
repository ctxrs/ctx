//! The admitted transcript dialect is a snapshot of trajectory steps. Its
//! conversation directory owns identity; Gemini/Qwen field names are not lineage.
use crate::{
    genai,
    records::{extras, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(crate) fn convert(spool: &Spool, w: &mut Writer, v: &Value) -> Result<()> {
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), v)?;
        return Ok(());
    }
    let mut source = v.clone();
    if let Some(ts) = v.get("created_at") {
        source["timestamp"] = ts.clone();
    }
    let kind = v["type"].as_str().unwrap_or("unknown");
    if !matches!(
        kind,
        "USER_INPUT"
            | "PLANNER_RESPONSE"
            | "CODE_ACTION"
            | "SYSTEM_MESSAGE"
            | "CONVERSATION_HISTORY"
    ) {
        return w.vendor(v, kind);
    }
    let id = v["step_index"]
        .as_u64()
        .map(|n| format!("step:{n}"))
        .unwrap_or_else(|| format!("step:{}", w.pos));
    let mut r = json!({"type":"message","messageId":id,"idSource":"derived","content":[],"displayContent":genai::parts(&v["content"],Provider::Antigravity),"capture":{"content":{"state":"absent"},"role":{"state":"absent"}},"assembly":{"mode":"snapshot"}});
    meta(&mut r, Provider::Antigravity, "stepType", &v["type"]);
    if !v["step_index"].is_u64() {
        r["idScope"] = json!("capture");
    }
    // Transcripts are rendered trajectory logs, not the actual API request.
    match kind {
        "SYSTEM_MESSAGE" => {
            r["author"] = json!("harness");
            r["origin"] = json!("notice");
            r["audience"] = json!("user");
        }
        "PLANNER_RESPONSE" => {
            r["author"] = json!("model");
            r["role"] = json!("assistant");
            r["capture"].as_object_mut().unwrap().remove("role");
        }
        _ => {}
    }
    if let Some(text) = v["thinking"].as_str() {
        r["displayContent"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"reasoning","text":text}));
    }
    extras(
        &mut r,
        Provider::Antigravity,
        v,
        &["content", "thinking", "tool_calls"],
    );
    w.emit(r, &source)?;
    for (i, call) in v["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let mut r = genai::call(call, &format!("call:{id}:{i}"), Provider::Antigravity);
        if !matches!(v["source"].as_str(), Some("planner" | "assistant")) {
            r.as_object_mut().unwrap().remove("requestedBy");
        }
        r["assembly"] = json!({"mode":"snapshot"});
        meta(
            &mut r,
            Provider::Antigravity,
            "step_index",
            &v["step_index"],
        );
        w.emit(r, &source)?;
    }
    let _ = spool;
    Ok(())
}
