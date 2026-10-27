use super::common::*;
use crate::{
    records::{copy, meta, path, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
const P: Provider = Provider::Crush;

pub(super) fn index_calls(spool: &Spool, row: &Value) -> Result<()> {
    for p in parsed(row, "parts")
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if p["type"] == "tool_call" {
            if let Some(id) = p["data"]["id"].as_str() {
                index_call(spool, id)?;
            }
        }
    }
    Ok(())
}
pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    match row["_table"].as_str() {
        Some("sessions") => return session(spool, writer, row),
        Some("files") => return file(spool, writer, row),
        Some("read_files") => {
            let mut out = json!({"type":"sh.charm.crush/read_file_state"});
            row_meta(spool, &mut out, P, row)?;
            time(&mut out, row, "read_at", false);
            writer.emit(out, &Value::Null)?;
            return Ok(());
        }
        _ => {}
    }
    let Some(parts) = parsed(row, "parts").filter(Value::is_array) else {
        let mut gap = json!({"type":"gap","reason":"unparseable","count":1});
        row_meta(spool, &mut gap, P, row)?;
        writer.emit(gap, &Value::Null)?;
        return Ok(());
    };
    let mut content = Vec::new();
    let mut finish = None;
    let mut tool_parts = Vec::new();
    for (i, part) in parts.as_array().into_iter().flatten().enumerate() {
        let data = &part["data"];
        match part["type"].as_str() {
            Some("text") if data["text"].is_string() => {
                content.push(json!({"type":"text","text":data["text"]}))
            }
            Some("reasoning") => {
                let mut p = json!({"type":"reasoning","visibility":"full"});
                copy(&mut p, "text", data, "thinking");
                copy(&mut p, "signature", data, "signature");
                part_meta(spool, &mut p, P, part)?;
                content.push(p);
            }
            Some("image_url") if data["url"].is_string() => {
                let mut p = json!({"type":"resource_link","uri":data["url"],"name":"image"});
                part_meta(spool, &mut p, P, part)?;
                content.push(p);
            }
            Some("binary") => content.push(binary(
                spool,
                data["MIMEType"]
                    .as_str()
                    .unwrap_or("application/octet-stream"),
                data["Data"].as_str().unwrap_or_default(),
            )?),
            Some("finish") => finish = Some(data),
            Some("tool_call" | "tool_result" | "shell_command") => tool_parts.push((i, part)),
            _ => content.push(opaque(spool, part, P)?),
        }
    }
    let mut out = if row["is_summary_message"] == 1 || row["is_summary_message"] == true {
        json!({"type":"compaction","compactionId":row["id"],"kind":"summary","summary":content,"replaces":{"state":"unknown"}})
    } else {
        message(
            spool,
            row,
            P,
            row["id"].as_str(),
            row["role"].as_str(),
            content,
        )?
    };
    row_meta(spool, &mut out, P, row)?;
    time(&mut out, row, "created_at", true);
    if let Some(f) = finish {
        if out["type"] == "message" {
            let status = match f["reason"].as_str() {
                Some("end_turn" | "stop" | "tool_use") => Some("complete"),
                Some("max_tokens" | "canceled") => Some("interrupted"),
                Some("error") => Some("error"),
                _ => None,
            };
            if let Some(status) = status {
                out["status"] = json!(status);
            }
        }
    }
    writer.emit(out, &Value::Null)?;
    for (i, part) in tool_parts {
        let data = &part["data"];
        writer.slot(100 + (i as u64) * 4);
        let mut out = match part["type"].as_str() {
            Some("tool_call") => call(
                spool,
                writer,
                row,
                part,
                data["id"].as_str(),
                data["name"].as_str(),
                data.get("input"),
            )?,
            Some("shell_command") => {
                let id = writer.id();
                let c = json!({"type":"tool_call","callId":id,"callIdSource":"derived","name":"shell_command","kind":"execute","requestedBy":"user","input":{"command":data["command"]}});
                writer.emit(c, &Value::Null)?;
                let mut r = json!({"type":"tool_result","callId":id,"linkage":"derived","status":"unknown","capture":{"content":{"state":"absent"}},"exec":{"command":data["command"],"output":data["output"]}});
                if let Some(n) = data["exit_code"]
                    .as_i64()
                    .filter(|n| (-2147483648..=4294967295).contains(n))
                {
                    r["exec"]["exitCode"] = json!(n);
                }
                r
            }
            _ => {
                let mut r = json!({"type":"tool_result","status":match data["is_error"].as_bool(){Some(true)=>"failed",Some(false)=>"completed",None=>"unknown"}});
                r["raw"] = data.clone();
                link(spool, &mut r, data["tool_call_id"].as_str(), P)?;
                result_identity(
                    writer,
                    &mut r,
                    &parts,
                    "tool_result",
                    "/data/tool_call_id",
                    data["tool_call_id"].as_str(),
                );
                if let Some(media) = data["data"].as_str().filter(|s| !s.is_empty()) {
                    r["content"] = json!([binary(
                        spool,
                        data["mime_type"]
                            .as_str()
                            .unwrap_or("application/octet-stream"),
                        media
                    )?]);
                } else if let Some(text) = data["content"].as_str() {
                    r["content"] = json!([{"type":"text","text":text}]);
                } else {
                    r["capture"] = json!({"content":{"state":"absent"}});
                }
                r
            }
        };
        part_meta(spool, &mut out, P, part)?;
        meta(&mut out, P, "messageId", &row["id"]);
        // A message row timestamp is not a timestamp for each embedded tool operation.
        writer.emit(out, &Value::Null)?;
    }
    Ok(())
}
fn file(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    let (Some(name), Some(id)) = (row["path"].as_str(), row["id"].as_str()) else {
        let mut out = json!({"type":"gap","reason":"unparseable","count":1});
        row_meta(spool, &mut out, P, row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    };
    let Some(content) = row["content"].as_str() else {
        let mut out =
            json!({"type":"checkpoint","kind":"snapshot","ref":id,"files":[{"path":name}]});
        if let Some(version) = row["version"].as_i64() {
            out["files"][0]["version"] = json!(version.to_string());
        }
        checkpoint_path(&mut out, name);
        row_meta(spool, &mut out, P, row)?;
        writer.emit(out, &Value::Null)?;
        return Ok(());
    };
    let version = row["version"].as_i64();
    let bytes = blob(spool, content.as_bytes(), "text/plain")?;
    let mut checkpoint = json!({"type":"checkpoint","kind":"snapshot","ref":id,"files":[{"path":name,"blob":bytes}]});
    if let Some(version) = version {
        checkpoint["files"][0]["version"] = json!(version.to_string());
    }
    checkpoint_path(&mut checkpoint, name);
    row_meta(spool, &mut checkpoint, P, row)?;
    time(&mut checkpoint, row, "created_at", true);
    let current = writer.emit(checkpoint, &Value::Null)?;
    let key = format!("file:{name}");
    if let Some(previous) = spool.entity(&key)? {
        if version.and_then(|n| n.checked_sub(1)) == previous["version"].as_i64() {
            let mut change = path(name, None);
            change["op"] = json!("modify");
            change["before"] = json!({"content":previous["blob"]});
            change["after"] = json!({"content":bytes});
            let mut out = json!({"type":"file_change","status":"unknown","precision":"exact","basis":"net","endpoints":{"from":previous["record"],"to":current},"changes":[change]});
            // Version storage includes externally changed intermediate contents: no actor/call attribution.
            row_meta(spool, &mut out, P, row)?;
            writer.emit(out, &Value::Null)?;
        }
    }
    spool.save(
        &key,
        &json!({"version":version,"blob":bytes,"record":current}),
    )?;
    Ok(())
}
fn session(spool: &Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    session_tail(spool, writer, row)?;
    // These columns start at zero before any provider reports usage.
    if ["prompt_tokens", "completion_tokens", "total_tokens", "cost"]
        .iter()
        .all(|key| row[key].is_null() || row[key].as_f64() == Some(0.0))
    {
        return Ok(());
    }
    let usage = usage(row, "prompt_tokens", "completion_tokens", "total_tokens");
    if usage.as_object().is_some_and(|o| !o.is_empty()) || row["cost"].is_number() {
        let mut out = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":"session","usage":usage});
        if row["cost"].as_f64().is_some_and(|n| n >= 0.0) && safe(&row["cost"]) {
            out["cost"] =
                json!({"amount":row["cost"],"currency":"USD","source":"harness_estimated"});
        }
        writer.emit(out, &Value::Null)?;
    }
    if let Some(todos) = parsed(row, "todos").filter(Value::is_array) {
        let entries: Vec<Value> = todos
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let text = v["content"].as_str()?;
                let status = match v["status"].as_str() {
                    Some("pending") => "pending",
                    Some("in_progress") => "in_progress",
                    Some("completed") => "completed",
                    _ => return None,
                };
                Some(json!({"content":text,"status":status}))
            })
            .collect();
        if !entries.is_empty() {
            writer.emit(json!({"type":"plan","planId":"session-todos","idSource":"derived","planType":"items","entries":entries}),&Value::Null)?;
        }
    }
    Ok(())
}

// Checkpoint.files has only root-relative paths. Preserve a rootless absolute
// path as a structured vendor fact rather than inventing a workspace root.
fn checkpoint_path(out: &mut Value, name: &str) {
    let structured = path(name, None);
    let relative = structured["root"] == "unknown"
        && structured["path"]
            .as_str()
            .is_some_and(|p| !p.split('/').any(|s| s == ".."));
    if relative {
        out["root"] = json!("unknown");
        out["files"][0]["path"] = structured["path"].clone();
    } else {
        let mut file = out["files"][0].clone();
        file["path"] = structured;
        meta(out, P, "file", &file);
        out.as_object_mut().map(|o| o.remove("files"));
    }
}
