//! Lane/platform state has no inferred session owner. Retain it in manifest blobs.
use super::store;
use crate::{spool::Spool, Options, Provider, Result, Summary};
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

pub fn manifest(
    options: &Options,
    summary: &mut Summary,
    out: &mut impl Write,
    comma: &mut bool,
) -> Result<()> {
    for provider in [Provider::Goose, Provider::Warp] {
        let mut paths = BTreeSet::new();
        for input in options.inputs.iter().filter(|i| i.provider == provider) {
            databases(&input.path, &mut paths)?;
        }
        let mut opened = false;
        for path in paths {
            let native = store::open(&path)?;
            if !store::exists(
                &native,
                if provider == Provider::Goose {
                    "sessions"
                } else {
                    "agent_conversations"
                },
            )? {
                continue;
            }
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
                        summary.source_bytes += row["_bytes"].as_u64().unwrap_or(0);
                        if row["_gap"].is_string() {
                            summary.gaps += 1;
                            return Ok(());
                        }
                        let source = json!({"table":table,"key":row["_rowid"]});
                        if let Some(fields) = row.as_object_mut() {
                            fields.retain(|key, _| !key.starts_with('_'));
                        }
                        let mut entry = json!({"source":source,"record":row});
                        if provider == Provider::Goose {
                            if let Some(ns) = &options.namespace {
                                entry["namespace"] = json!(ns);
                            }
                        }
                        if !ctx_history_asr_core::numbers::safe(&row) {
                            entry["blob"] = ctx_history_asr_core::blobs::store(
                                &options.out,
                                ctx_history_asr_core::numbers::text(&row).as_bytes(),
                            )?;
                            entry.as_object_mut().unwrap().remove("record");
                        }
                        if !opened {
                            if *comma {
                                out.write_all(b",")?;
                            }
                            *comma = true;
                            serde_json::to_writer(
                                &mut *out,
                                &format!("{}/unassigned", provider.domain()),
                            )?;
                            out.write_all(b":[")?;
                            opened = true;
                        } else {
                            out.write_all(b",")?;
                        }
                        serde_json::to_writer(&mut *out, &entry)?;
                        Ok(())
                    },
                )?;
            }
        }
        if opened {
            out.write_all(b"]")?;
        }
    }
    Ok(())
}
fn databases(path: &Path, found: &mut BTreeSet<PathBuf>) -> Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                continue;
            }
            let path = entry.path();
            if path.is_dir()
                || path
                    .extension()
                    .is_some_and(|e| e == "db" || e == "sqlite" || e == "sqlite3")
            {
                databases(&path, found)?;
            }
        }
    } else if path.is_file() {
        found.insert(std::fs::canonicalize(path)?);
    }
    Ok(())
}
