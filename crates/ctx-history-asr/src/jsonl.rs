//! Shared boundary for the self-identifying Pi, Grok and DeepSeek JSONL families.
use crate::{
    records::{copy, extras, meta, session_ref, timestamp},
    spool::Spool,
    Provider, Result,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn session_id(provider: Provider, v: &Value) -> Option<String> {
    let id = match provider {
        Provider::Pi if v["kind"] == "header" && v["version"] == 4 => &v["id"],

        Provider::Pi if v["type"] == "session" && v["version"].as_u64().is_none_or(|v| v <= 3) => {
            &v["id"]
        }
        Provider::DeepSeekHarness
            if v["type"] == "session" && matches!(v["version"].as_u64(), Some(0 | 4)) =>
        {
            &v["id"]
        }
        Provider::GrokBuild
            if matches!(
                v["method"].as_str(),
                Some("session/update" | "_x.ai/session/update")
            ) =>
        {
            &v["params"]["sessionId"]
        }
        Provider::GrokBuild if v.get("update").is_some() => &v["sessionId"],
        _ => return None,
    };
    id.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
pub(crate) fn update(v: &Value) -> &Value {
    v.pointer("/params/update")
        .or_else(|| v.get("update"))
        .unwrap_or(v)
}
pub(crate) fn event_id(v: &Value) -> Option<&str> {
    v.pointer("/params/_meta/eventId")
        .or_else(|| v.pointer("/_meta/eventId"))
        .and_then(Value::as_str)
}
pub(crate) use crate::records::{clock, millis};
pub(crate) fn local(provider: Provider, v: &Value, physical: u64) -> String {
    let id = match provider {
        Provider::Pi if v["kind"] == "header" => Some("header".into()),
        Provider::Pi if v["kind"].is_string() => v["seq"].as_u64().map(|n| format!("seq:{n}")),
        Provider::Pi => v["id"].as_str().map(|s| format!("id:{s}")),
        Provider::GrokBuild => event_id(v).map(|s| format!("event:{s}")),
        Provider::DeepSeekHarness if v["type"] == "session" => Some("header".into()),
        Provider::DeepSeekHarness => v["seq"]
            .as_u64()
            .or_else(|| v["seq0"].as_u64())
            .map(|s| format!("seq:{s}")),
        _ => None,
    };
    id.unwrap_or_else(|| format!("l{physical}"))
}
pub(crate) fn index(spool: &Spool, provider: Provider, v: &Value, pos: u64) -> Result<()> {
    if provider == Provider::Pi {
        crate::pi::lanes::index(spool, v)?;
    }
    let calls: Vec<&str> = match provider {
        Provider::Pi => v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "toolCall")
            .filter_map(|p| p["id"].as_str())
            .collect(),
        Provider::GrokBuild if update(v)["sessionUpdate"] == "tool_call" => {
            update(v)["toolCallId"].as_str().into_iter().collect()
        }
        Provider::DeepSeekHarness if v["type"] == "assistant/message" => v
            .pointer("/data/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "tool-call")
            .filter_map(|p| p["id"].as_str())
            .collect(),
        Provider::DeepSeekHarness if v["type"] == "tool/call" => {
            v["data"]["callId"].as_str().into_iter().collect()
        }
        _ => Vec::new(),
    };
    for id in calls {
        if provider == Provider::DeepSeekHarness {
            let key = format!(
                "deepseek:index:{}:{}:{id}",
                v["data"]["turn"], v["data"]["step"]
            );
            if v["type"] == "tool/call" && spool.entity(&key)?.is_some() {
                continue;
            }
            spool.save(&key, &json!(pos))?;
        }
        spool
            .db
            .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, pos])?;
    }
    Ok(())
}
pub(crate) fn resolve(
    db: &rusqlite::Connection,
    provider: Provider,
    header: &mut Value,
) -> Result<()> {
    if provider != Provider::Pi {
        return Ok(());
    }
    let Some(parent) = header["parentSession"].as_str() else {
        return Ok(());
    };
    if !is_path(parent) {
        let path: Option<String> = db
            .query_row(
                "SELECT path FROM sources WHERE agent='pi' AND session=?1 ORDER BY path LIMIT 1",
                [parent],
                |r| r.get(0),
            )
            .optional()?;
        header["_parentId"] = json!(parent);
        if let Some(path) = path {
            header["_parentPath"] = json!(path);
        }
        return Ok(());
    }
    let id: Option<String> = db
        .query_row(
            "SELECT session FROM sources WHERE agent='pi' AND path=?1",
            [parent],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        header["_parentId"] = json!(id);
    }
    Ok(())
}
fn is_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s.ends_with(".jsonl")
}
/// Resolve a native path only by inspecting its session header. Never parse an ID from a filename.
pub(crate) fn parent_header(header: &mut Value, limit: usize) -> Result<String> {
    let Some(parent) = header["parentSession"].as_str().filter(|s| is_path(s)) else {
        return Ok(String::new());
    };
    let path = std::path::Path::new(parent);
    if !path.is_file() {
        return Ok(String::new());
    }
    let mut reader = crate::source::reader(path)?;
    let mut bytes = Vec::new();
    for _ in 0..64 {
        let Some((_, oversized)) = crate::source::line(&mut *reader, &mut bytes, limit)? else {
            break;
        };
        if oversized {
            continue;
        }
        if let Some(id) = crate::source::decode(&bytes).and_then(|v| session_id(Provider::Pi, &v)) {
            let m = path.metadata()?;
            header["_parentId"] = json!(id);
            return Ok(format!("{id}:{}:{:?}", m.len(), m.modified().ok()));
        }
    }
    Ok(String::new())
}
pub(crate) fn header(p: Provider, id: &str, n: &Value, revision: &str) -> Value {
    let losses = match p {
        Provider::Pi => vec![
            "message.author",
            "display_content",
            "provider.identity",
            "copies",
            "tool_result.content",
            "revision_history",
            "_io.github.badlogic.pi/request_context",
            "_io.github.badlogic.pi/branch_summary_extent",
            "_io.github.badlogic.pi/active_leaf",
            "_rs.ctx/native_line_limit",
            "_rs.ctx/vendor_only",
        ],
        Provider::GrokBuild => vec![
            "message.author",
            "inference",
            "usage",
            "tool_result.content",
            "copies",
            "_ai.x.grok-build/message_boundaries",
            "_ai.x.grok-build/nontranscript_state",
            "_ai.x.grok-build/tool_status_transitions",
            "reasoning",
            "_rs.ctx/native_line_limit",
            "_rs.ctx/vendor_only",
        ],
        _ => vec![
            "cost",
            "display_content",
            "content.binary",
            "provider.identity",
            "_com.deepseek.harness/streaming_chunks",
            "_com.deepseek.harness/request_context",
            "_rs.ctx/native_line_limit",
            "_rs.ctx/vendor_only",
        ],
    };
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":p.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":revision,"source":{"format":p.format()},"lossy":losses},"profiles":["core","coding","lineage","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    if let Some(version) = n["version"].as_u64() {
        h["producer"]["source"]["formatVersion"] = json!(version.to_string());
    }
    if let Some(cwd) = n["cwd"].as_str().filter(|p| p.starts_with('/')) {
        if let Some(roots) = h["workspace"]["roots"].as_array_mut() {
            roots.push(json!({"id":"workspace","path":cwd}));
        }
    }
    let start = if p == Provider::DeepSeekHarness || (p == Provider::Pi && n["version"] == 4) {
        millis(&n["createdAt"])
    } else if p == Provider::Pi {
        n["timestamp"].as_str().and_then(timestamp)
    } else {
        None
    };
    if let Some(start) = start {
        h["startedAt"] = json!(start);
    }
    if p == Provider::Pi {
        if n["kind"] == "header" {
            h["producer"]["source"]["format"] = json!("pi-jsonl-v4");
            h["producer"]["lossy"]
                .as_array_mut()
                .unwrap()
                .push(json!("_io.github.badlogic.pi/retained_tail_context"));
        }
        if let Some(parent) = n["_parentId"]
            .as_str()
            .or_else(|| n["parentSessionId"].as_str())
        {
            h["lineage"] =
                json!({"forkedFrom":{"session":session_ref(p,parent),"evidence":"native_field"}});
        }
    } else if p == Provider::DeepSeekHarness {
        if let Some(parent) = n["parentSession"].as_str() {
            h["lineage"] =
                json!({"forkedFrom":{"session":session_ref(p,parent),"evidence":"native_field"}});
            if n["origin"] == "subagent" {
                h["lineage"]["spawnedBy"] =
                    json!({"session":session_ref(p,parent),"evidence":"native_field"});
            }
            if let Some(seed) = n["seedLength"]
                .as_u64()
                .or_else(|| n["_seedLength"].as_u64())
            {
                h["lineage"]["inherits"] = if seed == 0 {
                    json!({"mode":"none","evidence":"native_field"})
                } else {
                    json!({"mode":"copied","from":session_ref(p,parent),"throughOrdinal":seed-1,"evidence":"native_field"})
                };
            }
        } else if n["seedLength"] == 0 || n["isSeeded"] == false {
            h["lineage"] = json!({"inherits":{"mode":"none","evidence":"native_field"}});
        }
        if n["origin"] == "subagent" {
            h["scope"] = json!("subagent");
        }
    }
    if p != Provider::GrokBuild {
        extras(
            &mut h,
            p,
            n,
            &[
                "id",
                "version",
                "cwd",
                "createdAt",
                "_parentId",
                "_parentPath",
                "_seedLength",
            ],
        );
    }
    h
}
pub(crate) fn finish_header(
    spool: &Spool,
    provider: Provider,
    n: &Value,
    h: &mut Value,
) -> Result<()> {
    if provider == Provider::DeepSeekHarness
        && h.pointer("/lineage/inherits/mode") == Some(&json!("copied"))
    {
        if let Some(seed) = n["seedLength"]
            .as_u64()
            .or_else(|| n["_seedLength"].as_u64())
        {
            if let Some(last) = spool.entity("deepseek:copied_last")? {
                h["lineage"]["inherits"]["localThroughId"] = last;
            } else if seed > 0 {
                meta(h, provider, "unavailableSeed", &json!(seed));
            }
        }
    }
    Ok(())
}
pub(crate) fn result_status(error: &Value) -> &'static str {
    match error.as_bool() {
        Some(true) => "failed",
        Some(false) => "completed",
        None => "unknown",
    }
}
pub(crate) fn usage(u: &Value, pi: bool) -> Value {
    let mut v = json!({});
    for (dest, source) in [
        ("uncached", if pi { "input" } else { "inputTokens" }),
        (
            "cacheRead",
            if pi { "cacheRead" } else { "cacheReadTokens" },
        ),
        (
            "cacheWrite",
            if pi { "cacheWrite" } else { "cacheWriteTokens" },
        ),
    ] {
        if let Some(n) = u[source].as_u64() {
            if v.get("input").is_none() {
                v["input"] = json!({});
            }
            v["input"][dest] = json!(n);
        }
    }
    for (dest, source) in [
        ("total", if pi { "output" } else { "outputTokens" }),
        (
            "reasoning",
            if pi { "reasoning" } else { "reasoningTokens" },
        ),
    ] {
        if let Some(n) = u[source].as_u64() {
            if v.get("output").is_none() {
                v["output"] = json!({});
            }
            v["output"][dest] = json!(n);
        }
    }
    if pi {
        copy(&mut v, "total", u, "totalTokens");
    }
    v
}
