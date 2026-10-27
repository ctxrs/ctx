use super::{messages::parts, P};
use crate::{
    native as n,
    records::{copy, meta, path},
    spool::Spool,
    Result,
};
use rusqlite::params;
use serde_json::{json, Value};

pub(super) fn index(s: &Spool, e: &Value, pos: u64) -> Result<()> {
    let m = &e["message"];
    let calls = if m["customType"] == "openclaw.nested-tool.v1" {
        vec![
            json!({"id":m["details"]["toolCallId"],"name":m["details"]["toolName"],"arguments":m["details"]["input"]}),
        ]
    } else {
        m["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "toolCall")
            .cloned()
            .collect()
    };
    let mut local_ids = std::collections::HashSet::new();
    for call in calls {
        if let Some(id) = call["id"].as_str() {
            if !local_ids.insert(id.to_owned()) {
                return Err(crate::Error::Invalid(
                    "duplicate OpenClaw call identity within one entry".into(),
                ));
            }
            if e["supersedesEntryId"].is_null() {
                s.db.execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, pos])?;
            }
            let secret = call["name"] == "secrets"
                || call["arguments"]["secret"] == true
                || call["arguments"]["questions"]
                    .as_array()
                    .is_some_and(|qs| qs.iter().any(|q| q["secret"] == true));
            if secret {
                s.save(&format!("secret-call:{id}"), &json!(true))?;
                n::loss(s, "_ai.openclaw/secret_values")?;
            }
            if s.entity(&format!("call:{id}"))?.is_none() {
                let mut c = call.clone();
                let inf = m["responseId"].as_str().or(m["turnId"].as_str());
                if let Some(inf) = inf {
                    c["inferenceId"] = json!(inf);
                }
                s.save(&format!("call:{id}"), &c)?;
            }
        }
    }
    let details = &m["details"];
    if let Some(reviews) = details.get("approvalReviews") {
        for key in [details["approvalId"].as_str(), m["toolCallId"].as_str()]
            .into_iter()
            .flatten()
        {
            s.save(&format!("reviews:{key}"), reviews)?;
        }
    }
    Ok(())
}
pub(super) fn call(
    s: &Spool,
    c: &Value,
    inf: Option<&str>,
    by: &str,
    parent: Option<&Value>,
) -> Result<Value> {
    let Some(id) = c["id"].as_str() else {
        return Ok(json!({"type":"gap","reason":"unparseable"}));
    };
    let mut r = json!({"type":"tool_call","callId":id,"callIdSource":"native","name":c["name"],"requestedBy":by});
    if s.entity(&format!("secret-call:{id}"))?.is_some() {
        r["capture"] = json!({"input":{"state":"redacted","reason":"policy"}});
    } else {
        copy(&mut r, "input", c, "arguments");
    }
    if let Some(inf) = inf {
        r["inferenceId"] = json!(inf);
    }
    if let Some(p) = parent.filter(|v| !v.is_null()) {
        if let Some(id) = p.as_str() {
            if s.ambiguous_call(id)? {
                meta(&mut r, P, "parentToolCallId", p);
            } else {
                r["parentCallId"] = p.clone();
            }
        }
    }
    for key in ["async", "executionMode", "thoughtSignature"] {
        if let Some(v) = c.get(key) {
            meta(&mut r, P, key, v);
        }
    }
    Ok(r)
}
pub(super) fn result(s: &Spool, h: &Value, m: &Value, nested: bool) -> Result<Vec<Value>> {
    let id = m["toolCallId"].as_str();
    let d = &m["details"];
    let ambiguous = id
        .map(|id| s.ambiguous_call(id))
        .transpose()?
        .unwrap_or(false);
    let secret = id
        .map(|id| s.entity(&format!("secret-call:{id}")))
        .transpose()?
        .flatten()
        .is_some();
    let status = if d["openclawSyntheticMissingToolResult"] == true {
        "unknown"
    } else if d["reason"] == "policy-denied" || d["failureKind"] == "auto-review-denied" {
        "denied"
    } else if matches!(d["status"].as_str(), Some("running" | "approval-pending")) {
        "unknown"
    } else if m["isError"] == true {
        "failed"
    } else if m["isError"] == false {
        "completed"
    } else {
        "unknown"
    };
    let mut r = json!({"type":"tool_result","linkage":if ambiguous{"ambiguous"}else if id.is_some(){"exact"}else{"none"},"status":status});
    if let Some(id) = id {
        if ambiguous {
            meta(&mut r, P, "toolCallId", &json!(id));
        } else {
            r["callId"] = json!(id);
        }
    }
    if secret {
        r["capture"] = json!({"content":{"state":"redacted","reason":"policy"},"raw":{"state":"redacted","reason":"policy"}});
    } else {
        if nested {
            r["displayContent"] = json!(parts(&m["content"]));
            r["capture"]["content"] = json!({"state":"absent"});
        } else if let Some(content) = m.get("content") {
            r["content"] = json!(parts(content));
        } else {
            r["capture"]["content"] = json!({"state":"absent"});
        }
        if d.is_object() {
            r["raw"] = d.clone();
        }
        if d["persistedDetailsTruncated"] == true {
            r["capture"]["raw"] = json!({"state":"truncated","reason":"source_truncated"});
        }
        if d["mcpServer"].is_string() && d["mcpTool"].is_string() {
            r["provider"] = json!({"kind":"mcp","identity":"exact","server":d["mcpServer"],"tool":d["mcpTool"]});
        }
        copy(&mut r, "durationMs", d, "durationMs");
        let mut exec = json!({});
        if d["exitCode"].is_i64() {
            copy(&mut exec, "exitCode", d, "exitCode");
        }
        if d["exitSignal"].is_string() {
            copy(&mut exec, "signal", d, "exitSignal");
        }
        copy(&mut exec, "timedOut", d, "timedOut");
        copy(&mut exec, "output", d, "aggregated");
        if let Some(cwd) = d["cwd"].as_str() {
            exec["cwd"] = path(
                cwd,
                h.pointer("/workspace/roots/0/path").and_then(Value::as_str),
            );
        }
        if exec.as_object().is_some_and(|o| !o.is_empty()) {
            r["exec"] = exec;
        }
        if !ambiguous
            && id.is_some()
            && d["approvalId"].is_null()
            && s.entity(&format!("approval-call:{}", id.unwrap_or("")))?
                .is_none()
            && (d["approvalReviewOutcome"] == "approved"
                || d["failureKind"] == "auto-review-denied")
        {
            r["permission"] = json!({"decision":if d["approvalReviewOutcome"]=="approved"{"allow_once"}else{"reject_once"},"decidedBy":"classifier"});
        }
    }
    let mut out = vec![r];
    if secret {
        let mut q = json!({"type":"input_request","requestId":format!("secret-{}",id.unwrap_or("")),"idSource":"derived","mode":"form","secret":true,"capture":{"response":{"state":"redacted","reason":"policy"}}});
        if !ambiguous {
            copy(&mut q, "callId", m, "toolCallId");
        }
        if d["status"] == "answered" {
            q["status"] = json!("answered");
        }
        out.push(q);
    } else if let Some(id) = id.filter(|_| !ambiguous) {
        if let Some(c) = s.entity(&format!("call:{id}"))? {
            memory(&mut out, h, &c, d, status);
            question(&mut out, &c, m, status);
        }
    }
    Ok(out)
}
pub(super) fn nested(s: &Spool, h: &Value, m: &Value) -> Result<Vec<Value>> {
    let d = &m["details"];
    let c = json!({"id":d["toolCallId"],"name":d["toolName"],"arguments":d["input"]});
    let mut out = vec![call(s, &c, None, "code", d.get("parentToolCallId"))?];
    let mut result_message = d["result"].clone();
    result_message["toolCallId"] = d["toolCallId"].clone();
    copy(&mut result_message, "isError", d, "isError");
    n::set_time(&mut out[0], "ts", &d["startedAt"]);
    let mut results = result(s, h, &result_message, true)?;
    if let Some(r) = results.first_mut() {
        n::set_time(r, "ts", &d["timestamp"]);
        n::set_time(r, "startedAt", &d["startedAt"]);
        n::set_time(r, "completedAt", &d["timestamp"]);
    }
    out.extend(results);
    Ok(out)
}
pub(super) fn shell(m: &Value, id: &str) -> Result<Vec<Value>> {
    let call = format!("shell-{id}");
    let c = json!({"type":"tool_call","callId":call,"callIdSource":"derived","name":"bashExecution","requestedBy":"user","input":{"command":m["command"]}});
    let mut r = json!({"type":"tool_result","callId":call,"linkage":"derived","status":if m["cancelled"]==true{"cancelled"}else{"unknown"},"exec":{}});
    copy(&mut r["exec"], "command", m, "command");
    copy(&mut r["exec"], "output", m, "output");
    if m["exitCode"].is_i64() {
        copy(&mut r["exec"], "exitCode", m, "exitCode");
    }
    if m["excludeFromContext"] == true || m.get("output").is_none() {
        r["capture"] = json!({"content":{"state":"absent"}});
    } else {
        r["content"] = json!(parts(&m["output"]));
        if m["truncated"] == true {
            r["capture"] = json!({"content":{"state":"truncated","reason":"source_truncated"}});
        }
    }
    Ok(vec![c, r])
}
fn memory(out: &mut Vec<Value>, h: &Value, c: &Value, d: &Value, status: &str) {
    if !matches!(c["name"].as_str(), Some("write" | "edit")) {
        return;
    }
    let Some(cwd) = h.pointer("/workspace/roots/0/path").and_then(Value::as_str) else {
        return;
    };
    let Some(target) = c["arguments"]["path"]
        .as_str()
        .or(c["arguments"]["file_path"].as_str())
    else {
        return;
    };
    let target = target.strip_prefix(&format!("{cwd}/")).unwrap_or(target);
    if !["MEMORY.md", "USER.md", "DREAMS.md"].contains(&target)
        && !(target.starts_with("memory/")
            && target.ends_with(".md")
            && !target.contains("..")
            && target.len() >= 20)
    {
        return;
    }
    let mut r = json!({"type":"memory_change","store":"workspace","scope":if target=="USER.md"{"user"}else{"agent"},"op":if d["created"]==true{"add"}else{"update"},"target":{"path":path(target,Some(cwd))},"cause":{"callId":c["id"]}});
    if status == "completed" && d["changed"] == true {
        r["status"] = json!("applied");
    } else if matches!(status, "denied" | "failed") {
        r["status"] = json!("rejected");
    }
    out.push(r);
}
fn question(out: &mut Vec<Value>, c: &Value, m: &Value, status: &str) {
    if c["name"] != "ask_user" {
        return;
    }
    let qs:Vec<Value>=c["arguments"]["questions"].as_array().into_iter().flatten().enumerate().filter_map(|(i,q)|{
        let text=q["question"].as_str().or(q["text"].as_str())?;let mut r=json!({"id":q["id"].as_str().map(str::to_owned).unwrap_or_else(||i.to_string()),"text":text});
        for k in ["header","multiSelect","allowOther"]{copy(&mut r,k,q,k);}if let Some(opts)=q["options"].as_array(){r["options"]=json!(opts.iter().map(|o|if o.is_string(){json!({"label":o})}else{o.clone()}).collect::<Vec<_>>());}Some(r)
    }).collect();
    if qs.is_empty() {
        return;
    }
    let mut r = json!({"type":"input_request","requestId":format!("question-{}",c["id"].as_str().unwrap_or("")),"idSource":"derived","callId":c["id"],"mode":"questions","questions":qs});
    let d = &m["details"];
    match d["status"].as_str() {
        Some("answered") => r["status"] = json!("answered"),
        Some("no_answer") => r["status"] = json!("timed_out"),
        _ if status == "failed" => r["status"] = json!("cancelled"),
        _ => {}
    }
    if let Some(answers) = d["answers"].as_object() {
        r["answers"] = json!(answers
            .iter()
            .map(|(id, v)| {
                let mut a = json!({"questionId":id});
                if v.is_string() {
                    a["text"] = v.clone();
                } else if v.is_array() {
                    a["selected"] = v.clone();
                } else {
                    copy(&mut a, "text", v, "text");
                    copy(&mut a, "selected", v, "selected");
                }
                a
            })
            .collect::<Vec<_>>());
    }
    out.push(r);
}
