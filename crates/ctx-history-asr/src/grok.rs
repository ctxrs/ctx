//! Grok's persisted ACP v1 notifications. UI output is not model-facing tool output.
mod messages;
mod response;
use crate::{
    content,
    jsonl::{self, update},
    records::{copy, extras, meta, path, session_ref, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
const P: Provider = Provider::GrokBuild;
pub(crate) fn convert(
    spool: &mut Spool,
    w: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    if jsonl::session_id(P, v).is_some_and(|id| session_ref(P, &id) != w.root_session) {
        w.emit(
            json!({"type":"gap","reason":"conversion_loss"}),
            &jsonl::clock(P, v),
        )?;
        return w.vendor(v, "foreign_session");
    }
    if spool
        .entity("grok:skip")?
        .and_then(|v| v.as_u64())
        .is_some_and(|end| w.pos <= end)
    {
        return Ok(());
    }
    let u = update(v);
    let kind = u["sessionUpdate"].as_str().unwrap_or("unknown");
    let mut time = jsonl::clock(P, v);
    if let Some(turn) = v
        .pointer("/params/_meta/promptId")
        .or_else(|| v.pointer("/_meta/promptId"))
        .and_then(Value::as_str)
    {
        time["promptId"] = json!(turn);
    }
    match kind {
        "response_started" | "response_completed" => response::convert(spool, w, v, &time)?,
        "turn_completed" => response::turn(w, v, &time)?,
        "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" => {
            messages::emit(spool, w, v, &time)?
        }
        "tool_call" | "tool_call_update" => call(spool, w, v, &time, cwd)?,
        "plan" => {
            let mut r =
                json!({"type":"plan","planId":"plan","idSource":"derived","planType":"items"});
            copy(&mut r, "entries", u, "entries");
            native(&mut r, v);
            w.revise(spool, "grok:plan", r, &time)?;
        }
        "current_mode_update" => {
            let mut r = json!({"type":"config_change","mode":u["currentModeId"]});
            native(&mut r, v);
            w.emit(r, &time)?;
        }
        "session_info_update" => {
            let mut r = json!({"type":"session_info"});
            copy(&mut r, "title", u, "title");
            native(&mut r, v);
            w.emit(r, &time)?;
        }
        "config_option_update" => {
            let mut r = json!({"type":"config_change","options":{}});
            for o in u["configOptions"].as_array().into_iter().flatten() {
                if let Some(id) = o["id"].as_str() {
                    copy(&mut r["options"], id, o, "currentValue");
                    match o["category"].as_str() {
                        Some("model") => r["model"] = json!({"id":o["currentValue"]}),
                        Some("mode") => copy(&mut r, "mode", o, "currentValue"),
                        _ => {}
                    }
                }
            }
            native(&mut r, v);
            w.emit(r, &time)?;
        }
        "usage_update" => {
            let mut r = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"contextWindow":{}});
            copy(&mut r["contextWindow"], "used", u, "used");
            copy(&mut r["contextWindow"], "size", u, "size");
            copy(&mut r, "cost", u, "cost");
            native(&mut r, v);
            w.emit(r, &time)?;
        }
        "rewind_marker" => {
            if let Some(target) = u["target_prompt_index"].as_u64() {
                let key = format!("grok:prompt:{target}");
                if let Some(to) = spool.entity(&key)? {
                    let mut r = json!({"type":"rewind","to":to,"reason":"user_rewind"});
                    native(&mut r, v);
                    w.emit(r, &time)?;
                } else if target == 0 {
                    w.emit(
                        json!({"type":"rewind","to":null,"reason":"user_rewind"}),
                        &time,
                    )?;
                } else {
                    w.emit(json!({"type":"gap","reason":"not_retained"}), &time)?;
                    w.vendor(v, kind)?;
                }
            } else {
                w.vendor(v, kind)?;
            }
        }
        "subagent_spawned" => {
            if let Some(child) = u["child_session_id"].as_str() {
                let mut r = json!({"type":"subagent","childSession":session_ref(P,child)});
                copy(&mut r, "callId", u, "tool_call_id");
                copy(&mut r, "description", u, "description");
                native(&mut r, v);
                w.emit(r, &time)?;
            } else {
                w.vendor(v, kind)?;
            }
        }
        _ => {
            if v["type"] == "_gap" {
                w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), &time)?;
            } else {
                w.vendor(v, kind)?;
            }
        }
    }
    Ok(())
}
fn native(r: &mut Value, v: &Value) {
    let u = update(v);
    extras(
        r,
        P,
        v,
        &["params", "method", "update", "sessionId", "_meta"],
    );
    if let Some(params) = v.get("params") {
        extras(r, P, params, &["update", "sessionId", "_meta"]);
    }
    if let Some(m) = u.get("_meta") {
        meta(r, P, "updateMeta", m);
    }
    let e = v.get("params").unwrap_or(v);
    if let Some(m) = e.get("_meta") {
        meta(r, P, "eventMeta", m);
    }
    extras(
        r,
        P,
        u,
        &[
            "_meta",
            "sessionUpdate",
            "content",
            "rawInput",
            "rawOutput",
            "toolCallId",
            "title",
            "kind",
            "locations",
            "entries",
        ],
    );
}
fn patch(to: &mut Value, from: &Value) {
    if let (Some(to), Some(from)) = (to.as_object_mut(), from.as_object()) {
        for (k, v) in from {
            if v.is_null() {
                to.remove(k);
            } else if k == "_meta" {
                patch(to.entry(k).or_insert(json!({})), v);
            } else {
                to.insert(k.clone(), v.clone());
            }
        }
    }
}
fn call(spool: &Spool, w: &mut Writer, v: &Value, time: &Value, cwd: Option<&str>) -> Result<()> {
    let u = update(v);
    let Some(id) = u["toolCallId"].as_str() else {
        return w.vendor(v, "unidentified_call");
    };
    let ambiguous = spool.ambiguous_call(id)?;
    if ambiguous {
        // Keep separate requests, and do not assign later patches to one of reused IDs.
        if u["sessionUpdate"] == "tool_call" {
            let r = call_record(u, v, id, cwd);
            w.emit(r, time)?;
        } else if matches!(
            u["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            let mut r = result_record(u, id, cwd);
            r["linkage"] = json!("ambiguous");
            native(&mut r, v);
            w.emit(r, time)?;
        } else {
            w.vendor(v, "ambiguous_tool_update")?;
        }
        return Ok(());
    }
    let key = format!("grok:tool:{id}");
    let mut state = spool.entity(&key)?.unwrap_or(json!({}));
    patch(&mut state, u);
    spool.save(&key, &state)?;
    let mut r = call_record(&state, v, id, cwd);
    if let Some(response) = spool
        .entity("grok:response")?
        .filter(|v| v["id"].is_string())
    {
        r["inferenceId"] = response["id"].clone();
    }
    let call_key = format!("grok:call:{id}");
    let previous = spool.entity(&call_key)?;
    if previous.is_none()
        || u.get("rawInput").is_some()
        || u.get("title").is_some()
        || u.get("kind").is_some()
        || u.get("locations").is_some()
    {
        w.revise(spool, &call_key, r, time)?;
    }
    let status = state["status"].as_str().unwrap_or("unknown");
    if let Some(content) = u["content"].as_array() {
        let diffs: Vec<_> = content
            .iter()
            .filter(|p| p["type"] == "diff")
            .cloned()
            .collect();
        if !diffs.is_empty() {
            spool.save(&format!("grok:diffs:{id}"), &json!(diffs))?;
        }
    }
    let terminal = matches!(status, "completed" | "failed" | "cancelled");
    if terminal {
        let mut r = result_record(&state, id, cwd);
        native(&mut r, v);
        w.revise(spool, &format!("grok:result:{id}"), r, time)?;
    }
    if terminal
        || u["content"]
            .as_array()
            .is_some_and(|c| c.iter().any(|p| p["type"] == "diff"))
    {
        if let Some(diffs) = spool.entity(&format!("grok:diffs:{id}"))? {
            for (i, diff) in diffs.as_array().into_iter().flatten().enumerate() {
                let Some(file) = diff["path"].as_str() else {
                    continue;
                };
                let mut change = path(file, cwd);
                // ACP v1 carries snippets; a null oldText alone does not prove creation.
                change["op"] = json!("unknown");
                let mut r = json!({"type":"file_change","cause":{"callId":id},"status":match status{"completed"=>"applied","failed"=>"failed","cancelled"=>"unknown",_=>"proposed"},"precision":"snippet","changes":[change]});
                meta(&mut r, P, "diff", diff);
                w.revise(spool, &format!("grok:diff:{id}:{i}"), r, time)?;
            }
        }
    }
    Ok(())
}
fn call_record(u: &Value, v: &Value, id: &str, cwd: Option<&str>) -> Value {
    let mut r = json!({"type":"tool_call","callId":id,"callIdSource":"native"});
    let tool = &u["_meta"]["x.ai/tool"];
    if let Some(name) = tool["name"].as_str().or_else(|| u["name"].as_str()) {
        r["name"] = json!(name);
    } else {
        r["capture"] = json!({"name":{"state":"absent"}});
    }
    for (a, b) in [("title", "title"), ("kind", "kind"), ("input", "rawInput")] {
        copy(&mut r, a, u, b);
    }
    if r.get("kind").is_none() {
        copy(&mut r, "kind", tool, "kind");
    }
    if tool["namespace"] == "grok_build" {
        r["provider"] = json!({"kind":"builtin","identity":"exact"});
    }
    if let Some(locs) = u["locations"].as_array() {
        r["locations"] = json!(locs
            .iter()
            .filter_map(|l| {
                let mut p = path(l["path"].as_str()?, cwd);
                copy(&mut p, "line", l, "line");
                Some(p)
            })
            .collect::<Vec<_>>());
    }
    native(&mut r, v);
    r
}
fn result_record(u: &Value, id: &str, _cwd: Option<&str>) -> Value {
    let mut r = json!({"type":"tool_result","callId":id,"linkage":"exact","status":u["status"].as_str().unwrap_or("unknown"),"capture":{"content":{"state":"absent"}}});
    copy(&mut r, "raw", u, "rawOutput");
    let mut display = Vec::new();
    if let Some(content) = u.get("content") {
        meta(&mut r, P, "toolContent", content);
    }
    for block in u["content"].as_array().into_iter().flatten() {
        if block["type"] == "content" {
            display.extend(content::parts(&json!([block["content"]]), P));
        }
        if block["type"] == "terminal" {
            r["exec"] = json!({"terminalId":block["terminalId"]});
        }
    }
    if let Some(text) = u["content"].as_str() {
        display.push(json!({"type":"text","text":text}));
    }
    if u.get("content").is_some() {
        r["displayContent"] = json!(display);
    }
    if u["rawOutput"]["type"] == "Bash" {
        if r.get("exec").is_none() {
            r["exec"] = json!({});
        }
        copy(&mut r["exec"], "exitCode", &u["rawOutput"], "exit_code");
        copy(&mut r["exec"], "timedOut", &u["rawOutput"], "timed_out");
    }
    r
}
