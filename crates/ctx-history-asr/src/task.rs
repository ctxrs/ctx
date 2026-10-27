//! Cline/Roo native task arrays; API and UI stores are never joined by position.
mod api;
mod ui;
use crate::{
    capture,
    records::{copy, Writer},
    source::Source,
    spool::Spool,
    Error, Provider, Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path};
const FILES: &[&str] = &[
    "api_conversation_history.json",
    "claude_messages.json",
    "ui_messages.json",
    "task_metadata.json",
    "history_item.json",
    "_index.json",
    "context_history.json",
];

pub(crate) fn inspect(dir: &Path, provider: Provider, limit: usize) -> Result<Option<Source>> {
    if !FILES[..3].iter().any(|f| dir.join(f).is_file()) {
        return Ok(None);
    }
    let session = dir
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Invalid("missing native task directory ID".into()))?
        .to_owned();
    let mut header = json!({});
    if dir.join("history_item.json").is_file() {
        let item = capture::object(&dir.join("history_item.json"), limit)?;
        if item["id"].as_str().is_some_and(|id| id != session) {
            return Err(Error::Invalid(
                "task catalog ID disagrees with task directory".into(),
            ));
        }
        copy(&mut header, "cwd", &item, "cwd");
        copy(&mut header, "cwd", &item, "cwdOnTaskInitialization");
    }
    Ok(Some(Source {
        provider,
        path: dir.to_owned(),
        session: session.clone(),
        segment: session,
        header,
        stamp: capture::stamp(dir, FILES)?,
    }))
}

pub(crate) fn ingest(spool: &mut Spool, dir: &Path) -> Result<()> {
    let api = if dir.join(FILES[0]).is_file() {
        FILES[0]
    } else {
        FILES[1]
    };
    for name in [api, "ui_messages.json"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let component = if name == "ui_messages.json" {
            "ui"
        } else {
            "api"
        };
        let limit = spool.limit;
        spool.source_bytes += fs::metadata(&path)?.len();
        capture::array(&path, limit, &mut |index, value, _size| {
            let value = value.unwrap_or_else(|| json!({"type":"_gap","reason":"conversion_loss"}));
            let key = if component == "ui" {
                value["ts"]
                    .as_u64()
                    .filter(|v| *v <= 9_007_199_254_740_991)
                    .map(|n| format!("ui:ts:{n}"))
            } else {
                value["id"].as_str().map(|s| format!("api:id:{s}"))
            };
            let row = json!({"component":component,"index":index,"key":key,"row":value});
            let pos = spool.push(&row, key.as_deref())?;
            if component == "api" {
                for block in value["content"].as_array().into_iter().flatten() {
                    let (table, id) = match block["type"].as_str() {
                        Some("tool_use" | "server_tool_use") => {
                            ("native_calls", block["id"].as_str())
                        }
                        Some("tool_result") => ("tool_results", block["tool_use_id"].as_str()),
                        _ => continue,
                    };
                    if let Some(id) = id {
                        spool.db.execute(
                            &format!("INSERT INTO {table} VALUES(?1,?2)"),
                            rusqlite::params![id, pos],
                        )?;
                    }
                }
            }
            Ok(())
        })?;
    }
    for name in &FILES[3..] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        if *name == "context_history.json" {
            // Context edits have their own array order. Spool them before secret
            // preparation; creating an opaque blob here would bypass redaction.
            let limit = spool.limit;
            spool.source_bytes += fs::metadata(&path)?.len();
            capture::array(&path, limit, &mut |index, value, _size| {
                let value =
                    value.unwrap_or_else(|| json!({"type":"_gap","reason":"conversion_loss"}));
                spool.push(
                    &json!({"component":"context","index":index,"row":value}),
                    None,
                )?;
                Ok(())
            })?;
        } else {
            let value = capture::object(&path, spool.limit)?;
            spool.source_bytes += fs::metadata(path)?.len();
            spool.push(
                &json!({"component":"metadata","index":name,"row":value}),
                Some(name),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn convert(spool: &mut Spool, writer: &mut Writer, value: &Value) -> Result<()> {
    let component = value["component"].as_str().unwrap_or("metadata");
    let candidate = value["key"].as_str();
    let stable = candidate
        .map(|key| spool.unique(key))
        .transpose()?
        .unwrap_or(false);
    let key = if stable {
        candidate.unwrap().to_owned()
    } else {
        format!("{component}:{}", value["index"])
    };
    writer.capture_identity(
        &key,
        !stable && component != "metadata",
        component == "metadata",
    );
    let source = &value["row"];
    if source["type"] == "_gap" {
        writer.emit(
            json!({"type":"gap","reason":source["reason"],"count":1}),
            &json!({}),
        )?;
        return Ok(());
    }
    match component {
        "api" => api::convert(spool, writer, source),
        "ui" => ui::convert(spool, writer, source),
        _ => {
            let mut v = json!({"type":format!("{}/task_metadata", writer.provider.domain()),"component":value["index"]});
            capture::retain(&mut v, writer.provider, source, spool)?;
            writer.emit(v, &json!({}))?;
            if value["index"] == "history_item.json" {
                let mut info = json!({"type":"session_info"});
                copy(&mut info, "title", source, "task");
                if info.get("title").is_some() {
                    writer.emit(info, &json!({}))?;
                }
                if let Some(usage) = ui::usage(source) {
                    writer.emit(json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"usage":usage}), &json!({}))?;
                }
            }
            Ok(())
        }
    }
}

pub(super) fn emit(spool: &Spool, w: &mut Writer, mut record: Value, source: &Value) -> Result<()> {
    if let Some(ts) = capture::millis(&source["ts"]) {
        record["ts"] = json!(ts);
        record["tsPrecision"] = json!("ms");
    }
    capture::retain(&mut record, w.provider, source, spool)?;
    w.emit(record, &json!({}))?;
    Ok(())
}
