use crate::{
    capture, content,
    records::{copy, meta, path, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

fn emit(spool: &Spool, w: &mut Writer, mut r: Value, n: &Value) -> Result<()> {
    capture::retain(&mut r, w.provider, n, spool)?;
    if matches!(r["type"].as_str(), Some("tool_call" | "tool_result")) {
        if let Some(id) = r["callId"].as_str() {
            let calls: u64 = spool.db.query_row(
                "SELECT count(*) FROM native_calls WHERE call_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let results: u64 = spool.db.query_row(
                "SELECT count(*) FROM tool_results WHERE call_id=?1",
                [id],
                |r| r.get(0),
            )?;
            if calls == 1 && (r["type"] == "tool_call" || results == 1) {
                let key = format!("{}:{id}", r["type"].as_str().unwrap_or_default());
                w.emit_identified(r, &json!({}), &key)?;
                return Ok(());
            }
        }
    }
    w.emit(r, &json!({}))?;
    Ok(())
}
fn message(spool: &Spool, w: &mut Writer, role: &str, body: &Value, n: &Value) -> Result<()> {
    if body.is_null() {
        return Ok(());
    }
    let mut r = json!({"type":"message","messageId":w.id(),"idSource":"derived","role":role,"content":content::parts(body,w.provider)});
    if !body.is_string() {
        r["capture"] = json!({"content":{"state":"omitted","reason":"unsupported"}});
    }
    if n["kind"] == "interrupted" && role == "assistant" {
        r["status"] = json!("interrupted");
    }
    emit(spool, w, r, n)
}
pub(super) fn turn(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    if n["kind"] == "compacted_summary" {
        return emit(
            spool,
            w,
            json!({"type":"compaction","compactionId":w.id(),"idSource":"derived","kind":"summary","replaces":{"state":"unknown"},"summary":content::parts(&n["summary"],w.provider)}),
            n,
        );
    }
    message(spool, w, "user", &n["user"]["text"], &n["user"])?;
    for step in n["execution"]["tool_steps"]
        .as_array()
        .into_iter()
        .flatten()
    {
        message(spool, w, "assistant", &step["assistant"], step)?;
        for call in step["tool_calls"].as_array().into_iter().flatten() {
            tool(spool, w, call)?;
        }
        for result in step["tool_results"].as_array().into_iter().flatten() {
            let mut r = json!({"type":"tool_result","status":match result["status"].as_str(){Some("success")=>"completed",Some("failure")=>"failed",_=>"unknown"},"linkage":"none","raw":result["output"],"content":content::parts(&result["output"],w.provider)});
            if let Some(id) = result["tool_call_id"].as_str() {
                let count: u64 = spool.db.query_row(
                    "SELECT count(*) FROM native_calls WHERE call_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                if count == 1 {
                    r["callId"] = json!(id);
                    r["linkage"] = json!("exact");
                } else {
                    r["linkage"] = json!(if count > 1 { "ambiguous" } else { "none" });
                    meta(&mut r, w.provider, "nativeCallId", &json!(id));
                }
            }
            if !result["output"].is_string() {
                r["capture"] = json!({"content":{"state":"omitted","reason":"unsupported"}});
            } else if result["truncated"] == true {
                r["capture"] = json!({"content":{"state":"truncated","reason":"source_truncated"}});
            }
            if let Some(ts) = capture::millis(&result["created_at_ms"]) {
                r["ts"] = json!(ts);
            }
            let process = &result["command_process_presentation"];
            if process["kind"] == "exit_code" && !capture::unsafe_numbers(&process["value"]) {
                copy(&mut r["exec"], "exitCode", process, "value");
            }
            if process["kind"] == "signal" {
                r["raw"]["signal"] = process["value"].clone();
            }
            if process["kind"] == "timed_out" {
                r["exec"]["timedOut"] = json!(true);
            }
            emit(spool, w, r, result)?;
        }
    }
    if n["tool_call"].is_object() {
        tool(spool, w, &n["tool_call"])?;
    }
    message(spool, w, "assistant", &n["assistant"], n)?;
    for f in n["execution"]["files"].as_array().into_iter().flatten() {
        let op = match f["action"].as_str() {
            Some("write" | "edit") => "modify",
            Some("delete") => "delete",
            Some("rename") => "move",
            Some("copy") => "copy",
            _ => continue,
        };
        if let Some(p) = f["path"].as_str() {
            let mut change = path(p, None);
            change["op"] = json!(op);
            if matches!(op, "move" | "copy") {
                let Some(to) = f["new_path"].as_str() else {
                    continue;
                };
                let old = change;
                change = path(to, None);
                change["op"] = json!(op);
                change["oldRoot"] = old["root"].clone();
                change["oldPath"] = old["path"].clone();
            }
            let r = json!({"type":"file_change","status":if f["status"]=="success" {"applied"} else {"failed"},"precision":"unknown","changes":[change]});
            emit(spool, w, r, f)?;
        }
    }
    // All turn fields, including images, non-UTF8 durable bytes, background handles,
    // root messages and recovery metadata, remain available even without a core map.
    emit(spool, w, json!({"type":"com.vercel.fx/history_turn"}), n)
}
fn tool(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let mut r = json!({"type":"tool_call","requestedBy":"model"});
    if let Some(id) = n["id"].as_str() {
        r["callId"] = json!(id);
        r["callIdSource"] = json!("native");
    } else {
        r["callId"] = json!(w.id());
        r["callIdSource"] = json!("derived");
    }
    if let Some(name) = n["name"].as_str() {
        r["name"] = json!(name);
    } else {
        r["capture"] = json!({"name":{"state":"omitted","reason":"unsupported"}});
    }
    if let Some(args) = n["arguments_json"].as_str() {
        r["inputText"] = json!(args);
    }
    emit(spool, w, r, n)
}

pub(super) fn state(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let p = &n["preferences"];
    let mut config = json!({"type":"config_change","snapshot":true,"options":{"fast_mode":p["fast_mode"],"permission_state":n["permission_state"]}});
    if p["model"].is_string() {
        config["model"] = json!({"id":p["model"],"provider":p["provider"]});
    }
    copy(&mut config, "effort", p, "effort");
    emit(spool, w, config, p)?;
    w.slot(10);
    if let Some(cwd) = n["workspace_root"].as_str() {
        emit(
            spool,
            w,
            json!({"type":"workspace_state","cwd":path(cwd,None)}),
            &json!({}),
        )?;
    }
    let u = &n["usage"];
    w.slot(20);
    let r = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":"history","usage":super::usage::usage(n,"total_input_tokens","total_output_tokens")});
    if ["total_input_tokens", "total_output_tokens"]
        .iter()
        .any(|key| n[key].as_u64().is_some_and(|v| v > 0))
    {
        emit(spool, w, r, &json!({}))?;
    }
    w.slot(30);
    if u.is_object() {
        emit(spool, w, super::usage::summary(u), u)?;
    }
    w.slot(40);
    emit(spool, w, json!({"type":"com.vercel.fx/session_state"}), n)
}

/// Immutable side-log facts retain event order, before the current-state tail.
pub(super) fn event(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let p = &n["payload"];
    let mut r = match n["kind"].as_str() {
        Some("session_started" | "preferences_changed") => {
            let initial = n["kind"] == "session_started";
            let p = if initial { &p["preferences"] } else { p };
            let mut r = json!({"type":"config_change"});
            if initial {
                r["snapshot"] = json!(true);
            }
            if let Some(model) = p["model"].as_str() {
                r["model"] = json!({"id":model});
                copy(&mut r["model"], "provider", p, "provider");
            }
            copy(&mut r, "effort", p, "effort");
            if let Some(fast) = p.get("fast_mode") {
                r["options"] = json!({"fast_mode":fast});
            }
            r
        }
        Some("workspace_rebound") => {
            let Some(cwd) = p["workspace_root"].as_str() else {
                return Ok(());
            };
            json!({"type":"workspace_state","cwd":path(cwd,None)})
        }
        Some("usage_checkpointed") => super::usage::summary(&p["usage"]),
        Some("history_turn_committed") => {
            json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"counter":"history","usage":super::usage::usage(p,"total_input_tokens","total_output_tokens")})
        }
        _ => return Ok(()),
    };
    if let Some(ts) = capture::millis(&n["timestamp_ms"]) {
        r["ts"] = json!(ts);
    }
    emit(spool, w, r, p)
}
