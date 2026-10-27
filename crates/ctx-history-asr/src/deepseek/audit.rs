use super::*;
pub(super) fn convert(
    spool: &Spool,
    w: &mut Writer,
    v: &Value,
    time: &Value,
    cwd: Option<&str>,
) -> Result<bool> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("");
    let mut r = match kind {
        "session/title" => {
            let mut r = json!({"type":"session_info"});
            copy(&mut r, "title", d, "title");
            match d["source"]["kind"].as_str() {
                Some("user") => r["titleSource"] = json!("user"),
                Some("model") => r["titleSource"] = json!("model"),
                Some("fallback") => r["titleSource"] = json!("harness"),
                _ => {}
            }
            r
        }
        "approval/policy" => json!({"type":"config_change","permissionMode":d["policy"]}),
        "sandbox/mode" => json!({"type":"config_change","sandbox":{"mode":d["mode"]}}),
        "agent-preset/selected" => json!({"type":"config_change","agent":d["agentPreset"]}),
        "plan/mode" => {
            json!({"type":"config_change","mode":if d["active"]==true {"plan"}else{"act"}})
        }
        "goal/change" => {
            let g = &d["goal"];
            let id = g["id"].as_str().or_else(|| d["cleared"]["id"].as_str());
            let Some(id) = id else {
                return Ok(false);
            };
            let key = format!("deepseek:goal:{id}");
            let mut r = if d["operation"] == "clear" {
                let Some(mut r) = spool.entity(&key)? else {
                    return Ok(false);
                };
                r.as_object_mut().map(|o| o.remove("_record"));
                r["status"] = json!("_com.deepseek.harness/cleared");
                r
            } else {
                json!({"type":"plan","planId":id,"planType":"goal","status":g["phase"],"content":parts(&g["objective"])})
            };
            native(&mut r, v, &[]);
            w.revise(spool, &key, r, time)?;
            return Ok(true);
        }
        "todo/write" => {
            json!({"type":"plan","planId":"todos","idSource":"derived","planType":"items","entries":d["todos"]})
        }
        "approval/asked" => {
            let mut r = json!({"type":"permission","requestId":d["id"],"status":"pending"});
            if let Some(call) = d["callId"].as_str() {
                r["subject"] = json!({"callId":call});
            }
            copy(&mut r, "reason", d, "reason");
            r
        }
        "approval/decided" => {
            let Some(id) = d["id"].as_str() else {
                return Ok(false);
            };
            let mut r = spool
                .entity(&format!("deepseek:permission:{id}"))?
                .unwrap_or(json!({"type":"permission","requestId":id}));
            r.as_object_mut().map(|o| o.remove("_record"));
            let Some(outcome) = d["outcome"].as_str() else {
                return Ok(false);
            };
            r["status"] = json!("resolved");
            r["decision"] = json!(match outcome {
                "allowed-once" => "allow_once",
                "rejected" => "reject_once",
                "cancelled" => "cancelled",
                _ => "_com.deepseek.harness/unavailable",
            });
            r
        }
        "hook/invoked" | "hook/result" => {
            let event = match d["point"].as_str().unwrap_or("unknown") {
                "PreToolUse" => "pre_tool_use",
                "PostToolUse" => "post_tool_use",
                "Stop" => "stop",
                "UserPromptSubmit" => "user_prompt_submit",
                "SessionStart" => "session_start",
                other => other,
            };
            let mut r = json!({"type":"hook","event":event,"handlerKind":"command"});
            copy(&mut r, "name", d, "handlerId");
            if let Some(decision) = d["decision"].as_str() {
                r["decision"] = json!(match decision {
                    "allow" => "allow",
                    "deny" => "deny",
                    "ask" => "ask",
                    "pass" => "_com.deepseek.harness/pass",
                    "stop" => "_com.deepseek.harness/stop",
                    _ => decision,
                });
            }
            if kind == "hook/result" {
                for field in ["exitCode", "durationMs"] {
                    copy(&mut r, field, d, field);
                }
                copy(&mut r, "stderr", d, "stderrSummary");
                if d.get("stderrSummary").is_some() {
                    r["capture"] = json!({"stderr":{"state":"summarized"}});
                }
            }
            r
        }
        "compaction/start" | "compaction/end" | "step/end" | "session/end-seed" => {
            return Ok(false)
        }
        _ => return Ok(false),
    };
    native(&mut r, v, &[]);
    if kind.starts_with("approval/") && d["id"].is_string() {
        w.revise(
            spool,
            &format!("deepseek:permission:{}", d["id"].as_str().unwrap_or("")),
            r,
            time,
        )?;
    } else if kind == "todo/write" {
        w.revise(spool, "deepseek:todos", r, time)?;
    } else {
        w.emit(r, time)?;
    }
    let _ = cwd;
    Ok(true)
}
pub(super) fn diffs(w: &mut Writer, v: &Value, time: &Value, cwd: Option<&str>) -> Result<()> {
    let d = &v["data"];
    let m = &d["message"];
    let call = m["source"]["callId"].as_str();
    for diff in d
        .pointer("/meta/diffs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(file) = diff["path"].as_str() else {
            continue;
        };
        let mut change = path(file, cwd);
        change["op"] = json!("unknown");
        let mut r = json!({"type":"file_change","status":match m["content"][0]["isError"].as_bool(){Some(false)=>"applied",Some(true)=>"failed",None=>"unknown"},"precision":"snippet","changes":[change]});
        if let Some(call) = call {
            r["cause"] = json!({"callId":call});
        }
        meta(&mut r, P, "diff", diff);
        w.emit(r, time)?;
    }
    Ok(())
}
