use crate::{records::Writer, spool::Spool, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn segment(spool: &Spool, writer: &mut Writer, id: &str, source: &Value) -> Result<()> {
    let Some(header) = spool.entity(&format!("segment:{id}"))? else {
        return Ok(());
    };
    let Some(base) = header
        .pointer("/history_base/thread_id")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if spool.entity(&format!("segment:{base}"))?.is_none() {
        return Ok(());
    }
    let Some(end) = header
        .pointer("/history_base/end_ordinal_exclusive")
        .and_then(Value::as_u64)
    else {
        return Ok(());
    };
    let Some(tip) = prefix_tip(spool, base, end, writer.pos)? else {
        return writer.vendor(source, "unresolved_history_base");
    };
    writer.emit(
        json!({"type":"rewind","to":tip,"reason":"rollback"}),
        source,
    )?;
    Ok(())
}

/// Follow the named segment's retained prefix, never a numeric ordinal in another segment.
fn prefix_tip(spool: &Spool, base: &str, end: u64, before: u64) -> Result<Option<Value>> {
    let mut base = base.to_owned();
    let mut end = end;
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(base.clone()) {
        if end == 0 {
            return Ok(Some(Value::Null));
        }
        let ordinal: Option<u64> = spool.db.query_row("SELECT ordinal FROM rows WHERE segment=?1 AND ordinal<?2 AND pos<?3 ORDER BY ordinal DESC LIMIT 1",params![base,end,before],|r|r.get(0)).optional()?;
        if let Some(n) = ordinal {
            if let Some(tip) = spool.entity(&format!("tip:{base}:{n}"))? {
                return Ok(Some(tip));
            }
        }
        let Some(header) = spool.entity(&format!("segment:{base}"))? else {
            return Ok(None);
        };
        let Some(parent) = header
            .pointer("/history_base/thread_id")
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(boundary) = header
            .pointer("/history_base/end_ordinal_exclusive")
            .and_then(Value::as_u64)
        else {
            return Ok(None);
        };
        end = end.min(boundary);
        base = parent.to_owned();
    }
    Ok(None)
}

pub(crate) fn track(spool: &Spool, writer: &Writer) -> Result<()> {
    let Some(tip) = writer.tip.as_deref() else {
        spool.db.execute("DELETE FROM codex_active", [])?;
        return Ok(());
    };
    let old: Option<u64> = spool
        .db
        .query_row("SELECT pos FROM codex_active WHERE record=?1", [tip], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(pos) = old {
        spool
            .db
            .execute("DELETE FROM codex_active WHERE pos>?1", [pos])?;
    } else {
        spool.db.execute(
            "INSERT OR REPLACE INTO codex_active VALUES(?1,?2)",
            params![writer.pos, tip],
        )?;
    }
    Ok(())
}
pub(super) fn retained(spool: &Spool, native: &str) -> Result<Option<String>> {
    let count: u64 = spool.db.query_row(
        "SELECT count(*) FROM rows WHERE native_id=?1 AND kind='response_item'",
        [native],
        |r| r.get(0),
    )?;
    if count != 1 {
        return Ok(None);
    }
    let Some(id) = spool.output(native)? else {
        return Ok(None);
    };
    let active: bool = spool.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM codex_active WHERE record=?1)",
        [&id],
        |r| r.get(0),
    )?;
    Ok(active.then_some(id))
}

pub(super) fn rollback(
    spool: &Spool,
    writer: &mut Writer,
    p: &Value,
    source: &Value,
) -> Result<()> {
    let Some(turns) = p["num_turns"].as_u64() else {
        return writer.vendor(source, "thread_rolled_back");
    };
    if turns == 0 {
        return writer.vendor(source, "thread_rolled_back");
    }
    let mut target = Value::Null;
    for _ in 0..turns {
        let row: Option<(u64, String)> = spool
            .db
            .query_row(
                "SELECT position,tip FROM turns ORDER BY position DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((pos, tip)) = row else {
            return writer.vendor(source, "unresolved_rollback");
        };
        target = serde_json::from_str(&tip)?;
        spool
            .db
            .execute("DELETE FROM turns WHERE position=?1", [pos])?;
    }
    writer.emit(
        json!({"type":"rewind","to":target,"reason":"rollback"}),
        source,
    )?;
    Ok(())
}
