//! Lane/platform state has no inferred session owner. Retain it in manifest blobs.
use super::{common, store};
use crate::{spool::Spool, Options, Provider, Result, Summary};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{io::Write, path::Path};

pub fn manifest(db: &Connection, options: &Options, summary: &mut Summary) -> Result<Value> {
    let mut metadata = json!({});
    let mut stmt=db.prepare("SELECT DISTINCT agent,path FROM sources WHERE agent IN ('goose','warp') ORDER BY agent,path")?;
    let mut sources = stmt.query([])?;
    while let Some(source) = sources.next()? {
        let agent: String = source.get(0)?;
        let path: String = source.get(1)?;
        let provider = if agent == "goose" {
            Provider::Goose
        } else {
            Provider::Warp
        };
        let native = store::open(Path::new(&path))?;
        let spool = Spool::new(&options.out, options.max_record_bytes)?;
        if provider == Provider::Goose && store::exists(&native, "thread_messages")? {
            for table in ["messages", "thread_messages"] {
                store::rows(
                    &native,
                    table,
                    "?1=''",
                    "",
                    "rowid",
                    options.max_record_bytes,
                    |mut row| {
                        row["_table"] = json!("secret_schema_index");
                        super::goose::index_calls(&spool, &row)
                    },
                )?;
            }
        }
        let tables: &[&str] = if provider == Provider::Goose {
            &["threads", "thread_messages"]
        } else {
            &["ai_queries"]
        };
        let mut file = tempfile::NamedTempFile::new_in(&options.out)?;
        let mut count = 0;
        let mut gaps = 0;
        for table in tables {
            store::rows(
                &native,
                table,
                if *table == "thread_messages" {
                    "?1='' AND (session_id IS NULL OR session_id NOT IN (SELECT id FROM sessions))"
                } else if *table == "ai_queries" {
                    "?1='' AND (conversation_id IS NULL OR conversation_id NOT IN (SELECT conversation_id FROM agent_conversations))"
                } else {
                    "?1=''"
                },
                "",
                "rowid",
                options.max_record_bytes,
                |mut row| {
                    if provider == Provider::Goose {
                        super::goose::redact(&spool, &mut row)?;
                    }
                    let mut out = json!({});
                    common::row_meta(&spool, &mut out, provider, &row)?;
                    if row["_gap"].is_string() {
                        out["gap"] = row["_gap"].clone();
                        gaps += 1;
                    }
                    serde_json::to_writer(file.as_file_mut(), &out)?;
                    file.write_all(b"\n")?;
                    count += 1;
                    summary.source_bytes += row["_bytes"].as_u64().unwrap_or(0);
                    Ok(())
                },
            )?;
        }
        if count > 0 {
            let mut blob = spool.blob(file)?;
            blob["mimeType"] = json!("application/x-ndjson");
            let key = format!("{}/unassignedContext", provider.domain());
            if metadata.get(&key).is_none() {
                metadata[&key] = json!([]);
            }
            let mut descriptor = json!({"blob":blob,"rows":count,"gaps":gaps,"tables":tables});
            if provider == Provider::Goose {
                descriptor["namespace"] = json!(options.namespace);
            }
            if let Some(entries) = metadata[&key].as_array_mut() {
                entries.push(descriptor);
            }
        }
    }
    Ok(metadata)
}
