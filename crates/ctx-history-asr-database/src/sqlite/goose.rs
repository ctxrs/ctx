mod interaction;
mod usage;
use super::common::*;
use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
pub(super) use interaction::redact;
use serde_json::{json, Value};
const P: Provider = Provider::Goose;
pub(super) fn index_calls(spool: &Spool, row: &Value) -> Result<()> {
    for part in parsed(row, "content_json")
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if row["_table"] == "messages"
            && matches!(
                part["type"].as_str(),
                Some("toolRequest" | "frontendToolRequest")
            )
        {
            if let Some(id) = part["id"].as_str() {
                index_call(spool, id)?;
            }
        }
        interaction::index(spool, part)?;
    }
    Ok(())
}
pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, source: &Value) -> Result<()> {
    if source["_table"] == "sessions" {
        return session(spool, writer, source);
    }
    if source["_table"] == "usage_ledger" {
        return ledger(spool, writer, source);
    }
    let mut row = source.clone();
    interaction::redact(spool, &mut row)?;
    if row["_table"] == "thread_messages" {
        let mut out = json!({"type":"xyz.block.goose/thread_message"});
        row_meta(spool, &mut out, P, &row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    }
    let Some(parts) = parsed(&row, "content_json").filter(Value::is_array) else {
        let mut out = json!({"type":"gap","reason":"unparseable","count":1});
        row_meta(spool, &mut out, P, &row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    };
    let metadata = parsed(&row, "metadata_json").unwrap_or(Value::Null);
    let mut content = Vec::new();
    for part in parts.as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("text") if part["text"].is_string() => {let mut p=json!({"type":"text","text":part["text"]});part_meta(spool, &mut p,P,part)?;content.push(p);}
            Some("thinking") => {let mut p=json!({"type":"reasoning","visibility":"full"});copy(&mut p,"text",part,"thinking");copy(&mut p,"signature",part,"signature");part_meta(spool, &mut p,P,part)?;content.push(p);}
            Some("redactedThinking") => content.push(json!({"type":"reasoning","visibility":"redacted","opaque":{"format":"goose.redactedThinking","data":part["data"]}})),
            Some("image") => content.push(binary(spool,part["mimeType"].as_str().unwrap_or("application/octet-stream"),part["data"].as_str().unwrap_or_default())?),
            Some("toolRequest"|"frontendToolRequest"|"toolResponse"|"toolConfirmationRequest"|"actionRequired") => {},
            Some("systemNotification"|"error") => {},
            _ => content.push(opaque(spool,part,P)?),
        }
    }
    let id = row["message_id"].as_str().filter(|s| !s.is_empty());
    let mut m = message(spool, &row, P, id, row["role"].as_str(), content)?;
    time(&mut m, &row, "created_timestamp", false);
    match (
        metadata["agentVisible"].as_bool(),
        metadata["userVisible"].as_bool(),
    ) {
        (Some(true), Some(true)) => m["audience"] = json!("both"),
        (Some(true), Some(false)) => m["audience"] = json!("model"),
        (Some(false), Some(true)) => m["audience"] = json!("user"),
        _ => {}
    }
    if metadata["steer"] == true {
        m["origin"] = json!("steer");
    }
    if metadata["turnContext"] == true {
        m["origin"] = json!("injected");
        m["author"] = json!("harness");
    }
    // agentOnly also marks reminders and copied prompts; it is not a summary discriminator.
    if metadata.pointer("/usage/isCompaction") == Some(&Value::Bool(true)) {
        m = json!({"type":"compaction","compactionId":id.unwrap_or(row["_key"].as_str().unwrap_or_default()),"kind":"summary","summary":m["content"],"replaces":{"state":"unknown"}});
        row_meta(spool, &mut m, P, &row)?;
    }
    let inference = usage::message(spool, &row, &metadata)?;
    if let Some(inference) = &inference {
        m["inferenceId"] = inference["inferenceId"].clone();
    }
    writer.emit(m, &Value::Null)?;
    if let Some(inference) = inference {
        writer.slot(1);
        writer.emit(inference, &Value::Null)?;
    }
    for (i, part) in parts.as_array().into_iter().flatten().enumerate() {
        writer.slot(100 + i as u64 * 4);
        let mut out = match part["type"].as_str() {
            Some("toolRequest" | "frontendToolRequest") => {
                let value = &part["toolCall"]["value"];
                let mut out = call(
                    spool,
                    writer,
                    &row,
                    part,
                    part["id"].as_str(),
                    value["name"].as_str(),
                    value.get("arguments"),
                )?;
                if part["type"] == "frontendToolRequest" {
                    out.as_object_mut().map(|o| o.remove("requestedBy"));
                }
                out
            }
            Some("toolResponse") => {
                let result = &part["toolResult"];
                let value = &result["value"];
                let mut out = json!({"type":"tool_result","status":if result["status"]=="error" || value["isError"]==true {"failed"} else if result["status"]=="success" {"completed"} else {"unknown"}});
                link(spool, &mut out, part["id"].as_str(), P)?;
                result_identity(
                    writer,
                    &mut out,
                    &parts,
                    "toolResponse",
                    "/id",
                    part["id"].as_str(),
                );
                if result["status"] == "error" {
                    copy(&mut out, "error", result, "error");
                    if let Some(text) = result["error"].as_str() {
                        out["error"] = json!({"message":text});
                        out["content"] = json!([{"type":"text","text":text}]);
                    }
                } else if let Some(parts) = value["content"].as_array() {
                    let mut converted = Vec::new();
                    for p in parts {
                        converted.push(mcp_part(spool, p)?);
                    }
                    out["content"] = json!(converted);
                } else {
                    out["capture"] = json!({"content":{"state":"absent"}});
                }
                if !value.is_null() {
                    let text = value.to_string();
                    if safe(value) && text.len() <= 65536 {
                        out["raw"] = value.clone();
                    } else {
                        if !out["capture"].is_object() {
                            out["capture"] = json!({});
                        }
                        out["capture"]["raw"] = json!({"state":"external","blob":blob(spool,text.as_bytes(),"application/json")?});
                    }
                }
                part_meta(spool, &mut out, P, part)?;
                out
            }
            Some("toolConfirmationRequest" | "actionRequired") => {
                interaction::emit(spool, writer, part)?;
                continue;
            }
            Some("systemNotification" | "error") => {
                let text = part
                    .get("msg")
                    .or_else(|| part.get("message"))
                    .and_then(Value::as_str);
                let mut out = json!({"type":"message","messageId":writer.id(),"idSource":"derived","author":"harness","origin":"notice","audience":"user","content":[],"capture":{"role":{"state":"absent"},"content":{"state":"absent"}}});
                if let Some(text) = text {
                    out["displayContent"] = json!([{"type":"text","text":text}]);
                }
                part_meta(spool, &mut out, P, part)?;
                out
            }
            _ => continue,
        };
        meta(&mut out, P, "messageId", &row["message_id"]);
        writer.emit(out, &Value::Null)?;
    }
    Ok(())
}
fn mcp_part(spool: &Spool, p: &Value) -> Result<Value> {
    let mut out = match p["type"].as_str() {
        Some("text") if p["text"].is_string() => json!({"type":"text","text":p["text"]}),
        Some("image" | "audio") => binary(
            spool,
            p["mimeType"].as_str().unwrap_or("application/octet-stream"),
            p["data"].as_str().unwrap_or_default(),
        )?,
        Some("resource" | "resource_link") if safe(p) => {
            let mut v = p.clone();
            v.as_object_mut().map(|o| o.remove("_meta"));
            v
        }
        _ => opaque(spool, p, P)?,
    };
    part_meta(spool, &mut out, P, p)?;
    Ok(out)
}
fn ledger(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    let mut out = json!({"type":"inference","inferenceId":format!("usage_ledger:{}",row["id"]),"idSource":"derived","usage":usage(row,"input_tokens","output_tokens","total_tokens")});
    for (native, key) in [
        ("cache_read_tokens", "cacheRead"),
        ("cache_write_tokens", "cacheWrite"),
    ] {
        if let Some(n) = row[native].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
            if !out["usage"]["input"].is_object() {
                out["usage"]["input"] = json!({});
            }
            out["usage"]["input"][key] = json!(n);
        }
    }
    if row["model"].is_string() {
        out["model"] = json!({"id":row["model"]});
    }
    if row["is_compaction"] == 1 {
        out["purpose"] = json!("compaction");
    }
    if row["cost"].as_f64().is_some_and(|n| n >= 0.0) && safe(&row["cost"]) {
        out["cost"] = json!({"amount":row["cost"],"currency":"USD"});
        match row["cost_source"].as_str() {
            Some("provider_reported") => out["cost"]["source"] = json!("provider_reported"),
            Some("estimated") => out["cost"]["source"] = json!("harness_estimated"),
            _ => {}
        }
    }
    time(&mut out, row, "created_timestamp", false);
    row_meta(spool, &mut out, P, row)?;
    writer.emit(out, &Value::Null)?;
    Ok(())
}
fn session(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    session_tail(spool, writer, row)?;
    for (prefix, cumulative, counter) in [
        ("", false, "current-context"),
        ("accumulated_", true, "accumulated"),
    ] {
        let u = usage(
            row,
            &format!("{prefix}input_tokens"),
            &format!("{prefix}output_tokens"),
            &format!("{prefix}total_tokens"),
        );
        if u.as_object().is_some_and(|o| !o.is_empty()) {
            let mut out = json!({"type":"usage_summary","scope":"session","cumulative":cumulative,"includesChildren":null,"counter":counter,"usage":u});
            if cumulative
                && row["accumulated_cost"].as_f64().is_some_and(|n| n >= 0.0)
                && safe(&row["accumulated_cost"])
            {
                out["cost"] = json!({"amount":row["accumulated_cost"],"currency":"USD"});
            }
            writer.emit(out, &Value::Null)?;
        }
    }
    Ok(())
}
