//! Unowned database facts belong to the manifest, never the nearest session.
use super::{sqlite, values};
use crate::{spool::Spool, Options, Provider, Result};
use rusqlite::Connection;
use serde_json::json;
use std::{io::Write, path::Path};

pub(crate) fn write(db: &Connection, options: &Options, out: &mut impl Write) -> Result<()> {
    let mut sources = db.prepare("SELECT agent,path FROM native_databases ORDER BY agent,path")?;
    let mut rows = sources.query([])?;
    let mut first = true;
    while let Some(row) = rows.next()? {
        let agent: String = row.get(0)?;
        let path: String = row.get(1)?;
        let provider = match agent.as_str() {
            "com.firebender" => Provider::Firebender,
            "dev.forgecode" => Provider::ForgeCode,
            "com.langchain.deepagents" => Provider::DeepAgents,
            _ => Provider::Devin,
        };
        if first {
            out.write_all(b",\"_meta\":{\"rs.ctx/sqliteApplicationState\":[")?;
            first = false;
        } else {
            out.write_all(b",")?;
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
        write!(
            out,
            "{{\"agent\":{},\"rows\":[",
            serde_json::to_string(provider.agent())?
        )?;
        let mut pos = 0;
        let mut comma = false;
        while let Some(next) = spool.next(pos)? {
            pos = next;
            let v = spool.get(pos)?;
            let mut entry = json!({"table":v["table"]});
            values::preserve(&spool, &mut entry, provider, "row", &v["row"])?;
            if comma {
                out.write_all(b",")?;
            }
            comma = true;
            serde_json::to_writer(&mut *out, &entry)?;
        }
        spool.finish()?;
        out.write_all(b"]}")?;
    }
    if !first {
        out.write_all(b"]}")?;
    }
    Ok(())
}
