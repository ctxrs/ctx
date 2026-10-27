mod loop_events;
use super::*;

pub(super) fn header(h: &mut Value, n: &Value) {
    h["sessionId"] = n["_agent"].clone();
    h["namespace"] = n["_nativeSession"].clone();
    let agent = n["_agent"].as_str().unwrap_or("main");
    let namespace = n["_nativeSession"].as_str().unwrap_or("");
    if let Some(parent) = n["_state"]["forkedFrom"].as_str() {
        h["lineage"]["forkedFrom"] = json!({"session":crate::records::session_ref(Provider::Kimi,&json!([parent,agent]).to_string()),"evidence":"native_field"});
    }
    if agent != "main" {
        h["scope"] = json!("subagent");
        if let Some(parent) = n["_state"]["agents"][agent]["parentAgentId"].as_str() {
            h["lineage"]["spawnedBy"] = json!({"session":crate::records::session_ref(Provider::Kimi,&json!([namespace,parent]).to_string()),"evidence":"native_field"});
        }
    }
    if let Some(ms) = n["_first"]["created_at"]
        .as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
    {
        h["startedAt"] = json!(ms.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    }
    h["producer"]["lossy"].as_array_mut().unwrap().extend([
        json!("_com.moonshot.kimi-code/request_projection"),
        json!("_com.moonshot.kimi-code/blobref_media"),
        json!("_com.moonshot.kimi-code/compaction_replaces"),
        json!("branches"),
        json!("tool_result.content"),
    ]);
}
pub(super) fn convert(s: &mut Spool, w: &mut Writer, _pos: u64, v: &Value) -> Result<()> {
    let mut source = v.clone();
    if let Some(ms) = v["time"]
        .as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
    {
        source["timestamp"] = json!(ms.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    }
    let v = &source;
    match v["type"].as_str().unwrap_or("unknown") {
        "context.append_loop_event" => return loop_events::convert(s, w, v),
        "llm.request" => {
            let mut r = json!({"type":"inference","inferenceId":format!("request:{}",w.id()),"idSource":"derived","idScope":"capture"});
            r["purpose"] = json!(if v["kind"] == "compaction" {
                "compaction"
            } else {
                "turn"
            });
            if let Some(model) = v["model"].as_str() {
                r["model"]["requested"] = json!(model);
            }
            for (to, from) in [
                ("effort", "thinkingEffort"),
                ("temperature", "temperature"),
                ("topP", "topP"),
                ("maxOutputTokens", "maxTokens"),
            ] {
                if let Some(x) = v.get(from) {
                    r["params"][to] = x.clone();
                }
            }
            retain(&mut r, w.provider, v, &[]);
            if let Some(step) = v["turnStep"].as_str() {
                s.save(&format!("kimi:request:{step}"), &r)?;
            }
            let key = format!("kimi:inference:{}", r["inferenceId"].as_str().unwrap());
            w.revise(s, &key, r, v)?;
        }
        "turn.prompt" | "turn.steer" => {
            // Acceptance does not prove delivery or a new turn: steer may queue.
            // The context append is the authoritative model message.
            w.turn = None;
            w.vendor(v, v["type"].as_str().unwrap())?;
        }
        "context.append_message" => {
            let m = &v["message"];
            if m["role"] == "tool" {
                let mut r = result(
                    s,
                    m["toolCallId"].as_str(),
                    match m["isError"].as_bool() {
                        Some(true) => "failed",
                        Some(false) => "completed",
                        None => "unknown",
                    },
                    None,
                    w.provider,
                )?;
                r["raw"] = m.clone();
                r["displayContent"] = json!(parts(&m["content"], w.provider));
                retain(&mut r, w.provider, v, &["message"]);
                emit(w, r, v)?;
            } else if let Some(role) = m["role"].as_str() {
                let mut r =
                    json!({"type":"message","role":role,"content":parts(&m["content"],w.provider)});
                entity(&mut r, "messageId", None, format!("message:{}", w.id()));
                origin(&mut r, &m["origin"]);
                if role == "assistant" {
                    r["author"] = json!("model");
                }
                retain(&mut r, w.provider, v, &[]);
                emit(w, r, v)?;
                for call in m["toolCalls"].as_array().into_iter().flatten() {
                    let mut r = json!({"type":"tool_call","requestedBy":"model"});
                    entity(
                        &mut r,
                        "callId",
                        call["id"].as_str(),
                        format!("call:{}", w.id()),
                    );
                    copy(&mut r, "name", call, "name");
                    if r.get("name").is_none() {
                        r["capture"]["name"] = json!({"state":"absent"});
                    }
                    input(&mut r, call.get("arguments"));
                    retain(&mut r, w.provider, call, &[]);
                    emit(w, r, v)?;
                }
            } else {
                w.vendor(v, "context.append_message")?;
            }
        }
        "context.apply_compaction" => blocks::compaction(
            w,
            v,
            v.get("contextSummary").or_else(|| v.get("summary")),
            None,
        )?,
        "config.update" | "profile.bind" | "permission.set_mode" => {
            let mut r = json!({"type":"config_change"});
            if let Some(model) = v["model"].as_str().or_else(|| v["modelAlias"].as_str()) {
                r["model"] = json!({"id":model});
            }
            copy(&mut r, "effort", v, "thinkingEffort");
            if v["type"] == "permission.set_mode" {
                copy(&mut r, "permissionMode", v, "mode");
            }
            copy(&mut r, "agent", v, "profileName");
            if r.as_object().unwrap().len() == 1 {
                w.vendor(v, v["type"].as_str().unwrap())?;
            } else {
                retain(&mut r, w.provider, v, &[]);
                emit(w, r, v)?;
            }
            if let Some(prompt) = v["systemPrompt"].as_str() {
                emit(
                    w,
                    json!({"type":"instructions","kind":"system_prompt","content":[{"type":"text","text":prompt}]}),
                    v,
                )?;
            }
        }
        "llm.tools_snapshot" => {
            let mut r = json!({"type":"instructions","kind":"tool_definitions","content":[{"type":"data","data":v["tools"]}]});
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
        "context.update_token_count" => {
            let mut r = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":false,"contextWindow":{"used":v["tokenCount"]}});
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
        "permission.record_approval_result" => {
            let decision = match v["result"]["decision"].as_str() {
                Some("approved") if v["result"]["scope"] == "session" => "allow_session",
                Some("approved") => "allow_once",
                Some("rejected") => "reject_once",
                Some("cancelled") => "cancelled",
                _ => "_com.moonshot.kimi-code/unknown",
            };
            let mut r = json!({"type":"permission","requestId":format!("permission:{}",w.id()),"idSource":"derived","subject":{"callId":v["toolCallId"]},"decision":decision,"decidedBy":"user"});
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
        kind => w.vendor(v, kind)?,
    }
    Ok(())
}
fn origin(r: &mut Value, o: &Value) {
    match o["kind"].as_str() {
        Some("user") => {
            r["author"] = json!("human");
            r["origin"] = json!("prompt");
        }
        Some("hook_result") => {
            r["author"] = json!("hook");
            r["origin"] = json!("injected");
        }
        Some(
            "cron_job" | "cron_missed" | "background_task" | "system_trigger" | "injection"
            | "retry" | "compaction_summary",
        ) => {
            r["author"] = json!("harness");
            r["origin"] = json!("injected");
        }
        _ => {}
    }
}
