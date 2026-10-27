//! Cline 3 SDK SessionManifest and PersistedMessages v1 (not extension task arrays).
use crate::{
    batch10::document,
    capture,
    records::{copy, Writer},
    source::Source,
    spool::Spool,
    Error, Provider, Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path};

pub(super) fn inspect(dir: &Path, limit: usize) -> Result<Option<Source>> {
    let Some(id) = dir.file_name().and_then(|n| n.to_str()) else {
        return Ok(None);
    };
    let messages = format!("{id}.messages.json");
    let manifest = format!("{id}.json");
    if !dir.join(&messages).is_file() || !dir.join(&manifest).is_file() {
        return Ok(None);
    }
    let mut header = capture::object(&dir.join(&manifest), limit)?;
    if header["version"] != 1 || header["session_id"] != id {
        return Err(Error::Invalid(
            "unsupported Cline SDK manifest or inconsistent session identity".into(),
        ));
    }
    header["_sdk"] = json!(true);
    header["startedAt"] = header["started_at"].clone();
    Ok(Some(Source {
        provider: Provider::Cline,
        path: dir.into(),
        session: id.into(),
        segment: id.into(),
        header,
        stamp: capture::stamp(dir, &[&messages, &manifest])?,
    }))
}

pub(super) fn ingest(spool: &mut Spool, dir: &Path) -> Result<bool> {
    let Some(source) = inspect(dir, spool.limit)? else {
        return Ok(false);
    };
    let path = dir.join(format!("{}.messages.json", source.session));
    spool.source_bytes += fs::metadata(&path)?.len();
    let mut header = document::read(&path, &["messages"], spool.limit, &mut |_, index, raw| {
        let n = document::decode(raw)?;
        let key = n["id"].as_str().map(|id| format!("sdk:{id}"));
        let pos = spool.push(
            &json!({"component":"sdk","index":index,"key":key,"row":n}),
            key.as_deref(),
        )?;
        for b in n["content"].as_array().into_iter().flatten() {
            let (table, id) = match b["type"].as_str() {
                Some("tool_use") => ("native_calls", b["id"].as_str()),
                Some("tool_result") => ("tool_results", b["tool_use_id"].as_str()),
                _ => continue,
            };
            if let Some(id) = id {
                spool.db.execute(
                    &format!("INSERT INTO {table} VALUES(?1,?2)"),
                    rusqlite::params![id, pos],
                )?;
            }
        }
        Ok(())
    })?;
    if header["version"] != 1 || header["sessionId"] != source.session {
        return Err(Error::Invalid(
            "unsupported Cline SDK messages or inconsistent session identity".into(),
        ));
    }
    header
        .as_object_mut()
        .unwrap()
        .remove("_ctx_count_messages");
    let manifest = dir.join(format!("{}.json", source.session));
    spool.source_bytes += fs::metadata(&manifest)?.len();
    let manifest = capture::object(&manifest, spool.limit)?;
    for (key, value) in [("sdk_messages", header), ("sdk_manifest", manifest)] {
        spool.push(
            &json!({"component":"sdk_metadata","index":key,"key":key,"row":value}),
            Some(key),
        )?;
    }
    Ok(true)
}

pub(super) fn convert(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    super::api::convert(spool, w, n)?;
    if n["role"] == "assistant" && n["metrics"].is_object() {
        w.slot(800_000);
        let mut r = json!({"type":"inference","inferenceId":format!("inference:{}",n["id"].as_str().unwrap_or(&w.id())),"idSource":"derived"});
        if n["modelInfo"]["id"].is_string() {
            r["model"] = n["modelInfo"].clone();
        }
        let mut usage = json!({});
        for (dest, key) in [
            ("total", "inputTokens"),
            ("cacheRead", "cacheReadTokens"),
            ("cacheWrite", "cacheWriteTokens"),
        ] {
            copy(&mut usage["input"], dest, &n["metrics"], key);
        }
        if usage["input"]["total"].is_number() {
            usage["input"]["includesCache"] = Value::Null;
        }
        copy(&mut usage["output"], "total", &n["metrics"], "outputTokens");
        r["usage"] = usage;
        super::emit(spool, w, r, n)?;
    }
    Ok(())
}
