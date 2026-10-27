use super::common::*;
use crate::{records::Writer, spool::Spool, Result};
use ruopencode_source::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value, snapshot: bool) -> Result<()> {
    if !snapshot && selected_partial(spool, v)? {
        return Ok(());
    }
    let Some(role) = v["role"]
        .as_str()
        .filter(|r| matches!(*r, "user" | "assistant" | "system"))
    else {
        return vendor(w, v, "record");
    };
    let mut body = Vec::new();
    for p in v["parts"].as_array().into_iter().flatten() {
        match p["type"].as_str() {
            Some("dynamic-tool") => {},
            Some("text") if p["text"].is_string() => body.push(json!({"type":"text","text":p["text"]})),
            Some("reasoning") if p["text"].is_string() => body.push(json!({"type":"reasoning","visibility":"full","text":p["text"]})),
            Some("file") if p["url"].is_string() => body.push(json!({"type":"resource_link","uri":p["url"],"name":p["filename"].as_str().unwrap_or("attachment")})),
            _ => body.push(data(p)),
        }
    }
    let mut m = message(w, v, v["id"].as_str(), Some(role), &Value::Null);
    m["content"] = json!(body);
    m.get_mut("capture")
        .and_then(Value::as_object_mut)
        .map(|o| o.remove("content"));
    if v["metadata"]["partial"] == true {
        m["status"] = json!("partial");
    }
    let message_id = emit(w, m, v)?;
    let metadata = &v["metadata"];
    w.slot(10_000);
    let u = usage(
        &metadata["usage"],
        [
            "inputTokens",
            "outputTokens",
            "cachedInputTokens",
            "cacheWriteTokens",
            "totalTokens",
        ],
    );
    if u.as_object().is_some_and(|o| !o.is_empty()) {
        // StreamManager persists totalUsage across steps: this is not one call.
        let mut r = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"usage":u,"covers":{"fromId":message_id,"throughId":message_id}});
        retain(&mut r, w, "usageSource", metadata);
        emit(w, r, v)?;
    }
    if metadata["compactionBoundary"] == true
        || metadata["compacted"] == true
        || metadata["compacted"]
            .as_str()
            .is_some_and(|s| matches!(s, "user" | "idle"))
    {
        w.slot(15_000);
        let mut r = json!({"type":"compaction","replaces":{"state":"unknown"},"summary":body});
        if let Some(reason) = metadata["compacted"].as_str() {
            r["reason"] = json!(reason);
        }
        retain(&mut r, w, "compactionMetadata", metadata);
        emit(w, r, v)?;
    }
    w.slot(20_000);
    for p in v["parts"].as_array().into_iter().flatten() {
        if p["type"] != "dynamic-tool" {
            continue;
        }

        tool(spool, w, v, p, None)?;
        for nested in p["nestedCalls"].as_array().into_iter().flatten() {
            tool(spool, w, v, nested, p["toolCallId"].as_str())?;
        }
    }
    Ok(())
}
fn tool(
    spool: &Spool,
    w: &mut Writer,
    source: &Value,
    p: &Value,
    parent: Option<&str>,
) -> Result<()> {
    let id = p["toolCallId"].as_str();
    if let Some(id) = id {
        if !spool.ambiguous_call(id)? {
            w.entity_identity(format!("tool{:x}", Sha256::digest(id.as_bytes())));
            w.capture_scope = false;
        } else {
            w.entity_identity(format!("tool-pos-{}-{}", w.pos, w.count));
            w.capture_scope = true;
        }
    } else {
        w.entity_identity(format!("tool-pos-{}-{}", w.pos, w.count));
        w.capture_scope = true;
    }

    let mut c = call(w, id, p["toolName"].as_str(), p.get("input"));
    c["requestedBy"] = json!(if parent.is_some() { "code" } else { "model" });
    if let Some(parent) = parent {
        c["parentCallId"] = json!(parent);
    }
    retain(&mut c, w, "nativeToolCall", p);
    emit(w, c, source)?;
    if p["state"] == "output-available" || p["state"] == "output-redacted" {
        let mut r = result(
            spool,
            w,
            id,
            if p["failed"] == true {
                "failed"
            } else {
                "unknown"
            },
        )?;
        if p["state"] == "output-redacted" {
            r["capture"]["content"] = json!({"state":"redacted","reason":"policy"});
        } else {
            // Mux's tool adapter can transform output before sending it to the
            // model. Persist the native raw value without inventing that view.
            r["capture"]["content"] = json!({"state":"absent"});
            if let Some(out) = p.get("output") {
                if unsafe_number(out) {
                    retain(&mut r, w, "rawJson", out);
                } else {
                    r["raw"] = out.clone();
                }
            }
        }
        retain(&mut r, w, "nativeToolResult", p);
        emit(w, r, source)?;
    }
    Ok(())
}
pub(super) fn selected_partial(spool: &Spool, v: &Value) -> Result<bool> {
    let Some(p) = spool.entity("journal:partial")? else {
        return Ok(false);
    };
    Ok(p.pointer("/metadata/historySequence")
        .is_some_and(|seq| !seq.is_null() && Some(seq) == v.pointer("/metadata/historySequence"))
        && p["parts"].as_array().map_or(0, Vec::len) > v["parts"].as_array().map_or(0, Vec::len))
}
pub(super) fn unique_message_id(spool: &mut Spool, id: &str) -> Result<bool> {
    let partial = spool.entity("journal:partial")?;
    let mut count = usize::from(
        spool.entity("journal:partial-effective")? == Some(json!(true))
            && partial
                .as_ref()
                .is_some_and(|p| p["id"].as_str() == Some(id)),
    );
    let mut cursor = 0;
    while let Some(pos) = spool
        .db
        .query_row(
            "SELECT pos FROM rows WHERE native_id=?1 AND pos>?2 ORDER BY pos LIMIT 1",
            ruopencode_source::params![id, cursor],
            |r| r.get::<_, u64>(0),
        )
        .optional()?
    {
        cursor = pos;
        let row = spool.get(pos)?;
        if !selected_partial(spool, &row)? {
            count += 1;
        }
        if count > 1 {
            return Ok(false);
        }
    }
    Ok(count == 1)
}
