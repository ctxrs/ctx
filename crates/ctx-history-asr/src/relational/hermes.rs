//! Hermes state.db at nousresearch/hermes-agent@89a6e05de462768016b2ae0d684338e3cce3f3ee.
use super::{common as c, reference, sqlite};
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};
const P: Provider = Provider::Hermes;
pub(super) fn ingest(db: &Connection, s: &mut Spool, h: &mut Value) -> Result<()> {
    s.db.execute_batch("CREATE TABLE hermes_call_alias(provider_id TEXT,call_id TEXT); CREATE INDEX hermes_call_alias_ids ON hermes_call_alias(provider_id);")?;
    let id = h["_sessionId"].as_str().unwrap_or("").to_owned();
    if h["system_prompt"].is_null() {
        if let Some(hash) = h["system_prompt_hash"].as_str() {
            if sqlite::exists(db, "system_prompts")? {
                if let Ok(row) = sqlite::one(db, "system_prompts", "hash", hash, &[], s.limit) {
                    h["system_prompt"] = row["prompt"].clone();
                }
            }
        }
    }
    // Native row IDs preserve insertion order. Active-row links describe current replay
    // separately; archived rows never move across surviving rows in the serialization.
    let order = "id";
    sqlite::visit(
        db,
        "messages",
        Some(("session_id", &id)),
        order,
        &[],
        s.limit,
        &mut |n| {
            let calls = c::decoded(&n["tool_calls"]);
            let ids: Vec<_> = calls
                .as_array()
                .into_iter()
                .flatten()
                .map(|call| json!({"id":call_id(&n,call)}))
                .collect();
            c::index_calls(s, &json!(ids))?;
            for call in calls.as_array().into_iter().flatten() {
                if let (Some(raw), Some(id)) = (call["id"].as_str(), call_id(&n, call)) {
                    s.db.execute(
                        "INSERT INTO hermes_call_alias VALUES(?1,?2)",
                        rusqlite::params![raw, id],
                    )?;
                }
            }
            if n["role"] == "tool" {
                c::index_result(s, result_id(&n))?;
            }
            let key = format!("message:{}", c::id(&n["id"]).unwrap_or_default());
            s.push_relational("message", &key, &n, false)
        },
    )?;
    // Receipts are an immutable per-execution log. Serialize execution IDs in
    // lexical order, then native sequence; do not interleave by wall-clock time.
    if sqlite::exists(db, "worker_executions")? && sqlite::exists(db, "worker_receipts")? {
        let mut stmt = db.prepare(
            "SELECT execution_id FROM worker_executions WHERE session_id=?1 ORDER BY execution_id",
        )?;
        let ids = stmt.query_map([&id], |r| r.get::<_, String>(0))?;
        for execution in ids {
            let execution = execution?;
            sqlite::visit(
                db,
                "worker_receipts",
                Some(("execution_id", &execution)),
                "sequence",
                &[],
                s.limit,
                &mut |receipt| {
                    s.push_relational(
                        "worker_receipts",
                        &format!("receipt:{execution}:{}", receipt["sequence"]),
                        &receipt,
                        false,
                    )
                },
            )?;
        }
    }
    // Independently owned mutable tables are snapshots at the end, never timestamp-merged.
    for (table, key) in [
        ("session_model_usage", "session_id"),
        ("session_admissions", "target_session_id"),
        ("worker_executions", "session_id"),
    ] {
        if sqlite::exists(db, table)? {
            sqlite::visit(
                db,
                table,
                Some((key, &id)),
                "rowid",
                &[],
                s.limit,
                &mut |n| {
                    let identity = match table {
                        "session_model_usage" => json!([
                            n["model"],
                            n["billing_provider"],
                            n["billing_base_url"],
                            n["billing_mode"],
                            n["task"]
                        ])
                        .to_string(),
                        "session_admissions" => n["admission_id"].as_str().unwrap_or("").to_owned(),
                        _ => n["execution_id"].as_str().unwrap_or("").to_owned(),
                    };
                    s.push_relational(table, &format!("{table}:{identity}"), &n, false)?;
                    Ok(())
                },
            )?;
        }
    }
    s.push_relational("session", "session", h, false)
}
fn call_id(n: &Value, call: &Value) -> Option<String> {
    let id = call["id"].as_str()?;
    let uids = c::decoded(&n["tool_call_uids"]);
    Some(uids[id].as_str().unwrap_or(id).to_owned())
}
fn result_id(n: &Value) -> Option<&str> {
    n["tool_call_uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| n["tool_call_id"].as_str())
}
fn result(s: &Spool, w: &Writer, n: &Value, body: &Value) -> Result<Value> {
    if n["tool_call_uid"].as_str().is_some_and(|s| !s.is_empty()) {
        return c::result(s, w, result_id(n), body);
    }
    let raw = n["tool_call_id"].as_str();
    if let Some(raw) = raw {
        let (count, id): (u64, Option<String>) = s.db.query_row(
            "SELECT count(*),min(call_id) FROM hermes_call_alias WHERE provider_id=?1",
            [raw],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if count == 1 {
            return c::result(s, w, id.as_deref(), body);
        }
        if count > 1 {
            let mut v = c::result(s, w, None, body)?;
            v["linkage"] = json!("ambiguous");
            c::extra(&mut v, P, "callId", &json!(raw));
            return Ok(v);
        }
    }
    c::result(s, w, raw, body)
}
pub(super) fn header(h: &Value, v: &mut Value) {
    if let Some(t) = c::epoch(&h["started_at"]) {
        v["startedAt"] = json!(t);
    }
    copy(v, "conversationKey", h, "session_key");
    if let Some(source) = h["source"].as_str() {
        v["client"] = json!({"name":source});
        if let Some(kind) = match source {
            "cron" => Some("schedule"),
            "webhook" | "msgraph_webhook" => Some("webhook"),
            _ => None,
        } {
            v["trigger"] = json!({"kind":kind,"native":source});
        }
        if let Some(chat) = h["chat_id"].as_str() {
            if matches!(
                source,
                "whatsapp"
                    | "telegram"
                    | "slack"
                    | "discord"
                    | "imessage"
                    | "signal"
                    | "email"
                    | "sms"
                    | "web"
            ) {
                let mut conversation =
                    json!({"id":"conversation","channel":source,"nativeId":chat});
                if let Some(kind) = match h["chat_type"].as_str() {
                    Some("dm") => Some("direct"),
                    Some("group") => Some("group"),
                    Some("channel") => Some("channel"),
                    Some("thread") => Some("thread"),
                    _ => None,
                } {
                    conversation["kind"] = json!(kind);
                }
                c::extra(&mut conversation, P, "threadId", &h["thread_id"]);
                v["conversations"] = json!([conversation]);
            }
        }
        if let Some(user) = h["user_id"].as_str().filter(|s| !s.is_empty()) {
            let origin = c::decoded(&h["origin_json"]);
            let mut participant = json!({"id":"creator","ids":[{"scheme":source,"value":user}]});
            copy(&mut participant, "displayName", &origin, "user_name");
            if origin["is_bot"] == true {
                participant["kind"] = json!("bot");
            }
            v["participants"] = json!([participant]);
        }
    }
    if h["source"] == "subagent" {
        v["scope"] = json!("subagent");
    }
    if let Some(parent) = h["parent_session_id"].as_str() {
        let config = c::decoded(&h["model_config"]);
        let mut lineage = json!({});
        let mut known = false;
        for (marker, facet) in [
            ("_branched_from", "forkedFrom"),
            ("_delegate_from", "spawnedBy"),
            ("_reset_from", "continues"),
        ] {
            // Compression rotations inherit model_config: only markers bound to this parent apply.
            if config[marker].as_str() == Some(parent) {
                lineage[facet] = json!({"session":reference(P,h,parent),"evidence":"native_field"});
                if facet == "continues" {
                    lineage[facet]["reason"] = json!("reset");
                }
                known = true;
            }
        }
        if !known {
            lineage["relatedUnknown"] =
                json!({"session":reference(P,h,parent),"evidence":"native_field"});
        }
        v["lineage"] = lineage;
    }
    for loss in [
        "branches",
        "copies",
        "_com.nousresearch.hermes/unassigned_facts",
        "_com.nousresearch.hermes/turn_triggers",
        "content.binary",
    ] {
        v["producer"]["lossy"]
            .as_array_mut()
            .unwrap()
            .push(json!(loss));
    }
    c::extra(v, P, "origin", &c::decoded(&h["origin_json"]));
}
pub(super) fn convert(s: &mut Spool, w: &mut Writer, h: &Value, row: &Value) -> Result<()> {
    let n = &row["data"];
    match row["kind"].as_str().unwrap_or("") {
        "message" => message(s, w, h, n),
        "session" => {
            c::session_info(s, w, n, "title")?;
            if let Some(head) = s.entity("active_tip")? {
                c::emit(
                    s,
                    w,
                    json!({"type":"session_info","heads":[head]}),
                    &json!({}),
                )?;
            }
            if let Some(v) = c::summary(n, false, "session") {
                c::emit(s, w, v, n)?;
            }
            if let Some(prompt) = n["system_prompt"].as_str() {
                c::emit(
                    s,
                    w,
                    json!({"type":"instructions","kind":"system_prompt","content":c::parts(&json!(prompt),P)}),
                    &json!({"system_prompt_hash":n["system_prompt_hash"]}),
                )?;
            }
            if let Some(model) = n["model"].as_str() {
                let mut config =
                    json!({"type":"config_change","snapshot":true,"model":{"id":model}});
                let options = c::decoded(&n["model_config"]);
                if options.is_object() {
                    config["options"] = options;
                }
                c::emit(s, w, config, &json!({}))?;
            }
            if !n["ended_at"].is_null() {
                let mut v = json!({"type":"state","state":"ended"});
                c::timestamp(&mut v, n, "ended_at", true);
                c::emit(s, w, v, &json!({"end_reason":n["end_reason"]}))?;
            }
            Ok(())
        }
        "session_model_usage" => {
            // The task dimension does not exist in byModel: retain it on this named counter.
            if let Some(mut v) = c::summary(n, false, row["key"].as_str().unwrap_or("model")) {
                c::extra(&mut v, P, "modelUsage", n);
                c::emit(s, w, v, n)?;
            }
            Ok(())
        }
        kind => c::vendor(s, w, kind, n),
    }
}
fn message(s: &mut Spool, w: &mut Writer, h: &Value, n: &Value) -> Result<()> {
    if n.get("_oversized").is_some() {
        c::emit(
            s,
            w,
            json!({"type":"gap","reason":"conversion_loss","count":1}),
            n,
        )?;
        return Ok(());
    }
    let id = c::id(&n["id"]).unwrap_or_else(|| w.id());
    let role = n["role"].as_str().unwrap_or("");
    let body = if n["api_content"].is_null() {
        c::decoded(&n["content"])
    } else {
        c::decoded(&n["api_content"])
    };
    // content is a literal string unless it is a native multimodal array.
    let body = if body.is_array() {
        body
    } else {
        if n["api_content"].is_null() {
            n["content"].clone()
        } else {
            n["api_content"].clone()
        }
    };
    let mut v = if n["_compressed_summary"] == 1 {
        json!({"type":"compaction","compactionId":id,"kind":"summary","replaces":{"state":"unknown"},"summary":c::parts(&body,P)})
    } else if role == "tool" {
        result(s, w, n, &body)?
    } else if matches!(role, "user" | "assistant" | "system" | "developer") {
        c::message(s, w, &id, role, &body)?
    } else {
        return c::vendor(s, w, "message", n);
    };
    if n["active"] == 1 {
        let previous = s.entity("active_tip")?;
        if w.tip.as_deref() != previous.as_ref().and_then(Value::as_str) {
            v["branchesFrom"] = json!({"record":previous});
        }
    }
    if v["type"] == "message" {
        // Mirrors drop their source marker at persistence, so assistant role alone cannot prove author.
        if n["observed"] == 1 {
            v["addressed"] = json!(false);
        }
        if let Some(id) = n["platform_message_id"].as_str() {
            v["platform"] = json!({"messageIds":[id]});
        }
        if super::header(P, h).get("conversations").is_some() {
            v["conversation"] = json!("conversation");
        }
        if !n["api_content"].is_null() {
            let mut display = c::parts(&n["content"], P);
            c::binaries(s, &mut display)?;
            v["displayContent"] = json!(display);
            v["displayCorrespondence"] = json!("native");
        }
        for field in ["reasoning_content", "reasoning"] {
            if let Some(text) = n[field].as_str() {
                v["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"reasoning","text":text}));
                if let Some(cap) = v.get_mut("capture").and_then(Value::as_object_mut) {
                    cap.remove("content");
                }
                break;
            }
        }
    }
    c::timestamp(&mut v, n, "timestamp", true);
    c::emit(s, w, v, n)?;
    let calls = c::decoded(&n["tool_calls"]);
    for call in calls.as_array().into_iter().flatten() {
        c::call(
            s,
            w,
            call,
            call_id(n, call).as_deref(),
            call["function"]["name"].as_str(),
            &call["function"]["arguments"],
        )?;
    }
    if n["active"] == 1 {
        if let Some(tip) = &w.tip {
            s.save("active_tip", &json!(tip))?;
        }
    }
    Ok(())
}
