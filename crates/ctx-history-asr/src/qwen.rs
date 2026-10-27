use crate::{
    genai,
    records::{copy, extras, path, Writer},
    spool::Spool,
    tree, Provider, Result,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub(crate) fn header(spool: &mut Spool, h: &mut Value) -> Result<()> {
    let mut pos = 0;
    while let Some(next) = spool.next(pos)? {
        pos = next;
        let v = spool.get(pos)?;
        if v["subtype"] == "parent_session" {
            if let Some(p) = v
                .pointer("/systemPayload/parentSessionId")
                .and_then(Value::as_str)
            {
                h["_parentSession"] = json!(p);
            }
        }
        if h.get("forkedFrom").is_none() {
            if let Some(f) = v.get("forkedFrom") {
                h["forkedFrom"] = f.clone();
            }
        }
    }
    Ok(())
}
pub(crate) fn convert(spool: &Spool, w: &mut Writer, v: &Value, cwd: Option<&str>) -> Result<()> {
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), v)?;
        return Ok(());
    }
    if !matches!(
        v["type"].as_str(),
        Some("user" | "assistant" | "tool_result" | "system")
    ) {
        return w.vendor(v, v["type"].as_str().unwrap_or("unknown"));
    }
    let id = v["uuid"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message:{}", w.pos));
    if let Some(turn) = v.pointer("/goalContext/turnId").and_then(Value::as_str) {
        w.turn = Some(turn.to_owned());
    } else {
        w.turn = None;
    }
    workspace(spool, w, v, cwd)?;
    if v["type"] == "system" {
        return system(spool, w, v, &id);
    }
    let parts = v.pointer("/message/parts").unwrap_or(&v["content"]);
    let mut r =
        json!({"type":"message","messageId":id,"content":genai::parts(parts,Provider::QwenCode)});
    if !v["uuid"].is_string() {
        r["idSource"] = json!("derived");
        r["idScope"] = json!("capture");
    }
    match v
        .pointer("/message/role")
        .and_then(Value::as_str)
        .or_else(|| v["type"].as_str())
    {
        Some("model" | "assistant") => {
            r["role"] = json!("assistant");
            r["author"] = json!("model");
        }
        Some("user" | "tool_result") => {
            r["role"] = json!("user");
        }
        _ => r["capture"] = json!({"role":{"state":"absent"}}),
    }
    match v["provenance"].as_str() {
        Some("real_user") => r["author"] = json!("human"),
        Some("assistant_output") => r["author"] = json!("model"),
        Some("system" | "goal_control" | "goal_runtime") => {
            r["author"] = json!("harness");
            r["origin"] = json!("injected");
        }
        Some("tool_result") => r["author"] = json!("tool"),
        _ => {}
    }
    if let Some(display) = v.pointer("/systemPayload/displayText") {
        r["displayContent"] = json!(genai::parts(display, Provider::QwenCode));
        r["displayCorrespondence"] = json!("native");
    }
    extras(
        &mut r,
        Provider::QwenCode,
        v,
        &[
            "uuid",
            "parentUuid",
            "message",
            "content",
            "toolCallResult",
            "usageMetadata",
            "forkedFrom",
        ],
    );
    if let Some(message) = v.get("message") {
        extras(&mut r, Provider::QwenCode, message, &["role", "parts"]);
    }
    let mut first = true;
    if v["type"] != "tool_result" || !r["content"].as_array().unwrap().is_empty() {
        tree::emit(spool, w, r, v, None, true)?;
        first = false;
    }
    for (i, p) in parts.as_array().into_iter().flatten().enumerate() {
        if let Some(fc) = p.get("functionCall") {
            let mut call = genai::call(fc, &format!("call:{id}:{i}"), Provider::QwenCode);
            extras(&mut call, Provider::QwenCode, p, &["functionCall"]);
            tree::emit(spool, w, call, v, None, first)?;
            first = false;
        }
        if let Some(fr) = p.get("functionResponse") {
            let native = &v["toolCallResult"];
            let cid = fr["id"].as_str().or_else(|| native["callId"].as_str());
            let mut result =
                genai::result(spool, cid, Some(&json!([p])), native, Provider::QwenCode)?;
            extras(&mut result, Provider::QwenCode, fr, &["id", "response"]);
            extras(&mut result, Provider::QwenCode, p, &["functionResponse"]);
            tree::emit(spool, w, result, v, None, first)?;
            first = false;
        }
    }
    if first && v["type"] == "tool_result" {
        let n = &v["toolCallResult"];
        let r = genai::result(spool, n["callId"].as_str(), None, n, Provider::QwenCode)?;
        tree::emit(spool, w, r, v, None, true)?;
    }
    genai::usage(spool, w, v, &id)?;
    if let Some(size) = v["contextWindowSize"].as_u64() {
        w.emit(json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"contextWindow":{"size":size}}),&json!({}))?;
    }
    Ok(())
}
fn workspace(spool: &Spool, w: &mut Writer, v: &Value, cwd: Option<&str>) -> Result<()> {
    let mut r =
        json!({"type":"workspace_state","root":if cwd.is_some(){"workspace"}else{"unknown"}});
    if let Some(p) = v["cwd"].as_str() {
        r["cwd"] = path(p, cwd);
    }
    copy(&mut r, "branch", v, "gitBranch");
    if r.get("cwd").is_some() || r.get("branch").is_some() {
        genai::revision(spool, w, "workspace", r, v)?;
    }
    Ok(())
}
fn parent(spool: &Spool, v: &Value) -> Result<Option<Option<String>>> {
    let mut p = v["parentUuid"].as_str().map(str::to_owned);
    for _ in 0..100_000 {
        let Some(id) = p else { return Ok(Some(None)) };
        if let Some(r) = spool.output(&id)? {
            return Ok(Some(Some(r)));
        }
        let next: Option<Option<String>> = spool
            .db
            .query_row("SELECT parent FROM rows WHERE native_id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(next) = next else { return Ok(None) };
        p = next;
    }
    Ok(None)
}
fn system(spool: &Spool, w: &mut Writer, v: &Value, id: &str) -> Result<()> {
    let payload = &v["systemPayload"];
    match v["subtype"].as_str() {
        Some("rewind") => {
            if let Some(to) = parent(spool, v)? {
                let mut r = json!({"type":"rewind","to":to,"reason":"user_rewind"});
                extras(&mut r, Provider::QwenCode, v, &[]);
                w.emit(r, v)?;
            } else {
                w.emit(json!({"type":"gap","reason":"not_retained"}), v)?;
                w.vendor(v, "unresolved-rewind")?;
            }
        }
        Some("custom_title") => {
            let mut r = json!({"type":"session_info"});
            copy(&mut r, "title", payload, "customTitle");
            match payload["titleSource"].as_str() {
                Some("auto") => r["titleSource"] = json!("model"),
                Some("manual") => r["titleSource"] = json!("user"),
                _ => {}
            }
            extras(&mut r, Provider::QwenCode, v, &["systemPayload"]);
            extras(
                &mut r,
                Provider::QwenCode,
                payload,
                &["customTitle", "titleSource"],
            );
            genai::revision(spool, w, "title", r, v)?;
        }
        Some("chat_compression") => {
            if payload
                .pointer("/info/compressionStatus")
                .and_then(Value::as_u64)
                == Some(5)
            {
                return w.vendor(v, "chat_compression");
            }
            let mut r = json!({"type":"compaction","compactionId":id,"kind":"summary","replaces":{"state":"unknown"}});
            match payload
                .pointer("/info/compressionStatus")
                .and_then(Value::as_u64)
            {
                Some(1) => r["status"] = json!("completed"),
                Some(2 | 3 | 4 | 6) => r["status"] = json!("failed"),
                _ => {}
            }
            // compressedHistory includes a retained tail with no record IDs; do
            // not fabricate copies or call that entire array the summary.
            r["capture"] = json!({"summary":{"state":"absent"}});
            copy(
                &mut r["tokens"],
                "before",
                &payload["info"],
                "originalTokenCount",
            );
            copy(&mut r["tokens"], "after", &payload["info"], "newTokenCount");
            extras(&mut r, Provider::QwenCode, v, &[]);
            tree::emit(spool, w, r, v, None, true)?;
        }
        Some("agent_retry") => {
            let mut r = json!({"type":"state","state":"running"});
            if let Some(attempt) = payload["attempt"].as_u64().filter(|n| *n > 0) {
                r["attempt"] = json!({"n":attempt,"reason":"retry"});
            }
            extras(&mut r, Provider::QwenCode, v, &[]);
            w.emit(r, v)?;
        }
        _ => w.vendor(v, v["subtype"].as_str().unwrap_or("system"))?,
    }
    Ok(())
}
