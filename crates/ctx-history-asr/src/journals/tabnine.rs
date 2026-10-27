use super::common::*;
use crate::{records::Writer, spool::Spool, Result};
use serde_json::{json, Value};
pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    if v.get("sessionId").is_some() {
        return vendor(w, v, "session_header");
    }
    let role = match v["type"].as_str() {
        Some("user") => Some("user"),
        Some("tabnine" | "gemini") => Some("assistant"),
        _ => None,
    };
    if role.is_none() {
        return vendor(w, v, "record");
    }
    let mut m = message(w, v, v["id"].as_str(), role, &v["content"]);
    for thought in v["thoughts"].as_array().into_iter().flatten() {
        if let Some(text) = thought["description"].as_str() {
            m["content"]
                .as_array_mut()
                .map(|a| a.push(json!({"type":"reasoning","text":text,"visibility":"full"})));
        }
    }
    if m["content"].as_array().is_some_and(|a| !a.is_empty()) {
        m.get_mut("capture")
            .and_then(Value::as_object_mut)
            .map(|o| o.remove("content"));
    }
    let message_id = emit(w, m, v)?;
    w.slot(10_000);
    let u = usage(
        &v["tokens"],
        ["input", "output", "cached", "cacheCreation", "total"],
    );
    if u.as_object().is_some_and(|o| !o.is_empty()) {
        let mut r = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"usage":u,"covers":{"fromId":message_id,"throughId":message_id}});
        retain(&mut r, w, "usageSource", v);
        emit(w, r, v)?;
    }
    for (n, t) in v["toolCalls"].as_array().into_iter().flatten().enumerate() {
        w.slot(20_000 + n as u64 * 10);
        let id = t["id"].as_str();
        let mut c = call(w, id, t["name"].as_str(), t.get("args"));
        c["requestedBy"] = json!("model");
        retain(&mut c, w, "nativeToolCall", t);
        emit(w, c, v)?;
        if let Some(output) = t.get("result") {
            let mut r = result(
                spool,
                w,
                id,
                match t["status"].as_str() {
                    Some("success" | "completed") => "completed",
                    Some("error") => "failed",
                    Some("cancelled") => "cancelled",
                    _ => "unknown",
                },
            )?;
            // Tabnine stores the native result here; the recorder does not prove
            // the eventual model rendering of arbitrary structured results.
            r["capture"]["content"] = json!({"state":"absent"});
            if unsafe_number(output) {
                retain(&mut r, w, "rawJson", output);
            } else {
                r["raw"] = output.clone();
            }
            emit(w, r, v)?;
        }
    }
    Ok(())
}
