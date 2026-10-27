use super::{messages, push, sqlite, tree, values};
use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};

pub(super) fn ingest(spool: &mut Spool, db: &Connection, selector: &Value) -> Result<()> {
    let id = selector["nativeSession"].as_str().unwrap_or("");
    let mut header = selector.clone();
    sqlite::rows(
        db,
        "sessions",
        "WHERE id=?2",
        "rowid",
        &[id],
        spool.limit,
        &mut |row| {
            copy(&mut header, "cwd", &row, "working_directory");
            if let Some(t) = values::time(&row["created_at"], false) {
                header["startedAt"] = json!(t);
            }
            header["mainChain"] = row["main_chain_id"].clone();
            spool.save("batch8:sessionState", &row)
        },
    )?;
    spool.save("batch8:header", &header)?;
    let active = if let Some(head) = header["mainChain"].as_i64() {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_nodes WHERE session_id=?1 AND node_id=?2)",
            rusqlite::params![id, head],
            |r| r.get::<_, bool>(0),
        )?
    } else {
        false
    };
    sqlite::rows(
        db,
        "message_nodes",
        "WHERE session_id=?2",
        "node_id",
        &[id],
        spool.limit,
        &mut |mut row| {
            let key = json!(["message_nodes", id, row["node_id"]]);
            if row["_oversized"] == true {
                return super::gap(
                    spool,
                    json!(["message_nodes", id, row["_rowid"]]),
                    &row,
                    "conversion_loss",
                );
            }
            if !active {
                return push(
                    spool,
                    key,
                    json!({"type":"vendor","kind":"inactive_node","native":row}),
                    false,
                );
            }
            let body = match sqlite::json_field(&row, "chat_message") {
                Ok(v) if v.is_object() => v,
                _ => return super::gap(spool, key, &row, "unparseable"),
            };
            let metadata = sqlite::json_field(&row, "metadata").unwrap_or(Value::Null);
            row["chat_message"] = body.clone();
            row["metadata"] = metadata.clone();
            row.as_object_mut().map(|o| o.remove("_rowid"));
            let mut v = json!({"type":"devin","node":row["node_id"].as_i64().map(|n|n.to_string()),
            "parent":row["parent_node_id"].as_i64().map(|n|n.to_string()),"body":body,"native":row,
            "role":body["role"],"calls":messages::calls(crate::Provider::Devin,&body)});
            copy(&mut v, "messageId", &body, "message_id");
            copy(&mut v, "content", &body, "content");
            copy(&mut v, "callId", &body, "tool_call_id");
            if let Some(ts) = body
                .pointer("/metadata/created_at")
                .and_then(Value::as_str)
                .and_then(crate::records::timestamp)
            {
                v["timestamp"] = json!(ts);
            } else if let Some(ts) = values::time(&row["created_at"], false) {
                v["timestamp"] = json!(ts);
                v["tsSource"] = json!("container");
            }
            if body["role"] == "user" && body["metadata"]["is_user_input"] == true {
                v["author"] = json!("human");
            }
            if metadata["is_system_prefix"] == true {
                v["author"] = json!("harness");
                v["origin"] = json!("injected");
            }
            if body["role"] == "assistant" {
                copy(&mut v, "response", &body["metadata"], "request_id");
                copy(&mut v, "inferenceId", &body["metadata"], "request_id");
                copy(&mut v, "usage", &body["metadata"], "metrics");
                copy(&mut v, "stopReason", &body["metadata"], "finish_reason");
                if let Some(t) = body["thinking"].as_object() {
                    let mut r = json!({"type":"reasoning"});
                    if let Some(text) = t.get("thinking").and_then(Value::as_str) {
                        r["text"] = json!(text);
                        r["visibility"] = json!("full");
                    }
                    copy(&mut r, "signature", &body["thinking"], "signature");
                    v["reasoning"] = json!([r]);
                }
            }
            if body["role"] == "tool" {
                let ext = &body["metadata"]["extensions"];
                let timing = &ext["chisel/tool_call_timing"];
                copy(&mut v, "durationMs", timing, "duration_ms");
                copy(&mut v, "completedAt", timing, "finished_at");
                let terminal = &ext["chisel/terminal_output"];
                if terminal.is_object() {
                    let mut exec = json!({});
                    copy(&mut exec, "output", terminal, "text");
                    if let Some(cwd) = terminal["cwd"].as_str() {
                        exec["cwd"] = crate::records::path(cwd, header["cwd"].as_str());
                    }
                    if exec.as_object().is_some_and(|o| !o.is_empty()) {
                        v["exec"] = exec;
                    }
                }
                if let Some(node) = ext["subagent/chain_node_id"].as_i64() {
                    spool.save(&format!("batch8:head:{node}"), &json!(node.to_string()))?;
                }
            }
            if body["role"] == "system" {
                if let Some(previous) = metadata["summarized_from"].as_i64() {
                    v["summaryFrom"] = json!(previous.to_string());
                    // ASR-361 permits a logical parent only for an empty native
                    // parent. Devin's regenerated prefix usually has a parent.
                    // Keep that prefix and the prior forest; do not invent an edge.
                    if row["parent_node_id"].is_null() {
                        v["parent"] = json!(previous.to_string());
                    }
                }
            }
            push(spool, key, v, false)
        },
    )?;
    for table in ["prompt_history", "rendered_commits"] {
        sqlite::rows(
            db,
            table,
            "WHERE session_id=?2",
            "rowid",
            &[id],
            spool.limit,
            &mut |row| {
                push(
                    spool,
                    json!([table, id, row["id"], row["_rowid"]]),
                    json!({"type":"vendor","kind":table,"native":row}),
                    false,
                )
            },
        )?;
    }
    sqlite::rows(
        db,
        "tool_call_state",
        "WHERE session_id=?2",
        "tool_call_id",
        &[id],
        spool.limit,
        &mut |mut row| {
            for field in ["tool_call_json", "tool_call_update_json"] {
                if let Ok(value) = sqlite::json_field(&row, field) {
                    if !value.is_null() {
                        row[field] = value;
                    }
                }
            }
            push(
                spool,
                json!(["tool_call_state", id, row["tool_call_id"], row["_rowid"]]),
                json!({"type":"devinState","kind":"tool_state","native":row}),
                false,
            )
        },
    )?;
    sqlite::rows(
        db,
        "subagent_heads",
        "WHERE session_id=?2",
        "agent_id",
        &[id],
        spool.limit,
        &mut |row| {
            if let Some(node) = row["chain_node_id"].as_i64() {
                spool.save(&format!("batch8:head:{node}"), &json!(node.to_string()))?;
            }
            push(
                spool,
                json!(["subagent_heads", id, row["agent_id"], row["_rowid"]]),
                json!({"type":"vendor","kind":"subagent_head","native":row}),
                false,
            )
        },
    )?;
    if let Some(row) = spool.entity("batch8:sessionState")? {
        push(
            spool,
            json!(["sessions", id]),
            json!({"type":"state","native":row}),
            false,
        )?;
    }
    Ok(())
}
pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    if v["summaryFrom"].is_string() {
        let mut r = json!({"type":"compaction","compactionId":v["node"],"idSource":"derived","kind":"summary",
            "replaces":{"state":"unknown"}});
        let mut content = json!({});
        values::content(spool, &mut content, v.get("content"), writer.provider)?;
        if content["capture"]["content"].is_null() {
            r["summary"] = content["content"].clone();
        } else {
            r["capture"]["summary"] = content["capture"]["content"].clone();
        }
        values::preserve(spool, &mut r, writer.provider, "source", &v["native"])?;
        meta(
            &mut r,
            writer.provider,
            "replacementParent",
            &v["native"]["parent_node_id"],
        );
        tree::emit(
            spool,
            writer,
            r,
            v,
            &json!({"timestamp":v["timestamp"]}),
            true,
        )?;
        return Ok(());
    }
    messages::convert(spool, writer, v)
}
pub(super) fn tool_state(spool: &Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    let row = &v["native"];
    let call = row["tool_call_id"].as_str();
    let unique = if let Some(call) = call {
        !spool.ambiguous_call(call)?
    } else {
        false
    };
    if let Some(call) = call.filter(|_| unique) {
        if let Some(mut r) = spool.entity(&format!("batch8:call:{call}"))? {
            r["supersedes"] = r["_record"].clone();
            r.as_object_mut().map(|o| o.remove("_record"));
            let native = &row["tool_call_json"];
            copy(&mut r, "title", native, "title");
            copy(&mut r, "kind", native, "kind");
            let cwd = spool.entity("batch8:header")?;
            if let Some(locations) = native["locations"].as_array() {
                r["locations"] = json!(locations
                    .iter()
                    .filter_map(|l| {
                        let path = l["path"].as_str()?;
                        let mut location = crate::records::path(
                            path,
                            cwd.as_ref().and_then(|v| v["cwd"].as_str()),
                        );
                        if let Some(line) =
                            l["line"].as_u64().filter(|n| *n <= 9_007_199_254_740_991)
                        {
                            location["line"] = json!(line);
                        }
                        Some(location)
                    })
                    .collect::<Vec<_>>());
            }
            values::preserve(spool, &mut r, writer.provider, "toolState", row)?;
            r["assembly"] = json!({"mode":"snapshot"});
            writer.emit(r, &json!({}))?;
        }
    }
    let previous = match call {
        Some(call) if !spool.ambiguous_call(call)? => {
            spool.entity(&format!("batch8:result:{call}"))?
        }
        _ => None,
    };
    if let Some(mut r) = previous {
        let prior = r["_record"].clone();
        r.as_object_mut().map(|o| o.remove("_record"));
        let update = &row["tool_call_update_json"];
        if let Some(status @ ("completed" | "failed")) = update["status"].as_str() {
            r["status"] = json!(status);
        }
        if let Some(output) = update.get("rawOutput") {
            if values::safe(output) {
                r["raw"] = output.clone();
            }
        }
        if let Some(parts) = update["content"].as_array() {
            let parts: Vec<Value> = parts
                .iter()
                .filter(|p| p["type"] == "content")
                .filter_map(|p| p.get("content").cloned())
                .collect();
            if !parts.is_empty() {
                let mut display = json!({});
                values::content(spool, &mut display, Some(&json!(parts)), writer.provider)?;
                r["displayContent"] = display["content"].clone();
                r["displayCorrespondence"] = json!("native");
            }
        }
        let exit = &update["_meta"]["terminal_exit"];
        if let Some(code) = exit["exit_code"]
            .as_i64()
            .filter(|n| *n >= i32::MIN as i64 && *n <= u32::MAX as i64)
        {
            r["exec"]["exitCode"] = json!(code);
        }
        if let Some(signal) = exit["signal"].as_str() {
            r["exec"]["signal"] = json!(signal);
        }
        if exit["signal"].is_number() {
            // ACP raw output, when present, remains untouched. The full native
            // numeric exit object is retained in the tool-state metadata.
            if r.get("raw").is_none() && values::safe(exit) {
                r["raw"] = json!({"terminal_exit":exit});
            }
        }
        values::preserve(spool, &mut r, writer.provider, "toolState", row)?;
        r["supersedes"] = prior;
        r["assembly"] = json!({"mode":"snapshot"});
        // The timestamp of the earlier result is not the time this snapshot changed.
        for key in ["ts", "tsSource", "tsPrecision"] {
            r.as_object_mut().map(|o| o.remove(key));
        }
        writer.emit(r, &json!({}))?;
    } else {
        let mut r = json!({"type":format!("{}/tool_state",writer.provider.domain())});
        values::preserve(spool, &mut r, writer.provider, "source", row)?;
        writer.emit(r, &json!({}))?;
    }
    diffs(spool, writer, row, call.filter(|_| unique))?;
    Ok(())
}
fn diffs(spool: &Spool, writer: &mut Writer, row: &Value, call: Option<&str>) -> Result<()> {
    let update = &row["tool_call_update_json"];
    let content = update
        .get("content")
        .unwrap_or(&row["tool_call_json"]["content"]);
    let header = spool.entity("batch8:header")?;
    for part in content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["type"] == "diff")
    {
        let (Some(path), Some(after)) = (part["path"].as_str(), part["newText"].as_str()) else {
            continue;
        };
        let mut change =
            crate::records::path(path, header.as_ref().and_then(|v| v["cwd"].as_str()));
        change["op"] = json!(if part["oldText"].is_null() {
            "add"
        } else {
            "modify"
        });
        change["fileType"] = json!("text");
        change["after"] = json!({"text":after});
        if let Some(before) = part["oldText"].as_str() {
            change["before"] = json!({"text":before});
        }
        let mut r = json!({"type":"file_change","status":match update["status"].as_str() {Some("completed")=>"applied",Some("failed")=>"failed",_=>"proposed"},"precision":"exact","changes":[change]});
        if let Some(call) = call {
            r["cause"] = json!({"callId":call});
        }
        values::preserve(spool, &mut r, writer.provider, "diff", part)?;
        writer.emit(r, &json!({}))?;
    }
    Ok(())
}
pub(super) fn finish(spool: &Spool, writer: &mut Writer, header: &Value) -> Result<()> {
    writer.position(writer.pos + 1);
    writer.identity = Some("devin-heads".into());
    writer.capture_scoped = false;
    let primary = header["mainChain"].as_i64().map(|n| n.to_string());
    let (head, known) = tree::resolve(spool, primary.as_deref())?;
    if primary.is_none() || !known || head.is_none() {
        let mut r = json!({"type":"session_info"});
        meta(
            &mut r,
            writer.provider,
            "unresolvedMainChain",
            &header["mainChain"],
        );
        writer.emit(r, &json!({}))?;
        if primary.is_some() {
            writer.emit(json!({"type":"gap","reason":"not_retained"}), &json!({}))?;
        }
        return Ok(());
    }
    let mut heads = Vec::new();
    if let Some(head) = head {
        heads.push(head);
    }
    let mut stmt = spool
        .db
        .prepare("SELECT value FROM entities WHERE key LIKE 'batch8:head:%' ORDER BY key")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let value: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
        if let (Some(head), true) = tree::resolve(spool, value.as_str())? {
            if !heads.contains(&head) {
                heads.push(head);
            }
        }
    }
    if !heads.is_empty() {
        writer.emit(json!({"type":"session_info","heads":heads}), &json!({}))?;
    } else if !known {
        writer.emit(json!({"type":"gap","reason":"not_retained"}), &json!({}))?;
    }
    // A null main chain is metadata-only in Devin, not a claim that the last
    // stored node is active. Preserve it explicitly when ASR cannot name START.
    if primary.is_none() {
        let mut r = json!({"type":"session_info"});
        meta(&mut r, writer.provider, "mainChainAbsent", &json!(true));
        writer.emit(r, &json!({}))?;
    }
    Ok(())
}
