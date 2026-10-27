mod attachments;
pub(crate) mod coding;
mod context;
mod request;
mod response;
use crate::tree as structure;
mod usage;
use crate::{
    content,
    records::{copy, meta, session_ref, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use serde_json::{json, Value};

pub(crate) fn convert(
    spool: &mut Spool,
    writer: &mut Writer,
    pos: u64,
    v: &Value,
    cwd: Option<&str>,
    subagent: bool,
) -> Result<()> {
    let kind = v["type"].as_str().unwrap_or("unknown");
    if let Some(turn) = v["promptId"].as_str() {
        writer.turn = Some(turn.into());
    }
    match kind {
        "assistant" if v.pointer("/message/id").and_then(Value::as_str).is_some() => {
            response::convert(spool, writer, pos, v)
        }
        "user" if v["isCompactSummary"] == true => attachments::summary(spool, writer, v),
        "user" => user(spool, writer, v, cwd, subagent),
        "attachment" => attachments::convert(spool, writer, v, cwd),
        "system" if v["subtype"] == "compact_boundary" => {
            spool.save("compaction:pending", v)?;
            Ok(())
        }
        "ai-title" | "custom-title" => {
            let title = v
                .get("aiTitle")
                .or_else(|| v.get("customTitle"))
                .or_else(|| v.get("title"));
            if let Some(title) = title {
                changed(
                    spool,
                    writer,
                    "title",
                    json!({"type":"session_info","title":title,"titleSource":if kind=="ai-title" {"model"}else{"user"}}),
                    v,
                )?;
            }
            Ok(())
        }
        "agent-name" | "agent-color" => {
            let key = if kind == "agent-name" {
                "agentName"
            } else {
                "agentColor"
            };
            if let Some(value) = v.get(key) {
                let mut record = json!({"type":"session_info"});
                meta(&mut record, Provider::Claude, key, value);
                changed(spool, writer, key, record, v)?;
            } else {
                writer.vendor(v, kind)?;
            }
            Ok(())
        }
        "last-prompt" => {
            if let Some(id) = v["leafUuid"]
                .as_str()
                .map(|id| spool.output(id))
                .transpose()?
                .flatten()
            {
                changed(
                    spool,
                    writer,
                    "heads",
                    json!({"type":"session_info","heads":[id]}),
                    v,
                )?;
            } else {
                writer.vendor(v, kind)?;
            }
            Ok(())
        }
        "mode" | "permission-mode" => {
            let key = if kind == "mode" {
                "mode"
            } else {
                "permissionMode"
            };
            if let Some(value) = v.get(key) {
                changed(
                    spool,
                    writer,
                    key,
                    json!({"type":"config_change",key:value}),
                    v,
                )?;
            } else {
                writer.vendor(v, kind)?;
            }
            Ok(())
        }
        "continued-in" => {
            if let Some(id) = v["continuedInSessionId"].as_str() {
                writer.emit(
                    json!({"type":"session_info","continuedIn":session_ref(Provider::Claude,id)}),
                    v,
                )?;
            }
            Ok(())
        }
        "api-request" | "api-request-blob" | "api-request-shape" => {
            request::record(spool, writer, v)
        }
        "file-history-snapshot" => coding::checkpoint(writer, v, cwd),
        "cost-state" => usage::summary(writer, v),
        "pr-link" => {
            let mut pr = json!({});
            for (dest, key) in [
                ("url", "prUrl"),
                ("number", "prNumber"),
                ("repository", "prRepository"),
            ] {
                copy(&mut pr, dest, v, key);
            }
            writer.emit(
                json!({"type":"vcs","action":"pr_link","evidence":"reported","pr":pr}),
                v,
            )?;
            Ok(())
        }
        "queue-operation" if v["operation"] == "remove" => {
            let mut m = json!({"type":"message","messageId":v.get("commandUuid").cloned().unwrap_or(json!(writer.id())),"role":"user","status":"withdrawn","content":content::parts(&v["content"],Provider::Claude)});
            if v.get("commandUuid").is_none() {
                m["idSource"] = json!("derived");
            }
            meta(&mut m, Provider::Claude, "queue", v);
            content::capture_content(&mut m, &v["content"]);
            writer.emit(m, v)?;
            Ok(())
        }
        "_gap" => {
            writer.emit(
                json!({"type":"gap","reason":v["reason"],"count":1}),
                &Value::Null,
            )?;
            Ok(())
        }
        "session_cron_carry" => writer.vendor(v, "scheduled_tasks"),
        "ended-by-model" => {
            writer.emit(json!({"type":"state","state":"ended","stopReason":"_com.anthropic.claude-code/ended_by_model"}),v)?;
            Ok(())
        }
        _ => writer.vendor(v, kind),
    }
}
fn changed(
    spool: &Spool,
    writer: &mut Writer,
    key: &str,
    value: Value,
    source: &Value,
) -> Result<()> {
    let key = format!("metadata:{key}");
    if spool.entity(&key)?.as_ref() != Some(&value) {
        writer.emit(value.clone(), source)?;
        spool.save(&key, &value)?;
    }
    Ok(())
}

fn user(
    spool: &mut Spool,
    writer: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
    subagent: bool,
) -> Result<()> {
    let source = &v["message"]["content"];
    let mut m = json!({"type":"message","messageId":v.get("uuid").cloned().unwrap_or(json!(writer.id())),"role":"user","content":content::parts(source,Provider::Claude)});
    if v.get("uuid").is_none() {
        m["idSource"] = json!("derived");
    }
    provenance(&mut m, v, subagent && spool.entity("first_user")?.is_none());
    trigger(&mut m, v, spool)?;
    for k in ["origin", "promptSource", "turnPosition", "turnCompanion"] {
        if let Some(value) = v.get(k) {
            meta(&mut m, Provider::Claude, k, value);
        }
    }
    if let Some(fork) = v.get("forkedFrom") {
        if let (Some(session), Some(id)) =
            (fork["sessionId"].as_str(), fork["messageUuid"].as_str())
        {
            m["copiedFrom"] = json!({"session":session_ref(Provider::Claude,session),"record":format!("messageId={id}"),"proof":"native_field"});
        }
    }
    let mut first = true;
    if !m["content"].as_array().is_some_and(Vec::is_empty) || !source.is_array() {
        content::capture_content(&mut m, source);
        structure::emit(spool, writer, m, v, None, true)?;
        first = false;
        spool.save("first_user", &json!(true))?;
    }
    for block in source.as_array().into_iter().flatten() {
        if block["type"] != "tool_result" {
            continue;
        }
        let Some(id) = block["tool_use_id"].as_str() else {
            writer.vendor(block, "tool_result")?;
            continue;
        };
        let raw = &v["toolUseResult"];
        let status = if v.get("toolDenialKind").is_some() {
            "denied"
        } else if block["is_error"] == true {
            "failed"
        } else if block["is_error"] == false {
            "completed"
        } else {
            "unknown"
        };
        let mut r = json!({"type":"tool_result","callId":id,"linkage":"exact","status":status});
        let ambiguous = spool.ambiguous_call(id)?;
        if ambiguous {
            r["linkage"] = json!("ambiguous");
        }
        if spool.entity(&format!("secret:{id}"))?.is_some() {
            r["capture"] = json!({"content":{"state":"redacted","reason":"policy"},"raw":{"state":"redacted","reason":"policy"}});
        } else {
            if !block["content"].is_null() {
                r["content"] = json!(content::parts(&block["content"], Provider::Claude));
            }
            copy(&mut r, "raw", v, "toolUseResult");
            content::capture_content(&mut r, &block["content"]);
        }
        if !ambiguous {
            coding::result(&mut r, spool, id, raw, cwd)?;
        }
        permission(
            &mut r,
            v.get("permissionDecision")
                .or_else(|| raw.get("permissionDecision")),
        );
        let response = if ambiguous {
            None
        } else {
            spool
                .entity(&format!("call:{id}"))?
                .and_then(|c| c["source"].as_str().map(str::to_owned))
        };
        structure::emit(spool, writer, r, v, response.as_deref(), first)?;
        first = false;
        tools::result_audit(writer, spool, v, id, raw, status, cwd)?;
        coding::audit(writer, v, id, raw, cwd)?;
    }
    if let Some(mode) = v.get("permissionMode") {
        changed(
            spool,
            writer,
            "permissionMode",
            json!({"type":"config_change","permissionMode":mode}),
            v,
        )?;
    }
    Ok(())
}
fn provenance(m: &mut Value, v: &Value, first_child: bool) {
    let text = v["message"]["content"].as_str().unwrap_or("");
    match v.pointer("/origin/kind").and_then(Value::as_str) {
        Some("human") => {
            m["author"] = json!("human");
        }
        Some("peer" | "coordinator" | "observer") => {
            m["author"] = json!("agent");
            m["origin"] = json!("inter_session");
            if let Some(session) = v["origin"]["fromSession"].as_str() {
                m["from"] = json!({"session":session_ref(Provider::Claude,session)});
            } else if let Some(address) = v["origin"]
                .get("from")
                .or_else(|| v["origin"].get("name"))
                .and_then(Value::as_str)
            {
                m["from"] = json!({"address":address});
            }
        }
        Some("task-notification") => {
            m["author"] = json!("harness");
            m["origin"] = json!("injected");
        }
        Some("channel") => {
            m["via"] = json!("channel");
        }
        _ if v["isMeta"] == true => {
            m["author"] = json!("harness");
            m["origin"] = json!("injected");
        }
        _ if first_child => {
            m["author"] = json!("agent");
        }
        _ => {}
    }
    if text.contains("<command-name>") {
        m["origin"] = json!("command");
    }
    if text.contains("<local-command-stdout>") {
        m["author"] = json!("harness");
    }
}
fn trigger(m: &mut Value, v: &Value, spool: &Spool) -> Result<()> {
    let Some(turn) = v["promptId"].as_str() else {
        return Ok(());
    };
    if spool.entity(&format!("turn:{turn}"))?.is_some() {
        return Ok(());
    }
    spool.save(&format!("turn:{turn}"), &json!(true))?;
    let fallback =
        if v.pointer("/origin/subkind").and_then(Value::as_str) == Some("scheduled-trigger") {
            "scheduled"
        } else if v.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification") {
            "task_notification"
        } else {
            ""
        };
    let origin = v["turnOrigin"].as_str().unwrap_or(fallback);
    let kind = match origin {
        "scheduled" => "schedule",
        "task_notification" => "task_completion",
        "auto_continuation" => "auto_continuation",
        "peer" => "peer",
        _ => return Ok(()),
    };
    let mut t = json!({"kind":kind,"native":origin});
    for (dest, key) in [
        ("id", "scheduledTaskId"),
        ("runId", "scheduledFireId"),
        ("reason", "fireReason"),
    ] {
        copy(&mut t, dest, v, key);
    }
    m["trigger"] = t;
    Ok(())
}
fn permission(result: &mut Value, decision: Option<&Value>) {
    let Some(d) = decision else {
        return;
    };
    let source = d["source"].as_str().unwrap_or("");
    let decision = match d["decision"].as_str() {
        Some("accept") if source == "user_permanent" => "allow_always",
        Some("accept") => "allow_once",
        Some("reject") => "reject_once",
        _ => return,
    };
    let mut p = json!({"decision":decision});
    let by = if d["reasonType"] == "classifier" {
        Some("classifier")
    } else {
        match source {
            "config" => Some("policy"),
            "user_temporary" | "user_permanent" | "user_reject" | "user_abort" => Some("user"),
            "hook" => Some("hook"),
            _ => None,
        }
    };
    if let Some(by) = by {
        p["decidedBy"] = json!(by);
    }
    result["permission"] = p;
    if let Some(reason) = d.get("reasonType") {
        meta(result, Provider::Claude, "permissionReasonType", reason);
    }
}
