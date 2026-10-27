use crate::{
    records::{copy, extras, meta, session_ref, timestamp},
    Provider,
};
use serde_json::{json, Value};
pub(super) fn make(provider: Provider, id: &str, n: &Value) -> Value {
    if provider.sqlite() {
        return crate::sqlite::header(provider, id, n);
    }
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":super::REVISION,"source":{"format":provider.format()},"lossy":["native","display_content","message.author","cost","exec.exit_code","provider.identity","revision_history","_rs.ctx/native_line_limit","_rs.ctx/nontranscript_state","_rs.ctx/vendor_only"]},"profiles":["core","coding","lineage","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    let version = if provider == Provider::Claude {
        "version"
    } else {
        "cli_version"
    };
    copy(&mut h["agent"], "version", n, version);
    if let Some(cwd) = n["cwd"].as_str().filter(|p| p.starts_with('/')) {
        if let Some(roots) = h["workspace"]["roots"].as_array_mut() {
            roots.push(json!({"id":"workspace","path":cwd}));
        }
    }
    let mut lineage = json!({});
    match provider {
        Provider::Claude => {
            if n["agentId"].as_str() == Some(id) {
                h["scope"] = json!("subagent");
                if let Some(parent) = n
                    .pointer("/_sidecar/parentAgentId")
                    .and_then(Value::as_str)
                    .or_else(|| n["sessionId"].as_str())
                {
                    lineage["spawnedBy"] =
                        json!({"session":session_ref(provider,parent),"evidence":"native_field"});
                    if let Some(call) = n.pointer("/_sidecar/toolUseId").and_then(Value::as_str) {
                        lineage["spawnedBy"]["record"] = json!(format!("callId={call}"));
                    }
                }
                copy(&mut lineage, "role", &n["_sidecar"], "agentType");
                meta(&mut h, provider, "agentMeta", &n["_sidecar"]);
            }
            if let Some(fork) = n.get("forkedFrom") {
                if let Some(parent) = fork["sessionId"].as_str() {
                    lineage["forkedFrom"] =
                        json!({"session":session_ref(provider,parent),"evidence":"native_field"});
                }
            }
            if let Some(parent) = n
                .pointer("/_forkRef/parentSessionId")
                .and_then(Value::as_str)
            {
                let from = session_ref(provider, parent);
                lineage["forkedFrom"] = json!({"session":from,"evidence":"native_field"});
                lineage["inherits"] =
                    json!({"mode":"referenced","from":from,"evidence":"native_field"});
                if let Some(last) = n
                    .pointer("/_forkRef/parentLastUuid")
                    .and_then(Value::as_str)
                {
                    lineage["inherits"]["throughId"] = json!(format!("messageId={last}"));
                }
            }
            if n["agentId"].as_str() == Some(id) {
                h["native"] = json!({"record":{"sessionId":n["sessionId"],"agentId":id}});
            }
            if let Some(slug) = n.get("slug") {
                meta(&mut h, provider, "slug", slug);
            }
            if let Some(client) = n["entrypoint"].as_str() {
                h["client"] = json!({"name":client});
            }
        }
        Provider::Codex => {
            if let Some(start) = n["timestamp"].as_str().and_then(timestamp) {
                h["startedAt"] = json!(start);
            }
            if let Some(client) = n["originator"].as_str() {
                h["client"] = json!({"name":client});
            }
            if let Some(trigger) = n["thread_source"].as_str() {
                h["trigger"] = json!({"native":trigger});
            }
            if let Some(parent) = n
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .and_then(Value::as_str)
            {
                h["scope"] = json!("subagent");
                lineage["spawnedBy"] =
                    json!({"session":session_ref(provider,parent),"evidence":"native_field"});
                copy(&mut lineage, "role", n, "agent_role");
            }
            if let Some(parent) = n["forked_from_id"].as_str() {
                lineage["forkedFrom"] =
                    json!({"session":session_ref(provider,parent),"evidence":"native_field"});
                if n["history_base"].is_null() {
                    lineage["inherits"] = json!({"mode":"copied","from":session_ref(provider,parent),"evidence":"native_structure"});
                }
                if let Some(boundary) = n["forked_from_ordinal_exclusive"]
                    .as_u64()
                    .and_then(|n| n.checked_sub(1))
                {
                    lineage["forkedFrom"]["ordinal"] = json!(boundary);
                }
            }
            if let Some(base) = n.get("history_base").filter(|v| !v.is_null()) {
                if let Some(owner) = n["_baseOwner"].as_str().filter(|owner| *owner != id) {
                    let from = session_ref(provider, owner);
                    lineage["inherits"] =
                        json!({"mode":"referenced","from":from,"evidence":"native_field"});
                    if let Some(end) = base["end_ordinal_exclusive"]
                        .as_u64()
                        .and_then(|n| n.checked_sub(1))
                    {
                        lineage["inherits"]["throughOrdinal"] = json!(end);
                        if n["source"].is_object() && lineage.get("forkedFrom").is_none() {
                            lineage["forkedFrom"] =
                                json!({"session":from,"ordinal":end,"evidence":"native_field"});
                        }
                    }
                } else if n["_sameThreadBase"] != true {
                    meta(&mut h, provider, "unresolvedHistoryBase", base);
                }
            }
            if lineage.as_object().is_some_and(|o| o.is_empty())
                && n["source"].is_string()
                && n["history_base"].is_null()
            {
                lineage["root"] = json!(true);
                lineage["inherits"] = json!({"mode":"none","evidence":"native_field"});
                h["scope"] = json!("primary");
            }
            for (name, keys) in [
                ("agent", vec!["agent_nickname", "agent_path", "session_id"]),
                ("creator", vec!["creator_user_id", "creator_account_id"]),
                (
                    "thread",
                    vec![
                        "memory_mode",
                        "history_mode",
                        "multi_agent_version",
                        "selected_capability_roots",
                        "context_window",
                    ],
                ),
            ] {
                let mut object = json!({});
                for k in keys {
                    copy(&mut object, k, n, k);
                }
                if object.as_object().is_some_and(|o| !o.is_empty()) {
                    meta(&mut h, provider, name, &object);
                }
            }
            extras(
                &mut h,
                provider,
                n,
                &[
                    "id",
                    "session_id",
                    "timestamp",
                    "cwd",
                    "cli_version",
                    "originator",
                    "source",
                    "thread_source",
                    "forked_from_id",
                    "forked_from_ordinal_exclusive",
                    "history_base",
                    "_baseOwner",
                    "_sameThreadBase",
                    "agent_nickname",
                    "agent_path",
                    "creator_user_id",
                    "creator_account_id",
                    "memory_mode",
                    "history_mode",
                    "multi_agent_version",
                    "selected_capability_roots",
                    "context_window",
                ],
            );
        }
        _ => {}
    }
    if lineage.as_object().is_some_and(|o| !o.is_empty()) {
        h["lineage"] = lineage;
    }
    h
}

pub(super) fn resolve(
    db: &rusqlite::Connection,
    provider: Provider,
    session: &str,
    n: &mut Value,
) -> crate::Result<()> {
    use rusqlite::OptionalExtension;
    if provider == Provider::Codex {
        if let Some(base) = n.pointer("/history_base/thread_id").and_then(Value::as_str) {
            let owner: Option<String> = db
                .query_row(
                    "SELECT session FROM sources WHERE agent='codex' AND segment=?1 LIMIT 1",
                    [base],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(owner) = owner {
                n["_baseOwner"] = json!(owner);
                if owner == session {
                    n["_sameThreadBase"] = json!(true);
                }
            }
        }
    }
    Ok(())
}

pub(super) fn copied_boundary(
    spool: &crate::spool::Spool,
    provider: Provider,
    session: &str,
    n: &Value,
) -> crate::Result<Option<u64>> {
    use rusqlite::OptionalExtension;
    if provider != Provider::Codex
        || !n["history_base"].is_null()
        || !n["forked_from_id"].is_string()
    {
        return Ok(None);
    }
    if let Some(end) = n["subagent_history_start_ordinal"]
        .as_u64()
        .or_else(|| n["forked_from_ordinal_exclusive"].as_u64())
    {
        return Ok(spool.db.query_row(
            "SELECT max(pos) FROM rows WHERE ordinal<?1",
            [end],
            |r| r.get(0),
        )?);
    }
    let owner: Option<u64> = spool
        .db
        .query_row(
            "SELECT pos FROM rows WHERE kind='session_meta' AND native_id=?1 ORDER BY pos LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    Ok(owner.and_then(|p| p.checked_sub(1)).filter(|p| *p > 0))
}
