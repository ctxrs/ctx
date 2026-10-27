use crate::{
    content,
    records::{copy, meta, path, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
pub(super) fn summary(spool: &Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    let boundary = spool.entity("compaction:pending")?.unwrap_or(Value::Null);
    let metadata = &boundary["compactMetadata"];
    let mut r = json!({"type":"compaction","compactionId":boundary.get("uuid").cloned().unwrap_or(json!(writer.id())),"kind":"summary","summary":content::parts(&v["message"]["content"],Provider::Claude),"replaces":{"state":"unknown"}});
    if boundary.get("uuid").is_none() {
        r["idSource"] = json!("derived");
    }
    if let Some(parent) = boundary["logicalParentUuid"]
        .as_str()
        .map(|p| spool.output(p))
        .transpose()?
        .flatten()
    {
        r["replaces"] = json!({"throughId":parent});
        let mut tip = writer.tip.clone();
        let mut found = false;
        while let Some(node) = tip {
            if node == parent {
                found = true;
                break;
            }
            use rusqlite::OptionalExtension;
            tip = spool
                .db
                .query_row("SELECT parent FROM structure WHERE id=?1", [node], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .optional()?
                .flatten();
        }
        if !found {
            r["branchesFrom"] = json!({"record":parent});
        }
    }
    copy(&mut r, "durationMs", metadata, "durationMs");
    if let Some(trigger) = metadata["trigger"].as_str() {
        r["trigger"] = json!(if matches!(trigger, "auto" | "manual" | "model") {
            trigger.to_string()
        } else {
            format!("_com.anthropic.claude-code/{trigger}")
        });
    }
    meta(&mut r, Provider::Claude, "compactMetadata", metadata);
    super::structure::emit(spool, writer, r, v, None, false)?;
    Ok(())
}
pub(super) fn convert(
    spool: &Spool,
    writer: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    if super::context::attachment(spool, writer, v, cwd)? {
        return Ok(());
    }
    let a = &v["attachment"];
    let kind = a["type"].as_str().unwrap_or("unknown");
    match kind {
        "hook_success"
        | "hook_non_blocking_error"
        | "hook_blocking_error"
        | "hook_cancelled"
        | "hook_error_during_execution"
        | "async_hook_response" => hook(writer, v, a, None),
        "stop_hook_summary" => {
            for info in a["hookInfos"].as_array().into_iter().flatten() {
                hook(writer, v, info, Some("stop"))?;
            }
            Ok(())
        }
        "hook_additional_context" | "queued_command" => {
            let body = a
                .get("content")
                .or_else(|| a.get("prompt"))
                .or_else(|| a.get("additionalContext"))
                .unwrap_or(&Value::Null);
            let mut r = json!({"type":"message","messageId":v.get("uuid").cloned().unwrap_or(json!(writer.id())),"role":"user","content":content::parts(body,Provider::Claude),"author":if kind=="hook_additional_context" {"hook"}else{"harness"},"origin":"injected"});
            if v.get("uuid").is_none() {
                r["idSource"] = json!("derived");
            }
            if kind == "queued_command"
                && a.pointer("/origin/kind").and_then(Value::as_str) == Some("human")
            {
                r["author"] = json!("human");
                r["origin"] = json!("steer");
            }
            content::capture_content(&mut r, body);
            super::structure::emit(spool, writer, r, v, None, true)
        }
        "task_status" => {
            let Some(id) = a
                .get("taskId")
                .or_else(|| a.get("agentId"))
                .and_then(Value::as_str)
            else {
                return writer.vendor(v, "attachment");
            };
            let key = format!(
                "{}:{id}",
                if a["taskType"] == "local_agent" {
                    "subagent"
                } else {
                    "terminal"
                }
            );
            if let Some(mut r) = spool.entity(&key)? {
                r.as_object_mut().map(|o| o.remove("_record"));
                copy(&mut r, "status", a, "status");
                if r["type"] == "terminal"
                    && matches!(a["status"].as_str(), Some("completed" | "failed"))
                {
                    r["status"] = json!("exited");
                }
                writer.revise(spool, &key, r, v)?;
            } else {
                writer.vendor(v, "attachment")?;
            }
            Ok(())
        }
        "edited_text_file" => {
            if let Some(file) = a
                .get("filename")
                .or_else(|| a.get("filePath"))
                .and_then(Value::as_str)
            {
                let mut change = path(file, cwd);
                change["op"] = json!("modify");
                writer.emit(json!({"type":"file_change","status":"applied","precision":"unknown","cause":{"actor":"external","observed":true},"changes":[change]}),v)?;
            } else {
                writer.vendor(v, "attachment")?;
            }
            Ok(())
        }
        "instructions" => {
            for f in a
                .get("files")
                .or_else(|| a.get("instructions"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let kind = match f["type"].as_str() {
                    Some("User") => "user",
                    Some("Project") => "project",
                    Some("Local") => "local",
                    Some("Managed") => "managed",
                    Some("AutoMem") => "memory",
                    _ => "other",
                };
                let mut r = json!({"type":"instructions","kind":kind});
                if f.get("content").is_some() {
                    r["content"] = json!(content::parts(&f["content"], Provider::Claude));
                } else {
                    r["capture"] = json!({"content":{"state":"absent"}});
                }
                if let Some(file) = f["path"].as_str() {
                    r["source"] = path(file, cwd);
                }
                copy(&mut r, "truncated", f, "truncated");
                writer.emit(r, v)?;
            }
            Ok(())
        }
        _ => {
            let mut r = json!({"type":"message","messageId":v.get("uuid").cloned().unwrap_or(json!(writer.id())),"role":"user","author":"harness","origin":"injected","content":[{"type":"data","data":a,"schema":format!("com.anthropic.claude-code/attachment/{kind}")}],"capture":{"content":{"state":"truncated","reason":"source_truncated"}}});
            if v.get("uuid").is_none() {
                r["idSource"] = json!("derived");
            }
            super::structure::emit(spool, writer, r, v, None, true)
        }
    }
}
fn hook(writer: &mut Writer, v: &Value, a: &Value, event: Option<&str>) -> Result<()> {
    let event = event
        .or_else(|| a.get("hookEvent").and_then(Value::as_str))
        .unwrap_or("_com.anthropic.claude-code/unknown");
    let event = match event {
        "PreToolUse" => "pre_tool_use",
        "PostToolUse" => "post_tool_use",
        "Stop" => "stop",
        "SessionStart" => "session_start",
        "UserPromptSubmit" => "user_prompt_submit",
        e => e,
    };
    let mut r = json!({"type":"hook","event":event});
    for (dest, key) in [
        ("stdout", "stdout"),
        ("stderr", "stderr"),
        ("exitCode", "exitCode"),
        ("command", "command"),
        ("durationMs", "durationMs"),
        ("timedOut", "timedOut"),
        ("hookId", "hookId"),
        ("callId", "toolUseId"),
    ] {
        copy(&mut r, dest, a, key);
    }
    let status = match a["type"].as_str() {
        Some("hook_success") => Some("completed"),
        Some("hook_blocking_error") => Some("blocked"),
        Some("hook_cancelled") => Some("cancelled"),
        Some("hook_non_blocking_error" | "hook_error_during_execution") => Some("failed"),
        _ => None,
    };
    if let Some(status) = status {
        r["status"] = json!(status);
    }
    writer.emit(r, v)?;
    Ok(())
}
