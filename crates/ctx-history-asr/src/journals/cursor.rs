use super::common::*;
use crate::{records::Writer, spool::Spool, Error, Result};
use ctx_history_provider_claude_cursor::cursor::{
    parse_transcript_record, transcript_embedded_timestamp, CursorEventBody,
};
use serde_json::{json, Value};
pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(v)?;
    let events = parse_transcript_record(&bytes).map_err(|e| Error::Invalid(e.to_string()))?;
    let Some(events) = events else {
        return vendor(w, v, "unmapped_record");
    };
    let mut text = Vec::new();
    for e in &events {
        if let CursorEventBody::Text { text: t } = &e.body {
            text.push(json!({"type":"text","text":t}));
        }
    }
    if !text.is_empty() {
        let mut m = message(w, v, None, v["role"].as_str(), &Value::Null);
        m["content"] = json!(text);
        m.get_mut("capture")
            .and_then(Value::as_object_mut)
            .map(|o| o.remove("content"));
        if v.get("role").is_none() {
            m["audience"] = json!("user");
        }
        if v.get("timestamp").is_none() {
            if let Some(ts) = events.iter().find_map(transcript_embedded_timestamp) {
                m["ts"] = json!(ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                m["tsSource"] = json!("embedded_text");
                m["tsPrecision"] = json!("min");
            }
        }
        emit(w, m, v)?;
    } else {
        vendor(w, v, "record")?;
    }
    for e in events {
        w.slot(20_000 + u64::from(e.native_order.part_ordinal) * 10);
        match e.body {
            CursorEventBody::ToolCall {
                native_content,
                call_id,
                tool_name,
                arguments,
                protocol,
                server,
                explicit_tool,
                mcp_identity_unavailable,
                ..
            } => {
                let mut c = call(
                    w,
                    call_id.as_deref(),
                    tool_name.as_deref(),
                    arguments.as_ref(),
                );
                c["requestedBy"] = json!("model");
                if protocol.as_deref() == Some("mcp") && !mcp_identity_unavailable {
                    c["provider"] = json!({"kind":"mcp","identity":if server.is_some() && explicit_tool.is_some(){"exact"}else{"unknown"}});
                    if let Some(server) = server {
                        c["provider"]["server"] = json!(server);
                    }
                    if let Some(tool) = explicit_tool {
                        c["provider"]["tool"] = json!(tool);
                    }
                }
                retain(&mut c, w, "nativeToolCall", &native_content);
                emit(w, c, v)?;
            }
            CursorEventBody::ToolOutput {
                native_content,
                call_id,
                content_unavailable,
                ..
            } => {
                let status = match native_content["is_error"].as_bool() {
                    Some(true) => "failed",
                    Some(false) => "completed",
                    None => "unknown",
                };
                let mut r = result(spool, w, call_id.as_deref(), status)?;
                if !content_unavailable && !native_content["content"].is_null() {
                    r["content"] = json!(parts(&native_content["content"]));
                } else {
                    r["capture"]["content"] = json!({"state":"absent"});
                }
                retain(&mut r, w, "nativeToolResult", &native_content);
                emit(w, r, v)?;
            }
            _ => {}
        }
    }
    Ok(())
}
