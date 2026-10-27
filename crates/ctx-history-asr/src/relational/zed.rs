//! Zed thread format 0.3.0 at zed-industries/zed@eb5483528be607b497afdaad0a237bebeb191249; native no-model persistence also exercised with v1.17.2.
use super::{common as c, reference, sqlite};
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Read;
const P: Provider = Provider::Zed;
pub(super) fn ingest(db: &Connection, s: &mut Spool, h: &mut Value) -> Result<()> {
    use base64::Engine;
    let id = h["_sessionId"].as_str().unwrap_or("");
    let row = sqlite::one(db, "threads", "id", id, &[], s.limit)?;
    let bytes = if let Some(b64) = row["data"]["data"].as_str() {
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    } else {
        row["data"].as_str().map(|s| s.as_bytes().to_vec())
    };
    let decoded = bytes.and_then(|bytes| match row["data_type"].as_str() {
        Some("json") => Some(bytes),
        Some("zstd") => {
            let mut decoder = zstd::stream::read::Decoder::new(bytes.as_slice()).ok()?;
            let mut out = Vec::new();
            decoder
                .by_ref()
                .take(s.limit as u64 + 1)
                .read_to_end(&mut out)
                .ok()?;
            (out.len() <= s.limit).then_some(out)
        }
        _ => None,
    });
    let thread = decoded
        .as_deref()
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(c::parse);
    let Some(mut thread) = thread else {
        return s.push_native("gap","thread",&json!({"reason":if row.get("_oversized").is_some(){"conversion_loss"}else{"unparseable"},"source":row}),true);
    };
    if thread["version"] != "0.3.0" {
        return s.push_native(
            "gap",
            "thread",
            &json!({"reason":"conversion_loss","source":thread}),
            true,
        );
    }
    if let Some(messages) = thread
        .as_object_mut()
        .and_then(|o| o.remove("messages"))
        .and_then(|v| v.as_array().cloned())
    {
        let mut seen = std::collections::BTreeMap::<String, usize>::new();
        for n in &messages {
            if let Some(id) = n["User"]["id"].as_str() {
                *seen.entry(id.to_owned()).or_default() += 1;
            }
        }
        for (i, n) in messages.iter().enumerate() {
            let calls: Vec<_> = n["Agent"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b.get("ToolUse"))
                .cloned()
                .collect();
            c::index_calls(s, &json!(calls))?;
            for (id, _) in n["Agent"]["tool_results"].as_object().into_iter().flatten() {
                c::index_result(s, Some(id))?;
            }
            let id = n["User"]["id"]
                .as_str()
                .filter(|id| seen.get(*id) == Some(&1));
            s.push_native(
                "message",
                &id.map(|id| format!("user:{id}"))
                    .unwrap_or_else(|| format!("messages:{i}")),
                n,
                id.is_none(),
            )?;
        }
    }
    h["_thread"] = thread;
    s.push_native("session", "session", h, false)
}
pub(super) fn header(h: &Value, v: &mut Value) {
    v["profiles"].as_array_mut().unwrap().push(json!("coding"));
    let thread = &h["_thread"];
    if let Some(version) = thread["version"].as_str() {
        v["producer"]["source"]["formatVersion"] = json!(version);
    }
    if let Some(t) = c::time(&h["created_at"]) {
        v["startedAt"] = json!(t);
    }
    if let Some(parent) = thread["subagent_context"]["parent_thread_id"]
        .as_str()
        .or_else(|| h["parent_id"].as_str())
    {
        v["lineage"] =
            json!({"spawnedBy":{"session":reference(P,h,parent),"evidence":"native_field"}});
        v["scope"] = json!("subagent");
    }
    let roots: Vec<_> = thread["initial_project_snapshot"]["worktree_snapshots"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, n)| {
            let path = n["worktree_path"].as_str()?;
            let mut root = json!({"id":format!("worktree{i}"),"path":path});
            if let Some(git) = n["git_state"].as_object() {
                root["vcs"] = json!({"type":"git"});
                for (native, asr) in [
                    ("remote_url", "remote"),
                    ("head_sha", "head"),
                    ("current_branch", "branch"),
                ] {
                    if let Some(v) = git.get(native).filter(|v| !v.is_null()) {
                        root["vcs"][asr] = v.clone();
                    }
                }
                c::extra(&mut root, P, "dirtyPatch", &n["git_state"]["diff"]);
            }
            Some(root)
        })
        .collect();
    if !roots.is_empty() {
        v["workspace"] = json!({"roots":roots});
    }
    v["producer"]["lossy"]
        .as_array_mut()
        .unwrap()
        .push(json!("timestamps"));
}
fn parts(a: &Value) -> Vec<Value> {
    a.as_array().into_iter().flatten().filter_map(|b| {
        if let Some(text)=b["Text"].as_str() {Some(json!({"type":"text","text":text}))}
        else if let Some(t)=b.get("Thinking") {let mut p=json!({"type":"reasoning","visibility":"full","text":t["text"]});copy(&mut p,"signature",t,"signature");Some(p)}
        else if let Some(data)=b["RedactedThinking"].as_str() {Some(json!({"type":"reasoning","visibility":"redacted","opaque":{"format":"anthropic.redacted_thinking","data":data}}))}
        else if let Some(data)=b["Image"]["source"].as_str() {Some(json!({"type":"image","mimeType":"image/png","data":data}))}
        else if b.get("ToolUse").is_some() {None}
        else {Some(json!({"type":"data","schema":"dev.zed/content","data":b}))}
    }).collect()
}
fn usage_defaults(n: &Value) -> Value {
    let mut n = n.clone();
    if let Some(o) = n.as_object_mut() {
        // TokenUsage explicitly serializes zero by omission at this source version.
        for k in [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ] {
            o.entry(k).or_insert(json!(0));
        }
    }
    n
}
pub(super) fn convert(s: &mut Spool, w: &mut Writer, _h: &Value, row: &Value) -> Result<()> {
    let n = &row["data"];
    if row["kind"] == "gap" {
        c::emit(s, w, json!({"type":"gap","reason":n["reason"]}), n)?;
        return Ok(());
    }
    if row["kind"] == "session" {
        let t = &n["_thread"];
        let mut state = n.clone();
        state["title"] = t["title"].clone();
        c::session_info(s, w, &state, "title")?;
        if let Some(v) = c::summary(
            &usage_defaults(&t["cumulative_token_usage"]),
            true,
            "cumulative_token_usage",
        ) {
            c::emit(s, w, v, &t["cumulative_token_usage"])?;
        }
        // Request usage is keyed by native ClientUserMessageId, not an inference ID.
        for (id, u) in t["request_token_usage"].as_object().into_iter().flatten() {
            let mut v = json!({"type":"usage_summary","scope":"turn","cumulative":false,"includesChildren":null,"turnId":id,"counter":"request_token_usage","usage":c::usage(&usage_defaults(u),true)});
            c::extra(&mut v, P, "userMessageId", &json!(id));
            c::emit(s, w, v, u)?;
        }
        let mut config = json!({"type":"config_change","snapshot":true});
        if let Some(id) = t["model"]["model"].as_str() {
            config["model"] = json!({"id":id});
            copy(&mut config["model"], "provider", &t["model"], "provider");
        }
        copy(&mut config, "effort", t, "thinking_effort");
        c::extra(&mut config, P, "settings", t);
        if config.get("model").is_some() || config.get("effort").is_some() {
            c::emit(s, w, config, &json!({}))?;
        }
        return Ok(());
    }
    if let Some(compaction) = n.get("Compaction") {
        let mut v = json!({"type":"compaction","compactionId":w.id(),"idSource":"derived","replaces":{"state":"unknown"}});
        if let Some(summary) = compaction["Summary"].as_str() {
            v["kind"] = json!("summary");
            v["summary"] = json!(c::parts(&json!(summary), P));
        } else if let Some(native) = compaction.get("ProviderNative") {
            v["kind"] = json!("provider_native");
            v["opaque"] = json!({"format":native["provider"],"data":native["items"].to_string()});
        } else {
            return c::vendor(s, w, "compaction", n);
        }
        c::emit(s, w, v, n)?;
        return Ok(());
    }
    let (body, role) = if let Some(n) = n.get("User") {
        (n, "user")
    } else if let Some(n) = n.get("Agent") {
        (n, "assistant")
    } else {
        return c::vendor(s, w, "message", n);
    };
    let native_id = body["id"].as_str().filter(|_| !w.capture_scope);
    let mut content = parts(&body["content"]);
    c::binaries(s, &mut content)?;
    let mut v = json!({"type":"message","messageId":native_id.map(str::to_owned).unwrap_or_else(||w.id()),"role":role,"content":content});
    if native_id.is_none() {
        v["idSource"] = json!("derived");
    }
    if role == "assistant" {
        v["author"] = json!("model");
    }
    c::emit(s, w, v, n)?;
    for b in body["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.get("ToolUse"))
    {
        let input = &b["input"];
        let args = if input["type"] == "json" || input["type"] == "text" {
            &input["value"]
        } else {
            input
        };
        c::call(s, w, b, b["id"].as_str(), b["name"].as_str(), args)?;
    }
    for (id, result) in body["tool_results"].as_object().into_iter().flatten() {
        let mut v = c::result(s, w, Some(id), &Value::Null)?;
        let raw = &result["content"];
        let content = if raw.is_array() {
            parts(raw)
        } else if raw.is_string() {
            c::parts(raw, P)
        } else {
            parts(&json!([raw]))
        };
        v["content"] = json!(content);
        v.as_object_mut().unwrap().remove("capture");
        c::binaries(s, v["content"].as_array_mut().unwrap())?;
        if let Some(error) = result["is_error"].as_bool() {
            v["status"] = json!(if error { "failed" } else { "completed" });
        }
        if !result["output"].is_null() {
            v["raw"] = result["output"].clone();
        }
        c::emit(s, w, v, result)?;
    }
    Ok(())
}
