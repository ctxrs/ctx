//! Shelley SQLite at boldsoftware/shelley@b66b0cf61b80b11df7e6754a628ad8241fe2b6f6.
use super::{common as c, reference, sqlite};
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
const P: Provider = Provider::Shelley;
pub(super) fn ingest(db: &Connection, s: &mut Spool, h: &mut Value) -> Result<()> {
    let id = h["_sessionId"].as_str().unwrap_or("").to_owned();
    if sqlite::columns(db, "messages")?.contains("forked_from_message_id") {
        let owners = db.prepare("SELECT DISTINCT original.conversation_id FROM messages copied JOIN messages original ON copied.forked_from_message_id=original.message_id WHERE copied.conversation_id=?1 LIMIT 2")?
            .query_map([&id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        // At the pinned source, only CopyMessagesForFork sets this column.
        if owners.len() == 1 {
            h["_forkOwner"] = json!(owners[0]);
        }
    }
    sqlite::visit(
        db,
        "messages",
        Some(("conversation_id", &id)),
        "sequence_id, message_id",
        &[],
        s.limit,
        &mut |mut n| {
            let body = c::decoded(&n["llm_data"]);
            let calls: Vec<Value> = body["Content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| kind(b) == Some(5))
                .map(|b| json!({"id":b["ID"]}))
                .collect();
            c::index_calls(s, &json!(calls))?;
            for b in body["Content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| kind(b) == Some(6))
            {
                c::index_result(s, b["ToolUseID"].as_str())?;
            }
            if let Some(original) = n["forked_from_message_id"].as_str() {
                let owner: Option<String> = db
                    .query_row(
                        "SELECT conversation_id FROM messages WHERE message_id=?1",
                        [original],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(owner) = owner {
                    n["_copySession"] = json!(owner);
                }
            }
            let key = format!("message:{}", n["message_id"].as_str().unwrap_or(""));
            s.push_relational("message", &key, &n, false)
        },
    )?;
    s.push_relational("session", "session", h, false)
}
pub(super) fn header(h: &Value, v: &mut Value) {
    v["profiles"].as_array_mut().unwrap().push(json!("coding"));
    if let Some(t) = c::time(&h["created_at"]) {
        v["startedAt"] = json!(t);
    }
    if let Some(parent) = h["parent_conversation_id"].as_str() {
        v["lineage"] =
            json!({"spawnedBy":{"session":reference(P,h,parent),"evidence":"native_field"}});
        v["scope"] = json!("subagent");
    }
    if let Some(parent) = h["_forkOwner"].as_str() {
        v["lineage"]["forkedFrom"] =
            json!({"session":reference(P,h,parent),"evidence":"native_structure"});
    }
    v["producer"]["lossy"]
        .as_array_mut()
        .unwrap()
        .extend([json!("branches"), json!("copies")]);
}
fn kind(n: &Value) -> Option<u64> {
    n["Type"].as_u64().or_else(|| match n["Type"].as_str()? {
        "text" | "ContentTypeText" => Some(2),
        "thinking" | "ContentTypeThinking" => Some(3),
        "redacted_thinking" | "ContentTypeRedactedThinking" => Some(4),
        "tool_use" | "ContentTypeToolUse" => Some(5),
        "tool_result" | "ContentTypeToolResult" => Some(6),
        _ => None,
    })
}
fn parts(body: &Value) -> Vec<Value> {
    body.as_array().into_iter().flatten().filter_map(|b|match kind(b) {
        Some(2) if b["MediaType"].as_str().is_some_and(|s|!s.is_empty())=>Some(json!({"type":"image","mimeType":b["MediaType"],"data":b["Data"]})),
        Some(2)=>Some(json!({"type":"text","text":b["Text"]})),
        Some(3)=> {
            let mut p=json!({"type":"reasoning","visibility":"full","text":b["Thinking"]});copy(&mut p,"signature",b,"Signature");
            if !b["OpenAIResponsesReasoning"].is_null() {c::extra(&mut p,P,"openaiReasoning",&b["OpenAIResponsesReasoning"]);}
            Some(p)
        },
        Some(4)=>Some(json!({"type":"reasoning","visibility":"redacted","opaque":{"format":"anthropic.redacted_thinking","data":b["Data"]}})),
        Some(5|6)=>None,
        _=>Some(json!({"type":"data","schema":"dev.exe.shelley/content","data":b})),
    }).collect()
}
fn copied(v: &mut Value, h: &Value, n: &Value, record: Option<String>) {
    if let Some(owner) = n["_copySession"].as_str() {
        v["copiedFrom"] = json!({"session":reference(P,h,owner),"proof":"native_field"});
        if let Some(record) = record {
            v["copiedFrom"]["record"] = json!(record);
        }
    }
}
pub(super) fn convert(s: &mut Spool, w: &mut Writer, h: &Value, row: &Value) -> Result<()> {
    let n = &row["data"];
    s.save(
        "native_copy",
        &n["_copySession"]
            .as_str()
            .map(|owner| json!({"session":reference(P,h,owner),"proof":"native_field"}))
            .unwrap_or(Value::Null),
    )?;
    if row["kind"] == "session" {
        c::session_info(s, w, n, "slug")?;
        if let Some(head) = s.entity(&format!("generation_tip:{}", n["current_generation"]))? {
            c::emit(
                s,
                w,
                json!({"type":"session_info","heads":[head]}),
                &json!({}),
            )?;
        }
        if let Some(model) = n["model"].as_str() {
            c::emit(
                s,
                w,
                json!({"type":"config_change","snapshot":true,"model":{"id":model}}),
                &json!({"conversation_options":n["conversation_options"]}),
            )?;
        }
        return Ok(());
    }
    if n.get("_oversized").is_some() {
        c::emit(
            s,
            w,
            json!({"type":"gap","reason":"conversion_loss","count":1}),
            n,
        )?;
        return Ok(());
    }
    let body = c::decoded(&n["llm_data"]);
    let display = c::decoded(&n["user_data"]);
    let native_id = n["message_id"].as_str().unwrap_or("");
    let t = n["type"].as_str().unwrap_or("");
    if t == "system" {
        let mut content = parts(&body["Content"]);
        c::binaries(s, &mut content)?;
        c::emit(
            s,
            w,
            json!({"type":"instructions","kind":"system_prompt","content":content}),
            n,
        )?;
        return Ok(());
    }
    if t == "modelchange" {
        let mut v = json!({"type":"config_change"});
        if let Some(model) = display["to"].as_str().filter(|s| !s.is_empty()) {
            v["model"] = json!({"id":model});
        }
        if let Some(effort) = display["reasoning_to"].as_str().filter(|s| !s.is_empty()) {
            v["effort"] = json!(effort);
        }
        if v.get("model").is_some() || v.get("effort").is_some() {
            c::emit(s, w, v, n)?;
        }
    }
    if t == "gitinfo" {
        if let Some(path) = display["worktree"].as_str().filter(|s| s.starts_with('/')) {
            let mut v = json!({"type":"workspace_state","cwd":{"root":null,"path":path}});
            copy(&mut v, "branch", &display, "branch");
            if let Some(head) = display["commit"].as_str().filter(|s| {
                matches!(s.len(), 40 | 64)
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            }) {
                v["head"] = json!(head);
            }
            c::emit(s, w, v, n)?;
        }
    }
    let notice = matches!(t, "gitinfo" | "modelchange" | "error" | "warning");
    let excluded = n["excluded_from_context"] == true
        || n["excluded_from_context"] == 1
        || body["ExcludedFromContext"] == true
        || notice;
    let generation = n["generation"].clone();
    let previous = s.entity(&format!("generation_tip:{generation}"))?;
    if body.is_object() || display.is_object() {
        let role = match body["Role"].as_i64() {
            Some(0) => Some("user"),
            Some(1) => Some("assistant"),
            _ => match body["Role"].as_str() {
                Some("user") => Some("user"),
                Some("assistant") => Some("assistant"),
                _ => None,
            },
        };
        let mut content = parts(&body["Content"]);
        c::binaries(s, &mut content)?;
        let mut v = json!({"type":"message","messageId":native_id,"content":content});
        if t == "user" {
            if let Some(email) = n["user_email"].as_str().filter(|s| !s.is_empty()) {
                v["from"] = json!({"address":email});
            }
        }
        if let Some(role) = role {
            v["role"] = json!(role);
        } else {
            v["capture"]["role"] = json!({"state":"absent"});
        }
        if n["llm_data"].is_null() {
            v["capture"]["content"] = json!({"state":"absent"});
        }
        if !n["user_data"].is_null() {
            let mut display_content = parts(&display["Content"]);
            c::binaries(s, &mut display_content)?;
            v["displayContent"] = json!(display_content);
            v["displayCorrespondence"] = json!("native");
        }
        if notice {
            v["author"] = json!("harness");
            v["origin"] = json!("notice");
            v["audience"] = json!("user");
            v["displayContent"] = v["content"].take();
            v["content"] = json!([]);
            v["capture"]["content"] = json!({"state":"absent"});
            v.as_object_mut().unwrap().remove("role");
            v["capture"]["role"] = json!({"state":"absent"});
        } else if role == Some("assistant") && t == "agent" {
            v["author"] = json!("model");
        }
        if !excluded
            && !generation.is_null()
            && w.tip.as_deref() != previous.as_ref().and_then(Value::as_str)
        {
            v["branchesFrom"] = json!({"record":previous});
        }
        if body["EndOfTurn"] == true {
            v["phase"] = json!("final");
        }
        c::timestamp(&mut v, n, "created_at", false);
        copied(
            &mut v,
            h,
            n,
            n["forked_from_message_id"]
                .as_str()
                .map(|id| format!("messageId={id}")),
        );
        c::emit(s, w, v, n)?;
        for b in body["Content"].as_array().into_iter().flatten() {
            match kind(b) {
                Some(5) => c::call(
                    s,
                    w,
                    b,
                    b["ID"].as_str(),
                    b["ToolName"].as_str(),
                    &b["ToolInput"],
                )?,
                Some(6) => {
                    let mut v = c::result(s, w, b["ToolUseID"].as_str(), &Value::Null)?;
                    let mut content = parts(&b["ToolResult"]);
                    c::binaries(s, &mut content)?;
                    v["content"] = json!(content);
                    v.as_object_mut().unwrap().remove("capture");
                    if let Some(error) = b["ToolError"].as_bool() {
                        v["status"] = json!(if error { "failed" } else { "completed" });
                    }
                    if let Some(t) = c::time(&b["ToolUseStartTime"]) {
                        v["startedAt"] = json!(t);
                    }
                    if let Some(t) = c::time(&b["ToolUseEndTime"]) {
                        v["completedAt"] = json!(t);
                    }
                    copied(&mut v, h, n, None);
                    c::emit(s, w, v, b)?;
                }
                _ => {}
            }
        }
    } else {
        c::vendor(s, w, "message", n)?;
    }
    if !excluded && !generation.is_null() {
        if let Some(tip) = &w.tip {
            s.save(&format!("generation_tip:{generation}"), &json!(tip))?;
        }
    }
    let usage = c::decoded(&n["usage_data"]);
    // Other message kinds carry a zero-valued Usage struct as a persistence default.
    if usage.is_object() && matches!(t, "agent" | "slug") {
        inference(
            s,
            w,
            h,
            n,
            &usage,
            if t == "slug" { Some("title") } else { None },
            "response",
        )?;
    }
    let other = c::decoded(&n["other_usage_data"]);
    for (i, u) in other.as_array().into_iter().flatten().enumerate() {
        let purpose = match u["purpose"].as_str() {
            Some("compaction") => Some("compaction"),
            Some("slug") => Some("title"),
            _ => None,
        };
        inference(s, w, h, n, u, purpose, &format!("other:{i}"))?;
    }
    Ok(())
}
fn inference(
    s: &Spool,
    w: &mut Writer,
    h: &Value,
    n: &Value,
    u: &Value,
    purpose: Option<&str>,
    suffix: &str,
) -> Result<()> {
    let id = format!("{}:{suffix}", n["message_id"].as_str().unwrap_or(""));
    let mut v =
        json!({"type":"inference","inferenceId":id,"idSource":"derived","usage":c::usage(u,true)});
    if let Some(p) = purpose {
        v["purpose"] = json!(p);
    }
    if let Some(model) = u["model"].as_str() {
        v["model"] = json!({"id":model});
    }
    for (key, dest) in [("start_time", "startedAt"), ("end_time", "completedAt")] {
        if let Some(t) = c::time(&u[key]) {
            v[dest] = json!(t);
        }
    }
    if u["cost_usd"].is_number() {
        v["cost"] = json!({"amount":u["cost_usd"],"currency":"USD","source":"harness_estimated"});
    }
    copied(
        &mut v,
        h,
        n,
        n["forked_from_message_id"]
            .as_str()
            .map(|id| format!("inferenceId={id}:{suffix}")),
    );
    c::emit(s, w, v, u)?;
    Ok(())
}
