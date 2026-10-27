mod items;
mod paths;
mod structure;
mod usage;
use crate::{
    content,
    records::{copy, extras, meta, path, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use serde_json::{json, Value};
pub(crate) use structure::{segment, track};

pub(crate) fn convert(
    spool: &mut Spool,
    writer: &mut Writer,
    _pos: u64,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let p = &v["payload"];
    let kind = v["type"].as_str().unwrap_or("unknown");
    if let Some(turn) = p["turn_id"]
        .as_str()
        .or_else(|| p["turnId"].as_str())
        .or_else(|| {
            p.pointer("/internal_chat_message_metadata_passthrough/turn_id")
                .and_then(Value::as_str)
        })
    {
        writer.turn = Some(turn.into());
    }
    match kind {
        "session_meta" => writer.vendor(v, "session_meta"),
        "turn_context" => {
            if let Some(turn) = p["turn_id"].as_str() {
                spool.save(&format!("model:{turn}"), &p["model"])?;
            }
            config(writer, p, v, true, cwd)
        }
        "response_item" => response(spool, writer, p, v, cwd),
        "token_usage_record" => usage::inference(spool, writer, p, v),
        "event_msg" => event(spool, writer, p, v, cwd),
        "compacted" => compaction(spool, writer, p, v),
        "inter_agent_communication" => agent_message(writer, p, v),
        "world_state" if p["full"] == true => {
            let state = &p["state"];
            for (key, kind) in [
                ("agents_md", "project"),
                ("host_skills", "skill"),
                ("permissions", "rules"),
                ("managed_developer_instructions", "managed"),
            ] {
                if let Some(s) = state.get(key) {
                    let body = s
                        .get("text")
                        .or_else(|| s.get("body"))
                        .or_else(|| s.get("instructions"))
                        .unwrap_or(s);
                    let mut r = json!({"type":"instructions","kind":kind});
                    if body.is_string() {
                        r["content"] = json!(content::parts(body, Provider::Codex));
                    } else {
                        r["content"] = json!([{"type":"data","data":s,"schema":format!("com.openai.codex/world_state/{key}")}]);
                    }
                    writer.emit(r, v)?;
                }
            }
            writer.vendor(v, "world_state")
        }
        "_gap" => {
            writer.emit(
                json!({"type":"gap","reason":v["reason"],"count":1}),
                &Value::Null,
            )?;
            Ok(())
        }
        _ => writer.vendor(v, kind),
    }
}
fn config(
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    snapshot: bool,
    cwd: Option<&str>,
) -> Result<()> {
    let mut r = json!({"type":"config_change","options":{}});
    if snapshot {
        r["snapshot"] = json!(true);
    }
    if let Some(model) = p["model"].as_str() {
        writer.model = Some(model.into());
        r["model"] = json!({"id":model});
    }
    for (dest, key) in [("effort", "effort"), ("permissionMode", "approval_policy")] {
        copy(&mut r, dest, p, key);
    }
    if p["sandbox_policy"].is_object() {
        let sandbox = &p["sandbox_policy"];
        let mut s = json!({});
        copy(&mut s, "mode", sandbox, "type");
        copy(&mut s, "network", sandbox, "network_access");
        if let Some(roots) = sandbox["writable_roots"].as_array() {
            s["writableRoots"] = json!(roots
                .iter()
                .filter_map(Value::as_str)
                .map(|p| path(p, cwd))
                .collect::<Vec<_>>());
        }
        r["sandbox"] = s;
    }
    if let Some(fields) = p.as_object() {
        for (key, value) in fields {
            if !matches!(
                key.as_str(),
                "model" | "effort" | "approval_policy" | "sandbox_policy" | "turn_id"
            ) {
                r["options"][key] = value.clone();
            }
        }
    }
    writer.emit(r, v)?;
    Ok(())
}
fn response(
    spool: &mut Spool,
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let kind = p["type"].as_str().unwrap_or("unknown");
    match kind {
        "message" | "reasoning" => {
            let id = p["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| writer.id());
            let mut m = json!({"type":"message","messageId":id,"content":content::codex_parts(p)});
            if p["id"].is_null() {
                m["idSource"] = json!("derived");
            }
            copy(&mut m, "role", p, "role");
            if kind == "reasoning" {
                m["role"] = json!("assistant");
                let mut reasoning = json!({"type":"reasoning"});
                if let Some(summary) = p["summary"].as_array() {
                    reasoning["summary"] = json!(summary
                        .iter()
                        .filter_map(|s| s["text"].as_str())
                        .collect::<Vec<_>>());
                    reasoning["visibility"] = json!("summary");
                }
                if let Some(encrypted) = p["encrypted_content"].as_str() {
                    reasoning["opaque"] =
                        json!({"format":"openai.encrypted_content","data":encrypted});
                    reasoning["visibility"] = json!("encrypted");
                }
                m["content"] = json!([reasoning]);
            }
            if m["role"] == "assistant" {
                m["author"] = json!("model");
            }
            if let Some(phase) = p["phase"].as_str() {
                m["phase"] = json!(if phase == "final_answer" {
                    "final"
                } else {
                    phase
                });
            }
            if let Some(status) = p["status"].as_str() {
                m["status"] = json!(match status {
                    "completed" => "complete",
                    "in_progress" => "partial",
                    "incomplete" => "interrupted",
                    s => s,
                });
            }
            if m.get("role").is_none() {
                m["capture"] = json!({"role":{"state":"absent"}});
            }
            metadata(&mut m, p, v);
            let rid = writer.emit(m, v)?;
            spool.set_output(&id, &rid)?;
            Ok(())
        }
        "agent_message" => agent_message(writer, p, v),
        "function_call" | "custom_tool_call" | "local_shell_call" | "tool_search_call" => {
            let id = p["call_id"]
                .as_str()
                .or_else(|| p["id"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| writer.id());
            let name = p["name"].as_str();
            let raw = p
                .get("arguments")
                .or_else(|| p.get("input"))
                .or_else(|| p.get("action"))
                .cloned()
                .unwrap_or(Value::Null);
            let parsed = raw.as_str().and_then(ctx_history_asr_core::numbers::parse);
            let text = raw.is_string() && parsed.is_none();
            let input = parsed.unwrap_or(raw);
            let mut r = tools::call(
                writer,
                spool,
                p,
                &id,
                name,
                if text {
                    tools::Arguments::Text(input.as_str().unwrap_or_default().to_owned())
                } else {
                    tools::Arguments::Json(input.clone())
                },
                "model",
            )?;
            if p.get("call_id").is_none() && p.get("id").is_none() {
                r["callIdSource"] = json!("derived");
            }
            if let Some(enc) = p.get("encrypted_function_args").filter(|v| !v.is_null()) {
                meta(
                    &mut r,
                    Provider::Codex,
                    "encryptedFunctionArgs",
                    &json!({"format":"openai.encrypted_function_args","data":enc}),
                );
                if input.is_null() {
                    r["capture"]["input"] = json!({"state":"omitted","reason":"encrypted"});
                }
            }
            metadata(&mut r, p, v);
            let rid = writer.emit(r, v)?;
            if let Some(native) = p["id"].as_str() {
                spool.set_output(native, &rid)?;
            }
            tools::call_audit(writer, spool, v, &id, name.unwrap_or(""), &input, cwd)?;
            Ok(())
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            let Some(id) = p["call_id"].as_str() else {
                return writer.vendor(v, kind);
            };
            let mut r =
                json!({"type":"tool_result","callId":id,"linkage":"exact","status":"unknown"});
            let secret = spool.entity(&format!("secret:{id}"))?.is_some();
            if secret {
                r["capture"] = json!({"content":{"state":"redacted","reason":"policy"}});
            } else {
                if !p["output"].is_null() {
                    r["content"] = json!(content::parts(&p["output"], Provider::Codex));
                }
                content::capture_content(&mut r, &p["output"]);
                metadata(&mut r, p, v);
            }
            tools::result(writer, spool, id, r, v)?;
            let output = p["output"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| p["output"].clone());
            tools::result_audit(writer, spool, v, id, &output, "unknown", cwd)?;
            Ok(())
        }
        "compaction" | "compaction_summary" | "context_compaction" => {
            compaction(spool, writer, p, v)
        }
        _ => writer.vendor(v, kind),
    }
}
fn metadata(r: &mut Value, p: &Value, v: &Value) {
    if let Some(citation) = p.get("memory_citation") {
        meta(r, Provider::Codex, "memoryCitation", citation);
    }
    for (k, dest) in [
        ("user_input_order", "userInputOrder"),
        ("mcp_attribution", "mcpAttribution"),
        ("fallback_token_limit_override", "tokenLimit"),
        ("sender_user_messages", "senderUserMessages"),
    ] {
        if let Some(value) = v
            .get("metadata")
            .and_then(|m| m.get(k))
            .or_else(|| p.get("metadata").and_then(|m| m.get(k)))
        {
            meta(r, Provider::Codex, dest, value);
        }
    }
    extras(
        r,
        Provider::Codex,
        p,
        &[
            "id",
            "role",
            "content",
            "summary",
            "encrypted_content",
            "call_id",
            "arguments",
            "input",
            "output",
            "name",
            "phase",
            "status",
            "internal_chat_message_metadata_passthrough",
        ],
    );
}
fn agent_message(writer: &mut Writer, p: &Value, v: &Value) -> Result<()> {
    let mut r = json!({"type":"message","messageId":p.get("id").filter(|x|!x.is_null()).cloned().unwrap_or(json!(writer.id())),"role":"user","author":"agent","origin":"inter_session","content":content::codex_parts(p)});
    if p["id"].is_null() {
        r["idSource"] = json!("derived");
    }
    if let Some(author) = p["author"].as_str() {
        r["from"] = json!({"session":writer.root_session,"address":author});
    }
    let mut recipients = Vec::new();
    if let Some(to) = p["recipient"].as_str() {
        recipients
            .push(json!({"actor":{"session":writer.root_session,"address":to},"kind":"primary"}));
    }
    for to in p["other_recipients"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        recipients.push(
            json!({"actor":{"session":writer.root_session,"address":to},"kind":"additional"}),
        );
    }
    if !recipients.is_empty() {
        r["recipients"] = json!(recipients);
    }
    copy(&mut r, "triggersTurn", p, "trigger_turn");
    metadata(&mut r, p, v);
    writer.emit(r, v)?;
    Ok(())
}
fn event(
    spool: &mut Spool,
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let kind = p["type"].as_str().unwrap_or("unknown");
    match kind {
        "item_completed" => items::convert(spool, writer, &p["item"], v, cwd),
        "token_count" => usage::summary(writer, p, v),
        "task_started" | "turn_started" | "task_complete" | "turn_complete" | "turn_aborted" => {
            let started = matches!(kind, "task_started" | "turn_started");
            let mut r = json!({"type":"state","state":if started{"running"}else{"idle"}});
            if !started {
                r["stopReason"] = json!(if kind == "turn_aborted" {
                    match p["reason"].as_str() {
                        None | Some("interrupted") => "cancelled".to_owned(),
                        Some(_) => "cancelled".to_owned(),
                    }
                } else if p.get("error").is_some_and(|v| !v.is_null()) {
                    "error".to_owned()
                } else {
                    "end_turn".to_owned()
                });
            }
            if kind == "turn_aborted" {
                if let Some(reason) = p.get("reason") {
                    meta(&mut r, Provider::Codex, "abortReason", reason);
                }
            }
            if let Some(error) = p.get("error").filter(|v| !v.is_null()) {
                r["error"] = json!({});
                copy(&mut r["error"], "message", error, "message");
                meta(&mut r, Provider::Codex, "error", error);
            }
            for (dest, key) in [
                ("traceId", "trace_id"),
                ("durationMs", "duration_ms"),
                ("timeToFirstTokenMs", "time_to_first_token_ms"),
            ] {
                if let Some(value) = p.get(key) {
                    meta(&mut r, Provider::Codex, dest, value);
                }
            }
            if kind == "task_started" {
                spool.db.execute(
                    "INSERT OR IGNORE INTO turns VALUES(?1,?2)",
                    rusqlite::params![writer.pos, serde_json::to_string(&writer.tip)?],
                )?;
            }
            if let Some(turn) = writer.turn.as_ref() {
                let key = format!("turn:{turn}");
                if spool.entity(&key)?.is_none() {
                    if let Some(trigger) = p
                        .pointer("/turn_attribution/turn_trigger")
                        .and_then(Value::as_str)
                    {
                        r["trigger"] = json!({"native":trigger});
                    }
                    if let Some(address) = p
                        .pointer("/turn_attribution/initiating_agent_path")
                        .and_then(Value::as_str)
                    {
                        if r.get("trigger").is_none() {
                            r["trigger"] = json!({});
                        }
                        r["trigger"]["source"] =
                            json!({"session":writer.root_session,"address":address});
                    }
                    spool.save(&key, &json!(writer.tip))?;
                }
                if let Some(root) = p
                    .get("root_turn_id")
                    .or_else(|| p.pointer("/turn_attribution/root_turn_id"))
                    .filter(|v| !v.is_null())
                {
                    r["rootTurn"] = json!({"session":writer.root_session,"turnId":root});
                }
            }
            if let Some(parent) = p.pointer("/turn_attribution/parent_turn_id") {
                meta(&mut r, Provider::Codex, "parentTurnId", parent);
            }
            writer.emit(r, v)?;
            Ok(())
        }
        "thread_settings_applied" => config(writer, &p["thread_settings"], v, false, cwd),
        "thread_goal_updated" => {
            let goal = &p["goal"];
            let id = goal["id"].as_str().unwrap_or("goal");
            let mut r = json!({"type":"plan","planId":id,"planType":"goal","idSource":"derived"});
            if let Some(status) = goal["status"].as_str() {
                r["status"] = json!(match status {
                    "usageLimited" => "usage_limited",
                    "budgetLimited" => "budget_limited",
                    s => s,
                });
            }
            if let Some(objective) = goal.get("objective") {
                r["content"] = json!(content::parts(objective, Provider::Codex));
            } else {
                r["capture"] = json!({"content":{"state":"absent"}});
            }
            for (dest, keys) in [
                (
                    "budget",
                    [("tokens", "token_budget"), ("tokens", "tokenBudget")],
                ),
                (
                    "used",
                    [("tokens", "tokens_used"), ("tokens", "tokensUsed")],
                ),
            ] {
                for (field, key) in keys {
                    if let Some(value) = goal.get(key) {
                        if r.get(dest).is_none() {
                            r[dest] = json!({});
                        }
                        r[dest][field] = value.clone();
                    }
                }
            }
            if let Some(seconds) = goal
                .get("time_used_seconds")
                .or_else(|| goal.get("timeUsedSeconds"))
            {
                if r.get("used").is_none() {
                    r["used"] = json!({});
                }
                r["used"]["seconds"] = seconds.clone();
            }
            writer.revise(spool, &format!("goal:{id}"), r, v)?;
            Ok(())
        }
        "thread_rolled_back" => structure::rollback(spool, writer, p, v),
        _ => writer.vendor(v, kind),
    }
}
fn compaction(spool: &Spool, writer: &mut Writer, p: &Value, v: &Value) -> Result<()> {
    let id = p["window_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| writer.id());
    let mut r = json!({"type":"compaction","compactionId":id,"kind":"summary","replaces":writer.tip.as_ref().map_or(json!({"state":"unknown"}),|id|json!({"throughId":id}))});
    if p.get("window_id").is_none() {
        r["idSource"] = json!("derived");
    }
    if let Some(summary) = p.get("message") {
        r["summary"] = json!(content::parts(summary, Provider::Codex));
    }
    let mut retains = Vec::new();
    for (n, item) in p["replacement_history"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let retained = item.get("retained_source").or_else(|| {
            p["replacement_history_metadata"]
                .get(n)
                .and_then(|m| m.get("retained_source"))
        });
        if let Some(retained) = retained {
            if let Some(native) = retained.pointer("/id/message_id").and_then(Value::as_str) {
                if let Some(rid) = structure::retained(spool, native)? {
                    let mut entry = json!({"ref":rid});
                    if retained["complete"] == false {
                        entry["capture"] = json!("truncated");
                    }
                    retains.push(entry);
                } else {
                    meta(
                        &mut r,
                        Provider::Codex,
                        "unresolvedRetainedSource",
                        retained,
                    );
                }
            }
        }
        if let Some(encrypted) = item["encrypted_content"].as_str() {
            if matches!(
                item["type"].as_str(),
                Some("compaction" | "compaction_summary")
            ) {
                r["kind"] = json!("provider_native");
                r["opaque"] = json!({"format":"openai.encrypted_content","data":encrypted});
            }
        }
    }
    if let Some(encrypted) = p["encrypted_content"].as_str() {
        r["kind"] = json!("provider_native");
        r["opaque"] = json!({"format":"openai.encrypted_content","data":encrypted});
    }
    if !retains.is_empty() {
        r["retains"] = json!(retains);
    }
    let mut details = json!({});
    for (key, value) in p.as_object().into_iter().flatten() {
        if key == "replacement_history"
            || key == "replacement_history_metadata"
            || key == "retained_context"
            || key == "compaction_model_hash"
            || key.starts_with("guardian_")
        {
            details[key] = value.clone();
        }
    }
    if details.as_object().is_some_and(|o| !o.is_empty()) {
        meta(&mut r, Provider::Codex, "compaction", &details);
    }
    writer.emit(r, v)?;
    if let Some(response) = p["compaction_response_id"].as_str() {
        if spool.entity(&format!("inference:{response}"))?.is_none() {
            writer.emit(
                json!({"type":"inference","inferenceId":response,"purpose":"compaction"}),
                v,
            )?;
            spool.save(&format!("inference:{response}"), &json!(true))?;
        }
    }
    Ok(())
}
