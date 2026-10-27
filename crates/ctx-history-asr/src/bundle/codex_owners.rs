//! Bind causal roots to observed native turn owners, independently of spawn trees.
use crate::{source, spool::Spool, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) fn prepare(db: &Connection, limit: usize) -> Result<String> {
    db.execute_batch("CREATE TEMP TABLE codex_turn_owners(turn TEXT,session TEXT,PRIMARY KEY(turn,session)); BEGIN;")?;
    let mut sources = db.prepare(
        "SELECT session,path,header FROM sources WHERE agent='codex' ORDER BY session,path",
    )?;
    let mut rows = sources.query([])?;
    let mut bytes = Vec::new();
    while let Some(row) = rows.next()? {
        let session: String = row.get(0)?;
        let path: String = row.get(1)?;
        let header: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
        let copied = header["forked_from_id"].is_string() && header["history_base"].is_null();
        let cut = copied
            .then(|| {
                header["subagent_history_start_ordinal"]
                    .as_u64()
                    .or_else(|| header["forked_from_ordinal_exclusive"].as_u64())
            })
            .flatten();
        let mut owner = !copied;
        let mut input = source::reader(Path::new(&path))?;
        while let Some((_, large)) = source::line(&mut *input, &mut bytes, limit)? {
            if large {
                continue;
            }
            // Most rollout bytes are message bodies. Avoid decoding those merely
            // to collect the small ownership index. This never indexes text IDs.
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if !text.contains("\"turn_id\"")
                && !text.contains("\"turnId\"")
                && !text.contains("\"session_meta\"")
            {
                continue;
            }
            let Some(v) = source::decode(&bytes) else {
                continue;
            };
            let p = &v["payload"];
            if v["type"] == "session_meta" {
                owner = p["id"] == session;
                continue;
            }
            if !owner || cut.is_some_and(|cut| v["ordinal"].as_u64().is_none_or(|n| n < cut)) {
                continue;
            }
            if matches!(
                v["type"].as_str(),
                Some("turn_context" | "event_msg" | "token_usage_record")
            ) {
                if let Some(turn) = p["turn_id"].as_str().or_else(|| p["turnId"].as_str()) {
                    db.execute(
                        "INSERT OR IGNORE INTO codex_turn_owners VALUES(?1,?2)",
                        params![turn, session],
                    )?;
                }
            }
        }
    }
    db.execute_batch("COMMIT")?;
    // Ownership is a bundle dependency: a newly available or ambiguous owner
    // must invalidate previously exported root references, including no-op files.
    let mut hash = Sha256::new();
    let mut stmt =
        db.prepare("SELECT turn,session FROM codex_turn_owners ORDER BY turn,session")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        for i in 0..2 {
            hash.update(row.get::<_, String>(i)?.as_bytes());
            hash.update([0]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn bind(db: &Connection, spool: &Spool, v: &Value) -> Result<()> {
    let p = &v["payload"];
    if v["type"] == "event_msg" {
        if let Some(root) = p["root_turn_id"].as_str().or_else(|| {
            p.pointer("/turn_attribution/root_turn_id")
                .and_then(Value::as_str)
        }) {
            let owner: Option<String> = db
                .query_row(
                    "SELECT min(session) FROM codex_turn_owners WHERE turn=?1 HAVING count(*)=1",
                    [root],
                    |r| r.get(0),
                )
                .optional()?;
            spool.save(&format!("codex:root:{root}"), &json!(owner))?;
        }
    }
    Ok(())
}
