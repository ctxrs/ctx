//! Native ownership and cursors for the conversation families; no Core projection.
mod snapshot;
use crate::{
    records::{copy, extras, meta, session_ref, timestamp},
    source::{self, Source},
    spool::Spool,
    Input, Provider, Result,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Seek, Write},
    path::Path,
};

pub(crate) fn inspect(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let path = &input.path;
    let legacy =
        input.provider == Provider::Gemini && path.extension().is_some_and(|e| e == "json");
    // A migration retains the old JSON; the JSONL sibling is authoritative.
    if legacy && path.with_extension("jsonl").is_file() {
        return Ok(());
    }
    if input.provider == Provider::Antigravity {
        let name = path.file_name().and_then(|s| s.to_str());
        if !matches!(name, Some("transcript.jsonl" | "transcript_full.jsonl")) {
            return Ok(());
        }
        if name == Some("transcript.jsonl")
            && path.with_file_name("transcript_full.jsonl").is_file()
        {
            return Ok(());
        }
    }
    let mut h = if legacy {
        snapshot::visit(path, limit, &mut |_| Ok(()))?.0
    } else {
        let mut reader = source::reader(path)?;
        let mut bytes = Vec::new();
        let mut header = None;
        for _ in 0..64 {
            let Some((_, large)) = source::line(&mut *reader, &mut bytes, limit)? else {
                break;
            };
            if !large {
                if let Some(v) = source::decode(&bytes) {
                    let journal = input.provider != Provider::QwenCode
                        || (v["uuid"].is_string() && v["type"].is_string());
                    if input.provider == Provider::Antigravity
                        || (journal && v["sessionId"].is_string())
                    {
                        header = Some(v);
                        break;
                    }
                }
            }
        }
        header.unwrap_or(Value::Null)
    };
    let id = match input.provider {
        Provider::Antigravity => path
            .parent()
            .filter(|p| p.ends_with("logs"))
            .and_then(Path::parent)
            .filter(|p| p.ends_with(".system_generated"))
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .map(str::to_owned),
        Provider::QwenCode if h["isSidechain"] == true => h["agentId"].as_str().map(str::to_owned),
        _ => h["sessionId"].as_str().map(str::to_owned),
    };
    let Some(id) = id.filter(|s| !s.is_empty()) else {
        *unidentified += 1;
        return Ok(());
    };
    if input.provider == Provider::Gemini && h["kind"] == "subagent" {
        if let Some(parent) = path
            .parent()
            .filter(|p| p.parent().is_some_and(|p| p.ends_with("chats")))
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
        {
            h["_parentDirectory"] = json!(parent);
        }
    }
    h["_snapshot"] = json!(legacy);
    let m = fs::metadata(path)?;
    let stamp = format!("{}:{:?}", m.len(), m.modified()?);
    // Gemini resumed recordings may share a session ID; filename is the native segment.
    let segment = if input.provider == Provider::Gemini {
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or(&id)
            .to_owned()
    } else {
        id.clone()
    };
    visitor(Source {
        provider: input.provider,
        path: path.clone(),
        session: id,
        header: h,
        stamp,
        segment,
    })
}

pub(crate) fn ingest(
    spool: &mut Spool,
    path: &Path,
    provider: Provider,
    segment: &str,
    allocate: &mut dyn FnMut(&str) -> Result<u64>,
) -> Result<()> {
    let legacy = provider == Provider::Gemini && path.extension().is_some_and(|e| e == "json");
    let mut physical = 0;
    if legacy {
        let (h, bytes) = snapshot::visit(path, spool.limit, &mut |mut v| {
            physical += 1;
            if !v.is_object() {
                v = json!({"type":"_gap","reason":"unparseable"});
            }
            v["_snapshot"] = json!(true);
            let local = format!("l{physical}");
            insert(spool, provider, segment, allocate(&local)?, &v)
        })?;
        insert(spool, provider, segment, allocate("metadata")?, &h)?;
        spool.source_bytes += bytes;
    } else {
        let mut reader = source::reader(path)?;
        let mut bytes = Vec::new();
        while let Some((size, large)) = source::line(&mut *reader, &mut bytes, spool.limit)? {
            physical += 1;
            spool.source_bytes += size as u64;
            if !large && bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let v = (!large).then(||source::decode(&bytes)).flatten().unwrap_or_else(||json!({"type":"_gap","reason":if large {"conversion_loss"}else{"unparseable"}}));
            let local = match provider {
                Provider::QwenCode => v["uuid"].as_str().map(|id| format!("uuid:{id}")),
                Provider::Antigravity => v["step_index"].as_u64().map(|i| format!("step:{i}")),
                _ => None,
            }
            .unwrap_or_else(|| format!("l{physical}"));
            insert(spool, provider, segment, allocate(&local)?, &v)?;
        }
    }
    spool.bytes.flush()?;
    Ok(())
}
fn insert(spool: &mut Spool, provider: Provider, segment: &str, pos: u64, v: &Value) -> Result<()> {
    crate::privacy::questions(spool, v)?;
    let off = spool.bytes.stream_position()?;
    let bytes = serde_json::to_vec(v)?;
    spool.bytes.write_all(&bytes)?;
    spool.source_lines += 1;
    let native = if provider == Provider::QwenCode {
        v["uuid"].as_str()
    } else {
        v["id"].as_str()
    };
    spool.db.execute(
        "INSERT OR REPLACE INTO rows VALUES(?1,?2,?3,?4,?5,NULL,?6,NULL,?7,NULL,?8)",
        params![
            pos,
            off,
            bytes.len(),
            v["type"].as_str().unwrap_or("metadata"),
            native,
            v["parentUuid"].as_str(),
            segment,
            spool.source_lines
        ],
    )?;
    // JSONL observations with repeated Gemini IDs revise one call entity, not duplicate calls.
    let owner: u64 = if let Some(id) = native {
        spool
            .db
            .query_row("SELECT min(pos) FROM rows WHERE native_id=?1", [id], |r| {
                r.get(0)
            })?
    } else {
        pos
    };
    index_calls(spool, v, owner)?;
    for m in v
        .pointer("/$set/messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        index_calls(spool, m, owner)?;
    }
    Ok(())
}
fn index_calls(spool: &Spool, v: &Value, owner: u64) -> Result<()> {
    let owner = v["id"]
        .as_str()
        .or_else(|| v["uuid"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| owner.to_string());
    if let Some(calls) = v["toolCalls"].as_array() {
        for call in calls {
            if let Some(id) = call["id"].as_str() {
                spool
                    .db
                    .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, owner])?;
            }
        }
    }
    for part in v
        .pointer("/message/parts")
        .or_else(|| v.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = part.pointer("/functionCall/id").and_then(Value::as_str) {
            spool
                .db
                .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, owner])?;
        }
    }
    Ok(())
}

pub(crate) fn header(provider: Provider, id: &str, n: &Value, revision: &str) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":revision,"source":{"format":provider.format()},"lossy":["native","message.author","cost","exec.exit_code","provider.identity","revision_history","inference","_rs.ctx/native_line_limit","_rs.ctx/nontranscript_state","_rs.ctx/vendor_only"]},"profiles":["core","lineage","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    if let Some(start) = n["startTime"].as_str().and_then(timestamp) {
        h["startedAt"] = json!(start);
    }
    if provider == Provider::QwenCode {
        copy(&mut h["agent"], "version", n, "version");
    }
    if let Some(cwd) = n["cwd"].as_str().filter(|p| p.starts_with('/')) {
        h["workspace"]["roots"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"workspace","path":cwd}));
    }
    let mut lineage = json!({});
    if provider == Provider::Gemini {
        match n["kind"].as_str() {
            Some("main") => h["scope"] = json!("primary"),
            Some("subagent") => h["scope"] = json!("subagent"),
            _ => {}
        }
        if let Some(p) = n["_parentDirectory"].as_str() {
            lineage["spawnedBy"] =
                json!({"session":session_ref(provider,p),"evidence":"native_structure"});
        }
    }
    if provider == Provider::QwenCode {
        if n["isSidechain"] == true {
            h["scope"] = json!("subagent");
            if let Some(parent) = n["sessionId"].as_str() {
                lineage["spawnedBy"] =
                    json!({"session":session_ref(provider,parent),"evidence":"native_field"});
            }
        } else if let Some(parent) = n["_parentSession"].as_str() {
            lineage["spawnedBy"] =
                json!({"session":session_ref(provider,parent),"evidence":"native_field"});
        }
        if let Some(parent) = n.pointer("/forkedFrom/sessionId").and_then(Value::as_str) {
            lineage["forkedFrom"] =
                json!({"session":session_ref(provider,parent),"evidence":"native_field"});
            lineage["inherits"] = json!({"mode":"copied","from":session_ref(provider,parent),"evidence":"native_field"});
        }
    }
    if !lineage.as_object().unwrap().is_empty() {
        h["lineage"] = lineage;
    }
    if provider != Provider::Antigravity {
        extras(
            &mut h,
            provider,
            n,
            &[
                "messages",
                "content",
                "message",
                "toolCalls",
                "sessionId",
                "startTime",
                "timestamp",
                "uuid",
                "parentUuid",
                "version",
                "cwd",
                "_snapshot",
                "_parentDirectory",
                "_parentSession",
                "forkedFrom",
                "_sidecar",
            ],
        );
    }
    if let Some(losses) = h["producer"]["lossy"].as_array_mut() {
        match provider {
            Provider::Gemini => losses.push(json!("_com.google.gemini-cli/history_snapshot_order")),
            Provider::QwenCode => {
                losses.push(json!(
                    "_io.github.qwenlm.qwen-code/compressed_context_identity"
                ));
                losses.push(json!("_io.github.qwenlm.qwen-code/usage_inclusion"));
            }
            Provider::Antigravity => {
                losses.push(json!("tool_result.content"));
                losses.push(json!("lineage"));
                losses.push(json!("usage"));
            }
            _ => {}
        }
    }
    if provider == Provider::Gemini && n["_snapshot"] == true {
        meta(&mut h, provider, "storage", &json!("json-snapshot"));
    }
    h
}
