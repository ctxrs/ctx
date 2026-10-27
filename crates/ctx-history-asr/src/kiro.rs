//! Kiro conversations are whole-document snapshots, including v2 UUID sessions.
//! History indexes identify positions in this capture, not durable messages.
use crate::{
    content,
    opencode_source::millis,
    records::{meta, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

pub(crate) fn convert(s: &mut Spool, w: &mut Writer, n: &Value) -> Result<()> {
    match n["type"].as_str().unwrap_or("") {
        "_gap" => {
            w.emit(
                json!({"type":"gap","reason":n["reason"],"count":1}),
                &json!({}),
            )?;
        }
        "kiro_state" => {
            // Keep unassociated display history and provider continuation state;
            // neither supplies model-call boundaries or copy/lineage evidence.
            w.vendor(&n["data"], "conversation_state")?;
            let d = &n["data"];
            if let Some(summary) = d["latest_summary"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
            {
                w.slot(10);
                w.emit(json!({"type":"compaction","compactionId":"latest-summary","idSource":"derived","kind":"summary","summary":[{"type":"text","text":summary}],"replaces":{"state":"unknown"}}),&json!({}))?;
            }
        }
        "kiro_entry" => entry(s, w, n)?,
        _ => {
            w.vendor(n, "native")?;
        }
    }
    Ok(())
}
fn entry(s: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let d = &n["data"];
    let index = n["row"]["history_index"].as_u64().unwrap_or(0);
    let user = &d["user"];
    let c = &user["content"];
    let prompt = c
        .pointer("/Prompt/prompt")
        .or_else(|| c.pointer("/CancelledToolUses/prompt"));
    if let Some(prompt) = prompt.and_then(Value::as_str) {
        let mut parts =
            vec![json!({"type":"text","text":prompt,"author":"human","origin":"prompt"})];
        if let Some(context) = user["additional_context"].as_str() {
            parts
                .push(json!({"type":"text","text":context,"author":"harness","origin":"injected"}));
        }
        let mut r = json!({"type":"message","messageId":format!("history:{index}:user"),"idSource":"derived","role":"user","content":parts});
        meta(&mut r, w.provider, "user", user);
        truncated(&mut r, prompt);
        w.emit(r, user)?;
    }
    for (i, result) in c
        .pointer("/ToolUseResults/tool_use_results")
        .or_else(|| c.pointer("/CancelledToolUses/tool_use_results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        w.slot(100 + (i as u64) * 10);
        let id = result["tool_use_id"].as_str();
        let status = match result["status"].as_str() {
            Some("Success" | "success") => "completed",
            Some("Error" | "error") => "failed",
            _ => "unknown",
        };
        let mut r = json!({"type":"tool_result","linkage":"none","status":status});
        if let Some(id) = id {
            r["callId"] = json!(id);
            r["linkage"] = json!(if s.ambiguous_call(id)? {
                "ambiguous"
            } else {
                "exact"
            });
        }
        if let Some(blocks) = result["content"].as_array() {
            let mut parts = Vec::new();
            for b in blocks {
                if let Some(t) = b["Text"].as_str() {
                    parts.push(json!({"type":"text","text":t}));
                    truncated(&mut r, t);
                } else if let Some(v) = b.get("Json") {
                    parts.push(json!({"type":"data","data":v}));
                } else {
                    parts.push(json!({"type":"data","data":b}));
                }
            }
            r["content"] = json!(parts);
        } else {
            r["capture"]["content"] = json!({"state":"absent"});
        }
        meta(&mut r, w.provider, "toolResult", result);
        meta(&mut r, w.provider, "user", user);
        w.emit(r, &json!({}))?;
    }
    let assistant = d["assistant"]
        .get("Response")
        .or_else(|| d["assistant"].get("ToolUse"));
    if let Some(a) = assistant {
        w.slot(200_000);
        let native = a["message_id"].as_str();
        let mut r = json!({"type":"message","messageId":native.map(str::to_owned).unwrap_or_else(||format!("history:{index}:assistant")),"role":"assistant","author":"model","content":content::parts(&a["content"],w.provider)});
        if native.is_none() {
            r["idSource"] = json!("derived");
        }
        if a.get("content").is_none() {
            r["capture"]["content"] = json!({"state":"absent"});
        }
        meta(&mut r, w.provider, "assistant", &d["assistant"]);
        w.emit(r, &d["assistant"])?;
        for (i, t) in a
            .get("tool_uses")
            .or_else(|| a.get("toolUses"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            w.slot(300_000 + (i as u64) * 10);
            let Some(id) = t["id"].as_str() else {
                w.vendor(t, "unidentified_tool")?;
                continue;
            };
            let mut r = crate::tools::call(
                w,
                s,
                t,
                id,
                t["name"].as_str(),
                crate::tools::Arguments::Json(t.get("args").cloned().unwrap_or(Value::Null)),
                "model",
            )?;
            meta(&mut r, w.provider, "toolUse", t);
            w.emit(r, &json!({}))?;
        }
    }
    if let Some(req) = d.get("request_metadata").filter(|v| v.is_object()) {
        w.slot(900_000);
        let native = req["request_id"].as_str();
        let mut r = json!({"type":"inference","inferenceId":native.map(str::to_owned).unwrap_or_else(||format!("history:{index}:request"))});
        if native.is_none() {
            r["idSource"] = json!("derived");
        }
        if let Some(m) = req["model_id"].as_str() {
            r["model"] = json!({"requested":m});
        }
        if let Some(start) = millis(&req["request_start_timestamp_ms"]) {
            r["ts"] = json!(start);
        }
        if let (Some(start), Some(end)) = (
            req["request_start_timestamp_ms"].as_u64(),
            req["stream_end_timestamp_ms"].as_u64(),
        ) {
            if let Some(duration) = end.checked_sub(start) {
                r["latency"]["durationMs"] = json!(duration);
            }
        }
        meta(&mut r, w.provider, "requestMetadata", req);
        w.emit(r, &json!({}))?;
    }
    // Unknown entry fields, including unrecognized variants, cannot disappear.
    w.slot(990_000);
    let mut rest = d.clone();
    if let Some(o) = rest.as_object_mut() {
        if prompt.and_then(Value::as_str).is_some()
            || c.pointer("/ToolUseResults/tool_use_results")
                .or_else(|| c.pointer("/CancelledToolUses/tool_use_results"))
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
        {
            o.remove("user");
        }
        if assistant.is_some() {
            o.remove("assistant");
        }
        if d.get("request_metadata").is_some_and(Value::is_object) {
            o.remove("request_metadata");
        }
    }
    if rest.as_object().is_none_or(|o| !o.is_empty()) {
        w.vendor(&rest, "history_extra")?;
    }
    Ok(())
}
fn truncated(r: &mut Value, text: &str) {
    if text.ends_with("...content truncated due to length") {
        r["capture"]["content"] = json!({"state":"truncated","reason":"source_truncated"});
    }
}
