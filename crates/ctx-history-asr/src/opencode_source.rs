//! Native SQLite cursors. One read transaction per database; only bounded rows
//! cross into Rust. Transaction contents are staged on disk, never in a Vec of
//! sessions, and content digests include changes that did not bump native clocks.
mod kiro;
mod opencode;
use crate::{
    records::{copy, meta, session_ref},
    source::Source,
    spool::Spool,
    Error, Input, Provider, Result,
};
use rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

pub(crate) fn visit(
    input: &Input,
    limit: usize,
    staging: &Path,
    namespace: Option<&str>,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let db = Connection::open_with_flags(
        &input.path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-4096; PRAGMA temp_store=FILE; PRAGMA mmap_size=0; BEGIN;")?;
    if input.provider == Provider::Kiro {
        kiro::visit(
            &db,
            input.provider,
            limit,
            staging,
            namespace,
            visitor,
            unidentified,
        )?;
    } else {
        opencode::visit(&db, input.provider, limit, staging, visitor, unidentified)?;
    }
    db.execute_batch("COMMIT")?;
    Ok(())
}

fn table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}
fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut s = db.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
    let result = s
        .query_map([table], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(result)
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The total native row bound is evaluated by SQLite before returning any
/// potentially large value. Table/filter names are converter-owned constants.
fn rows(
    db: &Connection,
    table: &str,
    suffix: &str,
    values: &[&dyn rusqlite::ToSql],
    limit: usize,
    visit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<()> {
    let cols = columns(db, table)?;
    let size = cols
        .iter()
        .map(|c| format!("coalesce(octet_length({}),0)", quote(c)))
        .collect::<Vec<_>>()
        .join("+");
    if cols.is_empty() {
        return Err(Error::Invalid("missing native SQLite table".into()));
    }
    let select = cols
        .iter()
        .map(|c| {
            let identity = matches!(c.as_str(), "id" | "key" | "conversation_id");
            format!(
                "CASE WHEN ({size}) <= {limit} {} THEN {} END",
                if identity {
                    format!("OR octet_length({})<=4096", quote(c))
                } else {
                    String::new()
                },
                quote(c)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let key = if cols.iter().any(|c| c == "id") {
        "id"
    } else {
        "key"
    };
    let key = if cols.iter().any(|c| c == key) {
        format!("CASE WHEN octet_length({key})<=4096 THEN {key} END")
    } else {
        "NULL".into()
    };
    let sql = format!(
        "SELECT ({size}),{key},{select} FROM {} {suffix}",
        quote(table)
    );
    let mut s = db.prepare(&sql)?;
    let mut cursor = s.query(values)?;
    while let Some(r) = cursor.next()? {
        let bytes: u64 = r.get(0)?;
        let mut v = json!({});
        if bytes > limit as u64 {
            v["_gap"] = json!("conversion_loss");
            v["_key"] = json!(r.get::<_, Option<String>>(1)?);
        }
        for (i, c) in cols.iter().enumerate() {
            v[c] = match r.get_ref(i + 2)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(n) => json!(n),
                ValueRef::Text(b) => match std::str::from_utf8(b) {
                    Ok(s) => json!(s),
                    Err(_) => {
                        v["_gap"] = json!("unparseable");
                        Value::Null
                    }
                },
                ValueRef::Blob(b) => json!({"sqliteBlob":b}),
            };
        }
        v["_bytes"] = json!(bytes);
        visit(v)?;
    }
    Ok(())
}

fn data(row: &mut Value, column: &str) -> Option<Value> {
    row.as_object_mut()?
        .remove(column)?
        .as_str()
        .and_then(|s| crate::source::decode(s.as_bytes()))
}

struct Stage {
    file: tempfile::NamedTempFile,
    hash: Sha256,
    provider: Provider,
    session: String,
    header: Value,
}
impl Stage {
    fn new(staging: &Path, provider: Provider, session: &str, header: Value) -> Result<Self> {
        Ok(Self {
            file: tempfile::NamedTempFile::new_in(staging)?,
            hash: Sha256::new(),
            provider,
            session: session.into(),
            header,
        })
    }
    fn push(
        &mut self,
        local: &str,
        kind: &str,
        row: Value,
        body: Value,
        parent: Option<&str>,
    ) -> Result<()> {
        let reason = row.get("_gap").cloned();
        let mut v = json!({"uuid":local,"type":kind,"row":row,"data":body});
        if let Some(parent) = parent {
            v["message_id"] = json!(parent);
        }
        if let Some(reason) = reason {
            v["type"] = json!("_gap");
            v["reason"] = reason;
        }
        let bytes = serde_json::to_vec(&v)?;
        self.hash.update(&bytes);
        self.hash.update([b'\n']);
        self.file.write_all(&bytes)?;
        self.file.write_all(b"\n")?;
        Ok(())
    }
    fn finish(mut self, visitor: &mut dyn FnMut(Source) -> Result<()>) -> Result<()> {
        self.file.flush()?;
        self.hash.update(serde_json::to_vec(&self.header)?);
        let stamp = format!("{:x}", self.hash.finalize());
        let (_, path) = self.file.keep().map_err(|e| e.error)?;
        visitor(Source {
            provider: self.provider,
            path,
            session: self.session.clone(),
            header: self.header,
            stamp,
            segment: self.session,
        })
    }
}

pub(crate) fn header(provider: Provider, id: &str, n: &Value, revision: &str) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":revision,"source":{"format":provider.format()},"lossy":["native","revision_history","display_content","message.author","provider.identity","copies","branches","file_change","_rs.ctx/sqlite_row_limit","_rs.ctx/nontranscript_state","_rs.ctx/vendor_only"]},"profiles":["core","coding","lineage","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]},"extensions":[{"id":format!("{}/sqlite",provider.domain()),"version":"1","required":false}]});
    copy(&mut h["agent"], "version", n, "version");
    if let Some(cwd) = n["cwd"].as_str().filter(|p| p.starts_with('/')) {
        h["workspace"]["roots"]
            .as_array_mut()
            .map(|a| a.push(json!({"id":"workspace","path":cwd})));
    }
    if let Some(t) = millis(&n["time_created"]) {
        h["startedAt"] = json!(t);
    }
    if let Some(p) = n["parent_id"].as_str() {
        h["lineage"] =
            json!({"relatedUnknown":{"session":session_ref(provider,p),"evidence":"native_field"}});
    }
    if let Some(p) = n["fork_session_id"].as_str() {
        h["lineage"]["forkedFrom"] =
            json!({"session":session_ref(provider,p),"evidence":"native_field"});
    }
    meta(&mut h, provider, "session", n);
    if provider == Provider::Kiro {
        h["producer"]["lossy"].as_array_mut().map(|a| {
            a.extend([
                json!("usage"),
                json!("cost"),
                json!("timestamps"),
                json!("inference"),
                json!("_dev.kiro.cli/compaction_boundary"),
                json!("_dev.kiro.cli/context_and_images_in_metadata"),
            ])
        });
    }
    h
}
pub(crate) fn millis(v: &Value) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(v.as_i64()?)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

pub(crate) fn index_calls(spool: &Spool, v: &Value, pos: u64) -> Result<()> {
    fn add(spool: &Spool, v: &Value, pos: u64) -> Result<()> {
        let id = v["callID"]
            .as_str()
            .or_else(|| (v["type"] == "tool").then(|| v["id"].as_str()).flatten());
        if let Some(id) = id {
            spool
                .db
                .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, pos])?;
        }
        Ok(())
    }
    let d = &v["data"];
    if v["type"] == "part" {
        add(spool, d, pos * crate::records::STRIDE)?;
    }
    if v["type"] == "event" {
        for (i, c) in d["content"].as_array().into_iter().flatten().enumerate() {
            add(spool, c, pos * crate::records::STRIDE + i as u64)?;
        }
        if d["type"] == "shell" {
            add(spool, d, pos * crate::records::STRIDE)?;
        }
    }
    if v["type"] == "kiro_entry" {
        for t in d
            .pointer("/assistant/ToolUse/tool_uses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let (i, t) = t;
            if let Some(id) = t["id"].as_str() {
                spool.db.execute(
                    "INSERT INTO native_calls VALUES(?1,?2)",
                    params![id, pos * crate::records::STRIDE + i as u64],
                )?;
            }
        }
    }
    Ok(())
}
