//! Unowned database facts belong to the manifest, never the nearest session.
use super::sqlite;
use crate::{spool::Spool, Options, Provider, Result};
use rusqlite::Connection;
use serde_json::json;
use std::{io::Write, path::Path};

pub fn write(
    db: &Connection,
    options: &Options,
    out: &mut impl Write,
    comma: &mut bool,
) -> Result<()> {
    let mut sources = db.prepare("SELECT agent,path FROM native_databases ORDER BY agent,path")?;
    let mut rows = sources.query([])?;
    let mut current = String::new();
    let mut first_row = true;
    while let Some(row) = rows.next()? {
        let agent: String = row.get(0)?;
        let path: String = row.get(1)?;
        let provider = match agent.as_str() {
            "com.firebender" => Provider::Firebender,
            "dev.forgecode" => Provider::ForgeCode,
            "com.langchain.deepagents" => Provider::DeepAgents,
            _ => Provider::Devin,
        };
        if agent != current {
            if !current.is_empty() {
                out.write_all(b"],")?;
            } else if *comma {
                out.write_all(b",")?;
            }
            *comma = true;
            serde_json::to_writer(&mut *out, &format!("{}/unassigned", provider.domain()))?;
            out.write_all(b":[")?;
            current = agent;
            first_row = true;
        }
        let mut spool = Spool::new(&options.out, options.max_record_bytes)?;
        sqlite::read(Path::new(&path), |native| {
            let tables: &[&str] = match provider {
                Provider::Firebender => &["schema_info"],
                Provider::Devin => &["app_state", "refinery_schema_history"],
                _ => &[],
            };
            for table in std::iter::once("sqlite_schema").chain(tables.iter().copied()) {
                sqlite::rows(
                    native,
                    table,
                    "",
                    "rowid",
                    &[],
                    spool.limit,
                    &mut |mut row| {
                        // App state can contain JSON, including secret questions.
                        if table == "app_state" {
                            if let Ok(value) = sqlite::json_field(&row, "value") {
                                if !value.is_null() {
                                    row["value"] = value;
                                }
                            }
                        }
                        let v = json!({"table":table,"row":row});
                        crate::privacy::questions(&spool, &v)?;
                        spool.push_native(&v)
                    },
                )?;
            }
            Ok(())
        })?;
        crate::privacy::prepare(&mut spool)?;
        let mut pos = 0;
        while let Some(next) = spool.next(pos)? {
            pos = next;
            let v = spool.get(pos)?;
            let mut entry = json!({"source":{"table":v["table"]},"record":v["row"]});
            if !ctx_history_asr_core::numbers::safe(&v["row"]) {
                entry["blob"] = ctx_history_asr_core::blobs::store(
                    &options.out,
                    ctx_history_asr_core::numbers::text(&v["row"]).as_bytes(),
                )?;
                entry.as_object_mut().unwrap().remove("record");
            }
            if !first_row {
                out.write_all(b",")?;
            }
            first_row = false;
            serde_json::to_writer(&mut *out, &entry)?;
        }
        spool.finish()?;
    }
    if !current.is_empty() {
        out.write_all(b"]")?;
    }
    Ok(())
}
