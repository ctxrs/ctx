//! Native document and event-store adapters. No projection through searchable Core.
mod document;
#[cfg(test)]
pub(crate) use document::unsafe_numbers as unsafe_native_number;
mod hooks;
mod messages;
mod metrics;
mod openhands;
mod snapshots;
pub(crate) mod source;
use crate::{
    records::{meta, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) fn string<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let mut selected = None;
    for key in keys {
        if let Some(s) = v.get(*key).and_then(Value::as_str) {
            if selected.is_some_and(|p| p != s) {
                return None;
            }
            selected = Some(s);
        }
    }
    selected.filter(|s| !s.is_empty())
}
pub(super) fn identity(row: &Value, pos: u64) -> String {
    row["nativeId"]
        .as_str()
        .map(|s| format!("{}:{s}", row["kind"].as_str().unwrap_or("item")))
        .unwrap_or_else(|| format!("position:{pos}"))
}
pub(crate) fn header(p: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":p.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":"ctx-asr-batch10-1","source":{"format":p.format()},"lossy":["display_content","message.author","timestamps","inference","lineage","copies","provider.identity","revision_history","order","_rs.ctx/unmapped_native_variants"]},"profiles":["core"],"contentLevel":"full","extensions":[{"id":format!("{}/native",p.domain()),"version":"1","required":false}]});
    let metadata = n.get("_ctx_metadata").unwrap_or(n);
    if let Some(cwd) = string(
        metadata,
        &[
            "workspaceDirectory",
            "workspaceRoot",
            "workspace_path",
            "cwd",
        ],
    )
    .filter(|s| s.starts_with('/'))
    {
        h["workspace"] = json!({"roots":[{"id":"workspace","path":cwd}]});
    }
    // Parent labels do not, by themselves, establish fork or delegation semantics.
    if let Some(parent) = string(
        metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "parentConversationId",
        ],
    ) {
        h["lineage"] = json!({"relatedUnknown":{"session":crate::records::session_ref(p,parent),"evidence":"native_field"}});
    }
    h
}
pub(crate) fn prepare(spool: &mut Spool) -> Result<()> {
    spool.db.execute_batch("CREATE TABLE batch_calls(id TEXT, pos INTEGER, selector INTEGER, response TEXT); CREATE INDEX batch_call_ids ON batch_calls(id); CREATE TABLE batch_context(event TEXT, id TEXT PRIMARY KEY, parent TEXT, response TEXT, mechanical INTEGER, pos INTEGER); CREATE TABLE batch_parents(event TEXT PRIMARY KEY,parent TEXT);")?;
    let duplicate: bool = spool.db.query_row("SELECT EXISTS(SELECT 1 FROM rows WHERE kind='event' AND native_id IS NOT NULL GROUP BY native_id HAVING count(*)>1)", [], |r| r.get(0))?;
    if duplicate {
        return Err(crate::Error::Invalid(
            "duplicate OpenHands event IDs cannot establish a conversation tree".into(),
        ));
    }
    let mut pos = 0;
    let mut previous: Option<String> = None;
    while let Some(next) = spool.next(pos)? {
        pos = next;
        let row = spool.get(pos)?;
        let v = &row["value"];
        if v["message"]["role"] == "tool" {
            if let Some(id) = v["message"]["toolCallId"].as_str() {
                spool.save(&format!("continue-result:{id}"), &json!(true))?;
            }
        }
        if v["kind"] == "Condensation" {
            if let Some(id) = v["id"].as_str() {
                spool.save(&format!("condensation:{id}"), v)?;
            }
        }
        for (i, call) in messages::calls(v).iter().enumerate() {
            if let Some(id) = messages::call_id(call) {
                spool.db.execute(
                    "INSERT INTO batch_calls VALUES(?1,?2,?3,?4)",
                    params![id, pos, i, v["llm_response_id"].as_str()],
                )?;
            }
        }
        if v["kind"] == "SystemPromptEvent" {
            if let Some(id) = v["id"].as_str() {
                spool.save(&format!("system:{id}"), &json!(true))?;
            }
        }
        if row["kind"] == "event" {
            if let Some(id) = v["id"].as_str() {
                let parent = if v["parent_id"].is_null() {
                    previous.as_deref()
                } else {
                    v["parent_id"].as_str()
                };
                spool.db.execute(
                    "INSERT OR IGNORE INTO batch_parents VALUES(?1,?2)",
                    params![id, parent],
                )?;
                previous = Some(id.into());
            }
        }
    }
    Ok(())
}
pub(crate) fn convert(spool: &mut Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let mut key = identity(row, w.pos);
    let collision = if let Some(id) = row["nativeId"].as_str() {
        spool.db.query_row(
            "SELECT count(*)>1 FROM rows WHERE kind=?1 AND native_id=?2",
            params![row["kind"].as_str(), id],
            |r| r.get::<_, bool>(0),
        )?
    } else {
        false
    };
    if collision {
        key = format!("position:{}", w.pos);
    }
    w.identity = Some(format!("b{:x}", Sha256::digest(key.as_bytes())));
    let mut row = row.clone();
    row["captureScope"] = json!(collision || row["nativeId"].is_null());
    if row["kind"] == "gap" {
        w.slot(0);
        emit(
            spool,
            w,
            json!({"type":"gap","reason":row["value"]["reason"]}),
            &row,
        )?;
        return Ok(());
    }
    if row["value"].get("_ctx_native_json").is_some() {
        w.slot(0);
        emit(
            spool,
            w,
            json!({"type":format!("{}/numeric_record",w.provider.domain())}),
            &row,
        )?;
        return Ok(());
    }
    match w.provider {
        Provider::OpenHands => openhands::convert(spool, w, &row),
        Provider::Continue => snapshots::continue_item(spool, w, &row),
        Provider::Auggie => snapshots::auggie(spool, w, &row),
        Provider::CodeBuddy => messages::codebuddy(spool, w, &row),
        Provider::RovoDev => messages::rovo(spool, w, &row),
        _ => Ok(()),
    }
}
pub(super) fn emit(spool: &Spool, w: &mut Writer, mut v: Value, row: &Value) -> Result<String> {
    if row["snapshot"] == true {
        v["assembly"] = json!({"mode":"snapshot"});
    }
    if row["captureScope"] == true {
        v["idScope"] = json!("capture");
    }
    let native = &row["value"];
    crate::capture::retain(&mut v, w.provider, native, spool)?;
    let time = string(native, &["timestamp", "created_at", "createdAt"]);
    let source = time.map(|s| json!({"timestamp":s})).unwrap_or(json!({}));
    externalize(spool, &mut v)?;
    let context =
        w.provider == Provider::OpenHands && row["kind"] == "event" && crate::records::context(&v);
    if context {
        openhands::structure(spool, w, &mut v, row)?;
    }
    let previous = w.tip.clone();
    let id = w.emit(v.clone(), &source)?;
    if context {
        openhands::register(spool, w, &v, row, &id, previous)?;
    }
    Ok(id)
}
fn externalize(spool: &Spool, v: &mut Value) -> Result<()> {
    use base64::Engine;
    use std::io::Write;
    for field in ["content", "summary", "displayContent"] {
        if let Some(content) = v.get_mut(field).and_then(Value::as_array_mut) {
            for part in content {
                if matches!(part["type"].as_str(), Some("image" | "audio")) {
                    if let Some(data) = part["data"].as_str().filter(|s| s.len() > 65_536) {
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                            let mut f = tempfile::NamedTempFile::new_in(&spool.bundle)?;
                            f.write_all(&bytes)?;
                            let mut blob = spool.blob(f)?;
                            blob["mimeType"] = part["mimeType"].clone();
                            part.as_object_mut().expect("content object").remove("data");
                            part["blob"] = blob;
                        }
                    }
                }
            }
        }
    }
    for field in ["_meta", "raw"] {
        if let Some(data) = v.get(field) {
            let raw = serde_json::to_vec(data)?;
            if raw.len() > spool.limit / 2 {
                let mut f = tempfile::NamedTempFile::new_in(&spool.bundle)?;
                f.write_all(&raw)?;
                let blob = spool.blob(f)?;
                if field == "_meta" {
                    v[field] = json!({"rs.ctx/nativeMetadataBlob":blob});
                } else {
                    v[field] = json!({"blob":blob});
                    v["capture"]["raw"] = json!({"state":"external","blob":blob});
                }
            }
        }
    }
    Ok(())
}
pub(super) fn entity(v: &mut Value, key: &str, native: Option<&str>, w: &Writer) {
    v[key] = json!(native.map(str::to_owned).unwrap_or_else(|| w.id()));
    v[if key == "callId" {
        "callIdSource"
    } else {
        "idSource"
    }] = json!(if native.is_some() {
        "native"
    } else {
        "derived"
    });
}
pub(super) fn parts(v: &Value, p: Provider) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.iter().flat_map(|v| parts(v, p)).collect();
    }
    if v["type"] == "image" {
        if let Some(urls) = v["image_urls"].as_array() {
            return urls
                .iter()
                .flat_map(|url| {
                    crate::content::parts(&json!([{"type":"image","image_url":url}]), p)
                })
                .collect();
        }
    }
    if v.is_object() {
        crate::content::parts(&json!([v]), p)
    } else {
        crate::content::parts(v, p)
    }
}
pub(super) fn usage(v: &Value) -> Value {
    let mut usage = json!({});
    for (dest, keys) in [
        (
            "input",
            &[
                "promptTokens",
                "input_tokens",
                "prompt_tokens",
                "input_tokens_total",
            ][..],
        ),
        (
            "output",
            &["completionTokens", "output_tokens", "completion_tokens"][..],
        ),
    ] {
        if let Some(n) = keys
            .iter()
            .find_map(|k| v[*k].as_u64())
            .filter(|n| *n <= 9_007_199_254_740_991)
        {
            usage[dest] = json!({"total":n});
            if dest == "input" {
                usage[dest]["includesCache"] = Value::Null;
            }
        }
    }
    if let Some(n) = v["totalTokens"]
        .as_u64()
        .or_else(|| v["total_tokens"].as_u64())
    {
        usage["total"] = json!(n);
    }
    usage
}
pub(super) fn metadata(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let n = &row["value"];
    w.slot(800_000);
    let mut v = json!({"type":"session_info"});
    if let Some(title) = string(n, &["title", "name"]) {
        v["title"] = json!(title);
    }
    emit(spool, w, v, row)?;
    w.slot(810_000);
    if n.get("mode").is_some() || n.get("chatModelTitle").is_some() {
        let mut config = json!({"type":"config_change","snapshot":true});
        for k in ["mode", "chatModelTitle"] {
            if let Some(s) = n[k].as_str() {
                if k == "mode" {
                    config["mode"] = json!(s);
                } else {
                    config["options"] = json!({"chatModelTitle":s});
                }
            }
        }
        emit(spool, w, config, row)?;
    }
    if let Some(u) = n.get("usage").filter(|v| v.is_object()) {
        w.slot(820_000);
        let mut v = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"usage":usage(u)});
        if let Some(cost) = u["totalCost"].as_f64() {
            v["cost"] = json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
        }
        emit(spool, w, v, row)?;
    }
    Ok(())
}
