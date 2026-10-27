use super::{messages, push, sqlite, values};
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

pub(super) fn ingest(
    spool: &mut Spool,
    db: &Connection,
    provider: Provider,
    selector: &Value,
) -> Result<()> {
    let id = selector["nativeSession"].as_str().unwrap_or("");
    let child = selector["subagentCall"].as_str();
    let (table, predicate, args) = match (provider, child) {
        (Provider::Firebender, Some(call)) => (
            "subagent_conversations",
            "WHERE session_id=?2 AND tool_call_id=?3",
            vec![id, call],
        ),
        (Provider::Firebender, None) => ("chat_sessions", "WHERE id=?2", vec![id]),
        _ => ("conversations", "WHERE conversation_id=?2", vec![id]),
    };
    sqlite::rows(
        db,
        table,
        predicate,
        "rowid",
        &args,
        spool.limit,
        &mut |mut row| {
            let key = json!([table, selector["id"]]);
            if row["_oversized"] == true {
                return super::gap(spool, key, &row, "conversion_loss");
            }
            let field = if provider == Provider::Firebender {
                "messages_json"
            } else {
                "context"
            };
            let decoded = match sqlite::json_field(&row, field) {
                Ok(v) => v,
                Err(_) => return super::gap(spool, key, &row, "unparseable"),
            };
            let entries = if provider == Provider::Firebender {
                decoded.as_array()
            } else {
                decoded["messages"].as_array()
            };

            if let Some(entries) = entries {
                let mut counts = HashMap::new();
                for entry in entries {
                    if let Some(id) = entry["id"].as_str() {
                        *counts.entry(id).or_insert(0usize) += 1;
                    }
                }
                for (i, entry) in entries.iter().enumerate() {
                    let mut v = if provider == Provider::Firebender {
                        firebender(entry)
                    } else {
                        forgecode(entry)
                    };
                    let native_id = v["messageId"].as_str();
                    let unique = native_id.is_some_and(|id| counts.get(id) == Some(&1));
                    let key = if unique {
                        json!([table, selector["id"], "message", native_id])
                    } else {
                        json!([table, selector["id"], "position", i])
                    };
                    v["native"] = entry.clone();
                    push(spool, key, v, !unique)?;
                }
            } else if !decoded.is_null() {
                super::gap(spool, key.clone(), &row, "unparseable")?;
            }
            let mut h = selector.clone();
            if let Some(ts) = values::time(&row["created_at"], provider == Provider::Firebender) {
                h["startedAt"] = json!(ts);
            }
            if let Some(o) = row.as_object_mut() {
                o.remove(field);
                o.remove("_rowid");
            }
            if provider == Provider::ForgeCode {
                let mut context = decoded;
                context.as_object_mut().map(|o| o.remove("messages"));
                row["context"] = context;
                if let Ok(metrics) = sqlite::json_field(&row, "metrics") {
                    if !metrics.is_null() {
                        row["metrics"] = metrics;
                    }
                }
            } else if let Ok(metadata) = sqlite::json_field(&row, "metadata_json") {
                if !metadata.is_null() {
                    row["metadata_json"] = metadata;
                }
            }
            spool.save("batch8:header", &h)?;
            push(
                spool,
                json!([table, selector["id"], "state"]),
                json!({"type":"state","native":row}),
                false,
            )
        },
    )?;
    if provider == Provider::Firebender {
        // The native key identifies a call-owned conversation, not a session.
        // Preserve it under its explicit owner without manufacturing a session ID.
        sqlite::rows(
            db,
            "subagent_conversations",
            "WHERE session_id=?2",
            "tool_call_id",
            &[id],
            spool.limit,
            &mut |mut row| {
                let key = json!([
                    "subagent_conversations",
                    id,
                    row["tool_call_id"],
                    row["_rowid"]
                ]);
                if row["_oversized"] == true {
                    return super::gap(spool, key, &row, "conversion_loss");
                }
                if let Ok(messages) = sqlite::json_field(&row, "messages_json") {
                    if !messages.is_null() {
                        row["messages_json"] = messages;
                    }
                }
                push(
                    spool,
                    key,
                    json!({"type":"vendor","kind":"subagent_conversation","native":row}),
                    false,
                )
            },
        )?;
    }
    Ok(())
}
fn firebender(entry: &Value) -> Value {
    let mut v = json!({"type":"message","body":entry,"role":entry["role"],"calls":messages::calls(Provider::Firebender,entry)});
    for (to, from) in [
        ("messageId", "id"),
        ("content", "content"),
        ("callId", "tool_call_id"),
    ] {
        copy(&mut v, to, entry, from);
    }
    if let Some(ts) = entry.get("timestamp").and_then(|t| values::time(t, true)) {
        v["timestamp"] = json!(ts);
    }
    if entry["role"] == "tool" {
        if let Some(error) = entry["is_error"].as_bool() {
            v["status"] = json!(if error { "failed" } else { "completed" });
        }
    }
    v
}
fn forgecode(entry: &Value) -> Value {
    // Older persistence wraps `message`; current MessageEntry flattens the enum.
    let message = entry.get("message").unwrap_or(entry);
    let mut v = json!({"type":"message"});
    if let Some(body) = message.get("text") {
        v["body"] = body.clone();
        if let Some(role) = body["role"].as_str() {
            v["role"] = json!(match role {
                "User" => "user",
                "Assistant" => "assistant",
                "System" => "system",
                other => other,
            });
        }
        copy(&mut v, "content", body, "content");
        v["calls"] = json!(messages::calls(Provider::ForgeCode, body));
        if let Some(phase) = body["phase"].as_str() {
            if matches!(phase, "commentary" | "final_answer") {
                v["phase"] = json!(if phase == "commentary" {
                    "commentary"
                } else {
                    "final"
                });
            }
        }
        let mut reasoning = Vec::new();
        for detail in body["reasoning_details"].as_array().into_iter().flatten() {
            let mut part = json!({"type":"reasoning"});
            if detail["type_of"] == "reasoning.summary" {
                if let Some(text) = detail["text"].as_str() {
                    part["visibility"] = json!("summary");
                    part["summary"] = json!([text]);
                }
            } else if detail["type_of"] == "reasoning.encrypted" {
                part["visibility"] = json!("encrypted");
                if let Some(data) = detail["data"].as_str() {
                    part["opaque"] = json!({"data":data});
                    copy(&mut part["opaque"], "format", detail, "format");
                }
            } else {
                copy(&mut part, "text", detail, "text");
            }
            copy(&mut part, "signature", detail, "signature");
            reasoning.push(part);
        }
        if let Some(signature) = body["thought_signature"].as_str() {
            reasoning.push(json!({"type":"reasoning","signature":signature}));
        }
        if !reasoning.is_empty() {
            v["reasoning"] = json!(reasoning);
        }
        if v["role"] == "assistant" {
            copy(&mut v, "usage", entry, "usage");
            copy(&mut v, "model", body, "model");
        }
    } else if let Some(body) = message.get("tool") {
        v["role"] = json!("tool");
        v["body"] = body.clone();
        copy(&mut v, "callId", body, "call_id");
        copy(&mut v, "raw", body, "output");
        if let Some(error) = body["output"]["is_error"].as_bool() {
            v["status"] = json!(if error { "failed" } else { "completed" });
        }
        if let Some(items) = body["output"]["values"].as_array() {
            v["content"] = json!(items
                .iter()
                .map(|item| {
                    if let Some(text) = item["text"].as_str() {
                        json!({"type":"text","text":text})
                    } else if let Some(text) = item["ai"]["value"].as_str() {
                        json!({"type":"text","text":text})
                    } else if let Some(image) = item.get("image") {
                        json!({"type":"image_url","image_url":image["url"]})
                    } else {
                        json!({"type":"data","data":item})
                    }
                })
                .collect::<Vec<_>>());
        }
    } else if let Some(image) = message.get("image") {
        v["role"] = json!("user");
        v["content"] = json!([{"type":"image_url","image_url":image["url"]}]);
    } else {
        v["type"] = json!("vendor");
        v["kind"] = json!("context_entry");
    }
    v
}

pub(super) fn state(spool: &Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    let row = &v["native"];
    let mut r = json!({"type":"session_info"});
    if let Some(title) = row
        .get("title")
        .or_else(|| row.get("name"))
        .filter(|v| v.is_string())
    {
        r["title"] = title.clone();
    }
    values::preserve(spool, &mut r, writer.provider, "state", row)?;
    writer.emit(r, &json!({}))?;
    if writer.provider == Provider::Devin {
        let mut config = json!({"type":"config_change","snapshot":true});
        if let Some(model) = row["model"].as_str() {
            config["model"] = json!({"id":model});
        }
        copy(&mut config, "mode", row, "agent_mode");
        if config.as_object().is_some_and(|o| o.len() > 2) {
            writer.emit(config, &json!({}))?;
        }
    }
    if let Some(tools) = row
        .pointer("/context/tools")
        .filter(|v| values::safe(v))
        .and_then(Value::as_array)
    {
        let definitions = tools
            .iter()
            .filter_map(|t| {
                let name = t["name"].as_str()?;
                let mut d = json!({"name":name});
                copy(&mut d, "description", t, "description");
                copy(&mut d, "inputSchema", t, "input_schema");
                Some(d)
            })
            .collect::<Vec<_>>();
        writer.emit(
            json!({"type":"instructions","kind":"tool_definitions","tools":definitions}),
            &json!({}),
        )?;
    }
    Ok(())
}
