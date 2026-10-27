use crate::{
    content,
    records::{copy, meta, path, session_ref, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use serde_json::{json, Value};
pub(super) fn convert(
    spool: &Spool,
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let kind = p["type"].as_str().unwrap_or("unknown");
    let id = p["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| writer.id());
    match kind {
        "CommandExecution" | "McpToolCall" | "FileChange" | "DynamicToolCall" => {
            execution(spool, writer, p, v, &id, kind, cwd)
        }
        "Plan" => {
            writer.revise(spool,&format!("plan:{id}"),json!({"type":"plan","planId":id,"planType":"document","content":content::parts(&p["text"],Provider::Codex)}),v)?;
            Ok(())
        }
        "AgentMessage" if p["delivery"] == "async" && p["questions"].is_array() => {
            let mut input = p.clone();
            input["blocking"] = json!(false);
            tools::question(writer, spool, v, &id, &input, None)
        }
        "AgentMessage" | "Reasoning" if spool.output(&id)?.is_some() => {
            // These are UI snapshots of the native model item, not another model message.
            writer.vendor(v, kind)
        }
        "UserMessage" | "AgentMessage" => {
            let mut display = p["content"].clone();
            if let Some(parts) = display.as_array_mut() {
                for p in parts {
                    if p["type"] == "image" && p["image_url"].is_string() {
                        *p = json!({"type":"resource_link","uri":p["image_url"],"name":p.get("name").unwrap_or(&p["image_url"])});
                    }
                    if p["type"] == "Text" {
                        p["type"] = json!("text");
                    }
                }
            }
            let mut r = json!({"type":"message","messageId":id,"role":if kind=="UserMessage"{"user"}else{"assistant"},"content":[],"displayContent":content::parts(&display,Provider::Codex),"capture":{"content":{"state":"absent"}},"audience":"user"});
            if kind == "AgentMessage" {
                r["author"] = json!("model");
            }
            if let Some(citation) = p.get("memory_citation") {
                meta(&mut r, Provider::Codex, "memoryCitation", citation);
            }
            writer.emit(r, v)?;
            Ok(())
        }
        "CollabAgentToolCall" => {
            invocation(spool, writer, p, v, &id)?;
            if p["tool"] != "spawn_agent" {
                return writer.vendor(v, kind);
            }
            for child in p["receiver_thread_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let mut r = json!({"type":"subagent","childSession":session_ref(Provider::Codex,child),"callId":id});
                if let Some(model) = p["model"].as_str() {
                    r["model"] = json!({"id":model});
                }
                writer.revise(spool, &format!("subagent:{child}"), r, v)?;
            }
            Ok(())
        }
        "SubAgentActivity" => {
            // These kinds use the invocation call_id in the native v2 writer.
            if matches!(
                p["kind"].as_str(),
                Some("started" | "interacted" | "interrupted")
            ) {
                invocation(spool, writer, p, v, &id)?;
            }
            let Some(child) = p["agent_thread_id"].as_str() else {
                return writer.vendor(v, kind);
            };
            let key = format!("subagent:{child}");
            let mut r = spool.entity(&key)?.unwrap_or(
                json!({"type":"subagent","childSession":session_ref(Provider::Codex,child)}),
            );
            r.as_object_mut().map(|o| o.remove("_record"));
            if p["kind"] == "started" && p["id"].is_string() {
                r["callId"] = json!(id);
            }
            if let Some(status) = match p["kind"].as_str() {
                Some("started") => Some("running"),
                Some("completed") => Some("completed"),
                Some("interrupted") => Some("stopped"),
                _ => None,
            } {
                r["status"] = json!(status);
            }
            if let Some(model) = p["model"].as_str() {
                r["model"] = json!({"id":model});
            }
            writer.revise(spool, &key, r, v)?;
            Ok(())
        }
        "ExitedReviewMode" => {
            writer.emit(json!({"type":"message","messageId":id,"role":"assistant","author":"agent","content":[{"type":"data","schema":"com.openai.codex/review_output","data":p["review_output"]}]}),v)?;
            Ok(())
        }
        _ => writer.vendor(v, kind),
    }
}
// Native UI records prove invocations even for nested code tools without a
// response_item request. Neither complete arguments nor requester is recorded.
fn invocation(spool: &Spool, writer: &mut Writer, p: &Value, v: &Value, id: &str) -> Result<()> {
    if !p["id"].is_string()
        || spool.entity(&format!("call:{id}"))?.is_some()
        || spool.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_calls WHERE call_id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(());
    }
    let mut call = tools::call(
        writer,
        spool,
        p,
        id,
        p["tool"].as_str(),
        tools::Arguments::Json(Value::Null),
        "code",
    )?;
    call.as_object_mut().unwrap().remove("requestedBy");
    call["capture"]["input"] = json!({"state":"absent"});
    call["native"] = json!({"record":p});
    writer.emit(call, v)?;
    Ok(())
}

fn execution(
    spool: &Spool,
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    id: &str,
    kind: &str,
    cwd: Option<&str>,
) -> Result<()> {
    let known = spool.entity(&format!("call:{id}"))?.is_some()
        || spool.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_calls WHERE call_id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )?;
    let provider = if kind == "McpToolCall" {
        Some(if p["connectorId"].as_str().is_some() {
            let mut provider = json!({"kind":"connector","identity":"exact"});
            copy(&mut provider, "name", p, "appName");
            provider
        } else {
            json!({"kind":"mcp","server":p["server"],"tool":p["tool"],"identity":"exact"})
        })
    } else {
        None
    };
    if !known {
        let input = match kind {
            "CommandExecution" => json!({"command":p["command"],"cwd":p["cwd"]}),
            "FileChange" => p["changes"].clone(),
            _ => p["arguments"].clone(),
        };
        let mut r = tools::call(
            writer,
            spool,
            p,
            id,
            None,
            tools::Arguments::Json(input),
            if p["source"] == "user_shell" {
                "user"
            } else {
                "code"
            },
        )?;
        if let Some(provider) = &provider {
            r["provider"] = provider.clone();
        }
        if let Some(cell) = p
            .get("cell_id")
            .or_else(|| p.get("cellId"))
            .and_then(Value::as_str)
        {
            meta(&mut r, Provider::Codex, "cellId", &json!(cell));
        }
        let mut connector = json!({});
        for k in [
            "connectorId",
            "readOnlyHint",
            "mcpAppUi",
            "linkId",
            "actionName",
        ] {
            copy(&mut connector, k, p, k);
        }
        if connector.as_object().is_some_and(|o| !o.is_empty()) {
            meta(&mut r, Provider::Codex, "connector", &connector);
        }
        writer.emit(r, v)?;
    }
    let status = match p["status"].as_str() {
        Some("completed") => "completed",
        Some("failed") => "failed",
        Some("declined") => "denied",
        _ => "unknown",
    };
    let mut r = json!({"type":"tool_result","callId":id,"linkage":"exact","status":status,"capture":{"content":{"state":"absent"}}});
    if let Some(provider) = provider {
        r["provider"] = provider;
    }
    if kind == "CommandExecution" {
        let mut exec = json!({});
        for (dest, key) in [
            ("command", "command"),
            ("exitCode", "exit_code"),
            ("stdout", "stdout"),
            ("stderr", "stderr"),
            ("output", "aggregated_output"),
            ("processId", "process_id"),
        ] {
            copy(&mut exec, dest, p, key);
        }
        if let Some(directory) = p["cwd"].as_str() {
            if let Some(path) = super::paths::reference(directory, cwd) {
                exec["cwd"] = path;
            } else {
                meta(&mut r, Provider::Codex, "cwdUri", &json!(directory));
            }
        }
        r["exec"] = exec;
        let mut command = json!({});
        for k in [
            "parsed_cmd",
            "plugin_id",
            "script_path",
            "interaction_input",
        ] {
            copy(&mut command, k, p, k);
        }
        meta(&mut r, Provider::Codex, "command", &command);
        if p["aggregated_output"]
            .as_str()
            .is_some_and(|s| s.contains("truncated"))
        {
            r["capture"]["exec.output"] = json!({"state":"truncated","reason":"source_truncated"});
        }
    } else if kind == "McpToolCall" {
        copy(&mut r, "raw", p, "result");
        copy(&mut r, "error", p, "error");
    } else if kind == "DynamicToolCall" {
        copy(&mut r, "raw", p, "content_items");
        if let Some(error) = p["error"].as_str() {
            r["error"] = json!({"message":error});
        }
    } else if kind == "FileChange" {
        if let Some(value) = p.get("auto_approved") {
            meta(&mut r, Provider::Codex, "autoApproved", value);
        }
    }
    if let Some(duration) = p["duration"].as_object() {
        let secs = duration.get("secs").and_then(Value::as_u64);
        let nanos = duration.get("nanos").and_then(Value::as_u64);
        if let (Some(s), Some(n)) = (secs, nanos) {
            r["durationMs"] = json!(s as f64 * 1000.0 + n as f64 / 1e6);
        }
    }
    tools::result(writer, spool, id, r, v)?;
    if kind == "FileChange" {
        changes(writer, p, v, id, status, cwd)?;
    }
    Ok(())
}
fn changes(
    writer: &mut Writer,
    p: &Value,
    v: &Value,
    id: &str,
    status: &str,
    cwd: Option<&str>,
) -> Result<()> {
    for (file, change) in p["changes"].as_object().into_iter().flatten() {
        let (op, detail) = if let Some(kind) = change["type"].as_str() {
            (kind, change)
        } else {
            change
                .as_object()
                .and_then(|o| o.iter().next())
                .map(|(k, v)| (k.as_str(), v))
                .unwrap_or(("unknown", change))
        };
        let mut c = path(file, cwd);
        c["op"] = json!(match op {
            "add" => "add",
            "delete" => "delete",
            _ => "modify",
        });
        let mut precision = "unknown";
        if op == "add" && detail["content"].is_string() {
            c["after"] = json!({"text":detail["content"]});
            precision = "exact";
        } else if op == "delete" {
            precision = "exact";
        }
        let mut r = json!({"type":"file_change","cause":{"callId":id},"status":match status{"completed"=>"applied","failed"=>"failed","denied"=>"skipped",_=>"unknown"},"precision":precision,"changes":[c]});
        if let Some(diff) = detail["unified_diff"].as_str() {
            r["precision"] = json!("hunks");
            r["patch"] = json!({"format":"unified","text":diff});
        }
        if let Some(dest) = detail["move_path"].as_str() {
            let dest = path(dest, cwd);
            r["changes"][0]["oldPath"] = r["changes"][0]["path"].clone();
            r["changes"][0]["oldRoot"] = r["changes"][0]["root"].clone();
            r["changes"][0]["path"] = dest["path"].clone();
            r["changes"][0]["root"] = dest["root"].clone();
            r["changes"][0]["op"] = json!("move");
        }
        writer.emit(r, v)?;
    }
    Ok(())
}
