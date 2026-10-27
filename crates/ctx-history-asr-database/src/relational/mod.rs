//! Native relational histories; each observation is a new capture, never a source revision.
mod astrbot;
mod common;
mod hermes;
mod shelley;
mod sqlite;
mod zed;
use crate::{
    records::{session_ref, Writer},
    source::Source,
    spool::Spool,
    Input, Options, Provider, Result,
};
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};
pub const REVISION: &str = "ctx-asr-relational-1";

fn table(provider: Provider) -> (&'static str, &'static str, &'static [&'static str]) {
    match provider {
        Provider::Hermes => ("sessions", "id", &[]),
        Provider::AstrBot => ("conversations", "conversation_id", &["content"]),
        Provider::Shelley => ("conversations", "conversation_id", &[]),
        Provider::Zed => ("threads", "id", &["data"]),
        _ => unreachable!(),
    }
}
fn paths(input: &Input, visitor: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
    if input.path.is_file() {
        return visitor(&fs::canonicalize(&input.path)?);
    }
    if !input.path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&input.path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            paths(
                &Input {
                    provider: input.provider,
                    path: entry.path(),
                },
                visitor,
            )?;
        } else if entry
            .path()
            .extension()
            .is_some_and(|e| e == "db" || e == "sqlite" || e == "sqlite3")
            && entry.path().is_file()
        {
            visitor(&fs::canonicalize(entry.path())?)?;
        }
    }
    Ok(())
}
fn namespace(db: &rusqlite::Connection, p: Provider) -> Result<Value> {
    use rusqlite::OptionalExtension;
    if p == Provider::Hermes && sqlite::exists(db, "state_meta")? {
        let value: Option<String> = db
            .query_row(
                "SELECT value FROM state_meta WHERE key='store_instance_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(json!(value.filter(|v| !v.is_empty())));
    }
    Ok(Value::Null)
}
fn stamp(path: &Path) -> Result<String> {
    let mut stamp = String::new();
    for p in [
        path.to_path_buf(),
        std::path::PathBuf::from(format!("{}-wal", path.display())),
    ] {
        match fs::metadata(p) {
            Ok(m) => stamp.push_str(&format!("{}:{:?};", m.len(), m.modified()?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => stamp.push_str("absent;"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(stamp)
}
pub fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    paths(input, &mut |path| {
        let db = sqlite::open(path)?;
        let (table, key, omit) = table(input.provider);
        let cols = sqlite::columns(&db, table)?;
        if !cols.contains(key) {
            *unidentified += 1;
            return Ok(());
        }
        let stamp = stamp(path)?;
        sqlite::visit(
            &db,
            table,
            None,
            &sqlite::quote(key),
            omit,
            limit,
            &mut |mut header| {
                let Some(id) = header[key]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                else {
                    *unidentified += 1;
                    return Ok(());
                };
                header["_sessionId"] = json!(id);
                header["_namespace"] = namespace(&db, input.provider)?;
                visitor(Source {
                    provider: input.provider,
                    path: path.to_owned(),
                    session: id.clone(),
                    segment: id,
                    header,
                    stamp: stamp.clone(),
                })
            },
        )
    })
}
pub fn storage_key(s: &Source) -> String {
    if s.provider.is_relational() && s.header["_namespace"].is_string() {
        json!([s.header["_namespace"], s.session]).to_string()
    } else {
        s.session.clone()
    }
}
pub fn reference(provider: Provider, h: &Value, id: &str) -> String {
    let encoded = session_ref(provider, id);
    if let Some(namespace) = h["_namespace"].as_str() {
        let encoded_namespace = session_ref(provider, namespace);
        format!(
            "{}:{}:{}",
            provider.agent(),
            encoded_namespace.split_once(':').unwrap().1,
            encoded.split_once(':').unwrap().1
        )
    } else {
        encoded
    }
}
pub fn ingest(spool: &mut Spool, p: Provider, path: &Path, source: &Value) -> Result<()> {
    let db = sqlite::open(path)?;
    let (table, key, omit) = table(p);
    let id = source["_sessionId"].as_str().unwrap_or("");
    let mut header = sqlite::one(&db, table, key, id, omit, spool.limit)?;
    header["_sessionId"] = json!(id);
    header["_namespace"] = source["_namespace"].clone();
    match p {
        Provider::Hermes => hermes::ingest(&db, spool, &mut header)?,
        Provider::AstrBot => astrbot::ingest(&db, spool, &mut header)?,
        Provider::Shelley => shelley::ingest(&db, spool, &mut header)?,
        Provider::Zed => zed::ingest(&db, spool, &mut header)?,
        _ => unreachable!(),
    }
    // Forward announcements come only from the provider's explicit parent field.
    let parent = match p {
        Provider::Hermes => Some("parent_session_id"),
        Provider::Shelley => Some("parent_conversation_id"),
        Provider::Zed => Some("parent_id"),
        _ => None,
    };
    if let Some(parent) =
        parent.filter(|p| sqlite::columns(&db, table).is_ok_and(|c| c.contains(*p)))
    {
        sqlite::visit(
            &db,
            table,
            Some((parent, id)),
            &sqlite::quote(key),
            omit,
            spool.limit,
            &mut |n| {
                if p == Provider::Hermes
                    && common::decoded(&n["model_config"])["_delegate_from"] != id
                {
                    return Ok(());
                }
                if let Some(child) = n[key].as_str() {
                    spool.push_relational(
                        "subagent",
                        &format!("child:{child}"),
                        &json!({"child":child,"parentField":parent}),
                        false,
                    )?;
                }
                Ok(())
            },
        )?;
    }
    spool.save("native_header", &header)
}
pub fn header(p: Provider, h: &Value) -> Value {
    let mut v = json!({"type":"session","formatVersion":"0.1","agent":{"name":p.agent()},"sessionId":h["_sessionId"],"namespace":h["_namespace"],"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":REVISION,"source":{"format":p.format()},"lossy":["revision_history","order","message.author","inference","tool_result.raw","exec.exit_code","provider.identity","_rs.ctx/native_value_limit","_rs.ctx/vendor_only"]},"profiles":["core","lineage","usage","messaging"],"contentLevel":"full"});
    match p {
        Provider::Hermes => hermes::header(h, &mut v),
        Provider::AstrBot => astrbot::header(h, &mut v),
        Provider::Shelley => shelley::header(h, &mut v),
        Provider::Zed => zed::header(h, &mut v),
        _ => unreachable!(),
    }
    v
}
pub fn convert(s: &mut Spool, w: &mut Writer, h: &Value, row: &Value) -> Result<()> {
    let key = row["key"].as_str().unwrap_or("");
    w.capture_scope = row["capture_scope"] == true;
    // Extracted transcript items retain native source positions even when their
    // containing history is rewritten. Only mutable side-state rows form the tail.
    w.native_snapshot = matches!(
        row["kind"].as_str(),
        Some(
            "session"
                | "subagent"
                | "session_model_usage"
                | "session_admissions"
                | "worker_executions"
        )
    );
    // A capture-scoped position may reuse an ID in another capture. Never use
    // a source path, file stamp, content hash or export time to derive an ID.
    w.native_identity = Some(key.to_owned());
    if row["kind"] == "subagent" {
        common::emit(
            s,
            w,
            json!({"type":"subagent","childSession":reference(w.provider,h,row["data"]["child"].as_str().unwrap())}),
            &row["data"],
        )?;
        return Ok(());
    }
    match w.provider {
        Provider::Hermes => hermes::convert(s, w, h, row),
        Provider::AstrBot => astrbot::convert(s, w, h, row),
        Provider::Shelley => shelley::convert(s, w, h, row),
        Provider::Zed => zed::convert(s, w, h, row),
        _ => unreachable!(),
    }
}
pub fn bundle_facts(options: &Options, out: &mut dyn Write, comma: &mut bool) -> Result<()> {
    // Session filtering cannot assign lane records by copied checkpoints, time or text.
    let mut seen = std::collections::BTreeSet::new();
    for (provider, table, order) in [
        (Provider::Hermes, "delivery_obligations", "obligation_id"),
        (Provider::AstrBot, "platform_message_history", "id"),
    ] {
        let mut first_row = true;
        for input in options.inputs.iter().filter(|i| i.provider == provider) {
            paths(input, &mut |path| {
                if !seen.insert((provider.agent(), path.to_owned())) {
                    return Ok(());
                }
                let db = sqlite::open(path)?;
                let ns = options
                    .namespace
                    .as_ref()
                    .map(|v| json!(v))
                    .unwrap_or(namespace(&db, provider)?);
                sqlite::visit(
                    &db,
                    table,
                    None,
                    order,
                    &[],
                    options.max_record_bytes,
                    &mut |row| {
                        if first_row {
                            if *comma {
                                out.write_all(b",")?;
                            }
                            *comma = true;
                            serde_json::to_writer(
                                &mut *out,
                                &format!("{}/unassigned", provider.domain()),
                            )?;
                            out.write_all(b":[")?;
                            first_row = false;
                        } else {
                            out.write_all(b",")?;
                        }
                        let mut entry = json!({"source":{"table":table},"record":row});
                        if !ns.is_null() {
                            entry["namespace"] = ns.clone();
                        }
                        if !ctx_history_asr_core::numbers::safe(&row) {
                            entry["blob"] = ctx_history_asr_core::blobs::store(
                                &options.out,
                                ctx_history_asr_core::numbers::text(&row).as_bytes(),
                            )?;
                            entry.as_object_mut().unwrap().remove("record");
                        }
                        serde_json::to_writer(&mut *out, &entry)?;
                        Ok(())
                    },
                )
            })?;
        }
        if !first_row {
            out.write_all(b"]")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
