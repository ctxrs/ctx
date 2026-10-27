//! Native CLI journals. Each dialect owns its identities and semantic projection.
mod blocks;
mod copilot;
mod droid;
mod fold;
mod kimi;
mod qoder;
pub(crate) mod source;

use crate::{
    content,
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::params;
use serde_json::{json, Value};

pub(crate) fn convert(
    s: &mut Spool,
    w: &mut Writer,
    pos: u64,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), v)?;
        return Ok(());
    }
    match w.provider {
        Provider::Copilot => copilot::convert(s, w, pos, v, cwd),
        Provider::Droid => droid::convert(s, w, pos, v, cwd),
        Provider::Kimi => kimi::convert(s, w, pos, v),
        Provider::Qoder => qoder::convert(s, w, pos, v, cwd),
        _ => Ok(()),
    }
}

pub(crate) fn local(p: Provider, v: &Value, line: u64) -> String {
    // Droid retry result rows can reuse id with parentId == id. Do not overwrite
    // the original message or another retry merely because its id matches.
    let id = match p {
        Provider::Copilot => v["id"].as_str(),
        Provider::Qoder => v["uuid"].as_str().or_else(|| v["id"].as_str()),
        _ => None,
    };
    id.map(|id| format!("event:{id}"))
        .unwrap_or_else(|| format!("l{line}"))
}

pub(crate) fn index(s: &Spool, p: Provider, v: &Value, pos: u64) -> Result<()> {
    // An upsert of a native observation replaces its indexes as well as its bytes.
    let replacing: bool = s.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM rows WHERE pos=?1)",
        [pos],
        |r| r.get(0),
    )?;
    if replacing {
        let start = pos * crate::records::STRIDE;
        let end = start + crate::records::STRIDE;
        s.db.execute(
            "DELETE FROM native_calls WHERE position>=?1 AND position<?2",
            params![start, end],
        )?;
        s.db.execute(
            "DELETE FROM journal_calls WHERE position>=?1 AND position<?2",
            params![start, end],
        )?;
        s.db.execute("DELETE FROM journal_parts WHERE position=?1", [pos])?;
    }
    let mut calls = Vec::new();
    match p {
        Provider::Copilot if v["type"] == "tool.execution_start" => {
            if let Some(id) = v.pointer("/data/toolCallId").and_then(Value::as_str) {
                s.db.execute(
                    "INSERT OR IGNORE INTO journal_calls VALUES('start',?1,?2)",
                    params![id, pos * crate::records::STRIDE],
                )?;
            }
        }
        Provider::Copilot if v["type"] == "assistant.message" => {
            for (i, req) in v["data"]["toolRequests"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(id) = req["toolCallId"].as_str() {
                    s.db.execute(
                        "INSERT OR IGNORE INTO journal_calls VALUES('request',?1,?2)",
                        params![id, pos * crate::records::STRIDE + i as u64],
                    )?;
                }
            }
        }
        Provider::Droid | Provider::Qoder => {
            for b in v
                .get("content")
                .or_else(|| v.pointer("/message/content"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if b["type"] == "tool_use" {
                    calls.extend(b["id"].as_str());
                }
            }
        }
        Provider::Kimi if v["type"] == "context.append_loop_event" => {
            let e = &v["event"];
            if e["type"] == "tool.call" {
                calls.extend(e["toolCallId"].as_str());
            }
        }
        _ => {}
    }
    let group = match p {
        Provider::Kimi if v["event"]["type"] == "content.part" => v["event"]["stepUuid"].as_str(),
        Provider::Copilot
            if v["type"] == "assistant.message" && v["data"]["chunkIndex"].is_number() =>
        {
            v["data"]["messageId"].as_str()
        }
        _ => None,
    };
    if let Some(group) = group {
        s.db.execute(
            "INSERT OR REPLACE INTO journal_parts VALUES(?1,?2)",
            params![group, pos],
        )?;
    }
    if p == Provider::Kimi && v["type"] == "context.append_message" {
        for call in v["message"]["toolCalls"].as_array().into_iter().flatten() {
            calls.extend(call["id"].as_str());
        }
    }
    for (i, id) in calls.into_iter().enumerate() {
        s.db.execute(
            "INSERT INTO native_calls VALUES(?1,?2)",
            params![id, pos * crate::records::STRIDE + i as u64],
        )?;
    }
    Ok(())
}

pub(crate) fn header(p: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":p.agent()},"sessionId":id,
        "producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":crate::bundle::REVISION,"source":{"format":p.format()},
        "lossy":["message.author","cost","provider.identity","revision_history","file_change","branches","content.binary","_rs.ctx/native_line_limit","_rs.ctx/nontranscript_state","_rs.ctx/vendor_only"]},
        "profiles":["core","coding","lineage","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    if let Some(cwd) = n["cwd"]
        .as_str()
        .filter(|s| s.starts_with('/') || s.as_bytes().get(1) == Some(&b':'))
    {
        if let Some(a) = h["workspace"]["roots"].as_array_mut() {
            a.push(json!({"id":"workspace","path":cwd.replace('\\',"/")}));
        }
    }
    match p {
        Provider::Copilot => {
            let d = &n["data"];
            if let (Some(namespace), Some(agent)) =
                (n["_nativeSession"].as_str(), n["_agent"].as_str())
            {
                h["sessionId"] = json!(agent);
                h["namespace"] = json!(namespace);
                h["scope"] = json!("subagent");
                h["lineage"]["spawnedBy"] = json!({"session":crate::records::session_ref(p,namespace),"evidence":"native_structure"});
            }
            copy(&mut h["agent"], "version", d, "copilotVersion");
            if let Some(ts) = d["startTime"]
                .as_str()
                .filter(|_| n.get("_agent").is_none())
                .and_then(crate::records::timestamp)
            {
                h["startedAt"] = json!(ts);
            }
            if let Some(parent) = d["detachedFromSpawningParentSessionId"]
                .as_str()
                .filter(|_| n.get("_agent").is_none())
            {
                h["lineage"] = json!({"continues":{"session":crate::records::session_ref(p,parent),"reason":"resume","evidence":"native_field"}});
            }
        }
        Provider::Droid => {
            if let Some(parent) = n["parent"]
                .as_str()
                .or_else(|| n["callingSessionId"].as_str())
            {
                h["lineage"] = json!({"spawnedBy":{"session":crate::records::session_ref(p,parent),"evidence":"native_field"}});
                h["scope"] = json!("subagent");
            }
            if n["decompSessionType"] == "worker" {
                h["scope"] = json!("subagent");
            }
            if let Some(role) = n["decompSessionType"].as_str() {
                if h.get("lineage").is_none() {
                    h["lineage"] = json!({});
                }
                h["lineage"]["role"] = json!(role);
            }
            copy(&mut h["agent"], "version", n, "version");
            if let Some(ts) = n["timestamp"].as_str().and_then(crate::records::timestamp) {
                h["startedAt"] = json!(ts);
            }
        }
        Provider::Kimi => kimi::header(&mut h, n),
        _ => {}
    }
    if matches!(p, Provider::Droid | Provider::Qoder) {
        h["producer"]["lossy"]
            .as_array_mut()
            .unwrap()
            .push(json!("inference"));
    }
    h
}

/// Preserve native facts once on the nearest projection, including invalid or
/// future values. A grouped key avoids illegal _meta keys from arbitrary JSON.
fn retain(r: &mut Value, p: Provider, v: &Value, covered: &[&str]) {
    if p == Provider::Kimi {
        r["native"] = json!({"record":v});
        return;
    }
    let mut rest = v.clone();
    if let Some(o) = rest.as_object_mut() {
        for k in covered {
            o.remove(*k);
        }
        o.remove("timestamp");
        if !o.is_empty() {
            meta(r, p, "remainingFields", &rest);
        }
    }
}
fn entity(r: &mut Value, key: &str, native: Option<&str>, derived: String) {
    r[key] = json!(native.unwrap_or(&derived));
    r[if key == "callId" {
        "callIdSource"
    } else {
        "idSource"
    }] = json!(if native.is_some() {
        "native"
    } else {
        "derived"
    });
}
fn input(r: &mut Value, v: Option<&Value>) {
    if let Some(v) = v {
        if let Some(text) = v.as_str() {
            if let Some(parsed) = ctx_history_asr_core::numbers::decode(text) {
                r["input"] = parsed;
            } else {
                r["inputText"] = json!(text);
            }
        } else {
            r["input"] = v.clone();
        }
    } else {
        r["capture"]["input"] = json!({"state":"absent"});
    }
}
fn result(
    s: &Spool,
    id: Option<&str>,
    status: &str,
    body: Option<&Value>,
    p: Provider,
) -> Result<Value> {
    let mut r = json!({"type":"tool_result","linkage":"none","status":status});
    if let Some(id) = id {
        if s.ambiguous_call(id)? {
            r["linkage"] = json!("ambiguous");
            meta(&mut r, p, "nativeCallId", &json!(id));
        } else {
            r["callId"] = json!(id);
            r["linkage"] = json!("exact");
        }
    }
    if let Some(body) = body.filter(|b| !b.is_null()) {
        r["content"] = json!(parts(body, p));
        if !p.model_result() {
            r["raw"] = body.clone();
        }
    } else {
        r["capture"] = json!({"content":{"state":"absent"}});
    }
    Ok(r)
}
fn parts(v: &Value, p: Provider) -> Vec<Value> {
    if p == Provider::Kimi {
        if let Some(a) = v.as_array() {
            return a
                .iter()
                .flat_map(|b| match b["type"].as_str() {
                    Some("think") => {
                        let mut r =
                            json!({"type":"reasoning","visibility":"full","text":b["think"]});
                        copy(&mut r, "signature", b, "encrypted");
                        vec![r]
                    }
                    Some("image_url" | "audio_url" | "video_url") => {
                        let field = match b["type"].as_str() {
                            Some("image_url") => "imageUrl",
                            Some("audio_url") => "audioUrl",
                            _ => "videoUrl",
                        };
                        if let Some(url) = b[field]["url"].as_str() {
                            vec![json!({"type":"resource_link","name":field,"uri":url})]
                        } else {
                            vec![json!({"type":"data","data":b})]
                        }
                    }
                    _ => content::parts(&json!([b]), p),
                })
                .collect();
        }
    }
    if v.is_string() || v.is_array() {
        content::parts(v, p)
    } else {
        vec![json!({"type":"data","data":v,"schema":format!("{}/content",p.domain())})]
    }
}
fn emit(w: &mut Writer, r: Value, source: &Value) -> Result<String> {
    w.emit(r, source)
}
