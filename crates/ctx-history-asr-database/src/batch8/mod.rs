//! Native SQLite converters. Capture order is independent of stable native identity.
pub mod application;
mod deepagents;
mod devin;
mod messages;
mod snapshots;
pub mod sqlite;
mod tree;
mod values;

use crate::{
    records::{meta, Writer},
    source::Source,
    spool::Spool,
    Error, Input, Provider, Result,
};
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
    database: &mut dyn FnMut(Provider, &Path) -> Result<()>,
) -> Result<()> {
    if input.path.is_dir() {
        for entry in std::fs::read_dir(&input.path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                || entry
                    .path()
                    .extension()
                    .is_some_and(|s| s == "db" || s == "sqlite" || s == "sqlite3")
            {
                visit(
                    &Input {
                        provider: input.provider,
                        path: entry.path(),
                    },
                    limit,
                    visitor,
                    unidentified,
                    database,
                )?;
            }
        }
        return Ok(());
    }
    if !input.path.exists() {
        return Ok(());
    }
    let path = std::fs::canonicalize(&input.path)?;
    sqlite::read(&path, |db| {
        let (table, id) = match input.provider {
            Provider::Firebender => ("chat_sessions", "id"),
            Provider::ForgeCode => ("conversations", "conversation_id"),
            Provider::DeepAgents => ("checkpoints", "thread_id"),
            Provider::Devin => ("sessions", "id"),
            _ => return Err(Error::Invalid("not a SQLite provider".into())),
        };
        if !sqlite::exists(db, table)? {
            *unidentified += 1;
            return Ok(());
        }
        database(input.provider, &path)?;
        let stamp = sqlite::stamp(&path)?;
        let sql = if input.provider == Provider::DeepAgents {
            "SELECT thread_id AS native FROM checkpoints UNION SELECT thread_id FROM writes ORDER BY 1".into()
        } else {
            format!("SELECT {id} AS native FROM {table} ORDER BY {id}")
        };
        // Bound identity strings before transferring them from SQLite.
        let mut stmt = db.prepare(&format!("SELECT CASE WHEN typeof(native)='text' AND length(cast(native as blob))<=?1 THEN native END FROM ({sql}) AS ids"))?;
        let mut rows = stmt.query([limit as i64])?;
        while let Some(row) = rows.next()? {
            let Some(native) = row.get::<_, Option<String>>(0)?.filter(|s| !s.is_empty()) else {
                *unidentified += 1;
                continue;
            };
            visitor(Source {
                provider: input.provider,
                path: path.clone(),
                session: native.clone(),
                header: json!({"id":native,"nativeSession":native}),
                stamp: stamp.clone(),
                segment: native,
            })?;
        }
        Ok(())
    })
}

pub fn ingest(spool: &mut Spool, path: &Path, provider: Provider, selector: &Value) -> Result<()> {
    sqlite::read(path, |db| {
        spool.save("batch8:header", selector)?;
        match provider {
            Provider::Firebender | Provider::ForgeCode => {
                snapshots::ingest(spool, db, provider, selector)?
            }
            Provider::DeepAgents => deepagents::ingest(spool, db, selector)?,
            Provider::Devin => devin::ingest(spool, db, selector)?,
            _ => return Err(Error::Invalid("not a SQLite provider".into())),
        }
        Ok(())
    })?;
    // A complete capture is available before resolving any duplicate call ID.
    let mut pos = 0;
    while let Some(next) = spool.next(pos)? {
        pos = next;
        let v = spool.get(pos)?;
        crate::privacy::questions(spool, &v)?;
        let owner = if provider == Provider::DeepAgents {
            if let Some(id) = v["messageId"].as_str() {
                let key = format!("batch8:callOwner:{id}");
                let owner = spool.entity(&key)?.and_then(|v| v.as_u64()).unwrap_or(pos);
                spool.save(&key, &json!(owner))?;
                owner
            } else {
                pos
            }
        } else {
            pos
        };
        let call_counts = messages::call_counts(&v);
        for (i, call) in v["calls"].as_array().into_iter().flatten().enumerate() {
            if let Some(id) = call["id"].as_str() {
                let repeated_in_unit = call_counts.get(id).is_some_and(|n| *n > 1);
                if provider == Provider::DeepAgents && repeated_in_unit {
                    spool.save(
                        &format!("batch8:duplicateInMessage:{}", json!([v["messageId"], id])),
                        &json!(true),
                    )?;
                }
                let position = if repeated_in_unit {
                    pos * crate::records::STRIDE + i as u64
                } else {
                    owner * crate::records::STRIDE
                };
                spool.db.execute(
                    "INSERT INTO native_calls VALUES(?1,?2)",
                    params![id, position],
                )?;
                if let Some(response) = v["response"].as_str() {
                    spool.save(&format!("batch8:response:{id}"), &json!(response))?;
                }
            }
        }
    }
    Ok(())
}
pub(super) fn push(
    spool: &mut Spool,
    key: Value,
    mut item: Value,
    capture_scoped: bool,
) -> Result<()> {
    if item["calls"]
        .as_array()
        .is_some_and(|calls| calls.len() >= crate::records::STRIDE as usize - 1)
    {
        return Err(Error::Invalid("too many calls for one native item".into()));
    }
    item["key"] = key;
    item["captureScoped"] = json!(capture_scoped);
    spool.push_native(&item)
}
pub(super) fn gap(spool: &mut Spool, key: Value, raw: &Value, reason: &str) -> Result<()> {
    push(
        spool,
        key,
        json!({"type":"gap","reason":reason,"native":raw}),
        true,
    )
}
pub fn convert(spool: &mut Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    writer.capture_scope = v["captureScoped"] == true;
    writer.identity = Some(format!(
        "r{:x}",
        Sha256::digest(v["key"].to_string().as_bytes())
    ));
    match v["type"].as_str() {
        Some("message") => messages::convert(spool, writer, v),
        Some("deepRemove") => deepagents::remove(spool, writer, v),
        Some("devin") => devin::convert(spool, writer, v),
        Some("devinState") => devin::tool_state(spool, writer, v),
        Some("state") => snapshots::state(spool, writer, v),
        Some("gap") => {
            let mut r = json!({"type":"gap","reason":v["reason"]});
            values::preserve(spool, &mut r, writer.provider, "source", &v["native"])?;
            writer.emit(r, &json!({}))?;
            Ok(())
        }
        _ => {
            let mut r = json!({"type":format!("{}/{}", writer.provider.domain(), v["kind"].as_str().unwrap_or("row"))});
            values::preserve(spool, &mut r, writer.provider, "source", &v["native"])?;
            writer.emit(r, &json!({}))?;
            Ok(())
        }
    }
}
pub fn header(provider: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,
        "producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":"ctx-asr-batch8-1","source":{"format":provider.format()},
        "lossy":["timestamps","order","message.author","display_content","revision_history","inference","lineage","copies","file_change","provider.identity","_rs.ctx/sqlite_row_limit","_rs.ctx/vendor_semantics","_rs.ctx/unprojected_lanes","_rs.ctx/compaction_context"]},
        "profiles":["core","lineage","usage"],"contentLevel":"full",
        "workspace":{"roots":[{"id":"unknown","path":null}]},
        "extensions":[{"id":format!("{}/sqlite",provider.domain()),"version":"1","required":false}]});
    if let Some(cwd) = n["cwd"]
        .as_str()
        .filter(|s| s.starts_with('/') || s.as_bytes().get(1) == Some(&b':'))
    {
        if let Some(roots) = h["workspace"]["roots"].as_array_mut() {
            roots.push(json!({"id":"workspace","path":cwd}));
        }
    }
    if let Some(ts) = n["startedAt"].as_str() {
        h["startedAt"] = json!(ts);
    }
    meta(
        &mut h,
        provider,
        "partition",
        &json!({"nativeSession":n["nativeSession"],"namespace":n.get("namespace")}),
    );
    h
}
pub fn finish(spool: &mut Spool, writer: &mut Writer, header: &Value) -> Result<()> {
    if writer.provider == Provider::Devin {
        devin::finish(spool, writer, header)?;
    }
    Ok(())
}
