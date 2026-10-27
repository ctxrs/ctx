//! Shared content syntax; provider semantics are supplied by each dialect.
use super::*;

pub(super) fn message(
    s: &Spool,
    w: &mut Writer,
    v: &Value,
    body: &Value,
    role: &str,
    id: Option<&str>,
) -> Result<()> {
    let content = super::parts(body, w.provider);
    if !content.is_empty() || body.is_null() {
        let mut r = json!({"type":"message","role":role});
        entity(&mut r, "messageId", id, format!("message:{}", w.id()));
        if body.is_null() {
            r["capture"] = json!({"content":{"state":"absent"}});
        } else {
            r["content"] = json!(content);
        }
        if role == "assistant" {
            r["author"] = json!("model");
        }
        retain(&mut r, w.provider, v, &["content", "message"]);
        if let Some(m) = v.get("message") {
            meta(&mut r, w.provider, "message", m);
        }
        emit(w, r, v)?;
    }
    for b in body.as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("tool_use") => {
                let mut r = json!({"type":"tool_call"});
                entity(
                    &mut r,
                    "callId",
                    b["id"].as_str(),
                    format!("call:{}", w.id()),
                );
                if role == "assistant" {
                    r["requestedBy"] = json!("model");
                }
                if let Some(name) = b["name"].as_str() {
                    r["name"] = json!(name);
                    identity(&mut r, w.provider, b, name);
                } else {
                    r["capture"]["name"] = json!({"state":"absent"});
                }
                input(&mut r, b.get("input"));
                retain(&mut r, w.provider, b, &["type", "id", "name", "input"]);
                // Keep the owning row even when it has no textual message.
                meta(&mut r, w.provider, "envelope", v);
                emit(w, r, v)?;
            }
            Some("tool_result") => {
                let mut r = result(
                    s,
                    b["tool_use_id"].as_str(),
                    status(b),
                    b.get("content"),
                    w.provider,
                )?;
                if let Some(raw) = v.get("toolUseResult") {
                    // One native sidecar can only be assigned when the row has one result.
                    let count = body
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|b| b["type"] == "tool_result")
                        .count();
                    if count == 1 {
                        r["raw"] = raw.clone();
                    }
                }
                retain(
                    &mut r,
                    w.provider,
                    b,
                    &["type", "tool_use_id", "content", "is_error"],
                );
                meta(&mut r, w.provider, "envelope", v);
                emit(w, r, v)?;
            }
            _ => {}
        }
    }
    Ok(())
}
pub(super) fn status(v: &Value) -> &'static str {
    match v["is_error"].as_bool() {
        Some(true) => "failed",
        Some(false) => "completed",
        None => "unknown",
    }
}
pub(super) fn identity(r: &mut Value, p: Provider, b: &Value, name: &str) {
    if let (Some(server), Some(tool)) = (b["mcpServerName"].as_str(), b["mcpToolName"].as_str()) {
        r["provider"] = json!({"kind":"mcp","identity":"exact","server":server,"tool":tool});
    } else if name.starts_with("mcp__") || (p == Provider::Copilot && name.starts_with("mcp_")) {
        r["provider"] = json!({"kind":"mcp","identity":"unknown"});
    }
    let kind = match p {
        Provider::Droid => match name {
            "Execute" => Some("execute"),
            "Read" => Some("read"),
            "Edit" | "Create" | "ApplyPatch" => Some("edit"),
            "Grep" | "Glob" | "LS" => Some("search"),
            "FetchUrl" | "WebSearch" => Some("fetch"),
            _ => None,
        },
        Provider::Copilot => match name {
            "bash" | "powershell" => Some("execute"),
            "view" => Some("read"),
            "edit" | "create" => Some("edit"),
            "grep" | "glob" => Some("search"),
            "web_fetch" | "web_search" => Some("fetch"),
            _ => None,
        },
        _ => None,
    };
    if let Some(kind) = kind {
        r["kind"] = json!(kind);
    }
}
pub(super) fn compaction(
    w: &mut Writer,
    v: &Value,
    summary: Option<&Value>,
    id: Option<&str>,
) -> Result<()> {
    let mut r = json!({"type":"compaction","kind":"summary","replaces":{"state":"unknown"}});
    entity(&mut r, "compactionId", id, format!("compaction:{}", w.id()));
    if let Some(summary) = summary {
        r["summary"] = json!(parts(summary, w.provider));
    } else {
        r["capture"] = json!({"summary":{"state":"absent"}});
    }
    for (to, from) in [("before", "tokensBefore"), ("after", "tokensAfter")] {
        if let Some(n) = v.get(from) {
            r["tokens"][to] = n.clone();
        }
    }
    retain(&mut r, w.provider, v, &[]);
    emit(w, r, v)?;
    Ok(())
}
