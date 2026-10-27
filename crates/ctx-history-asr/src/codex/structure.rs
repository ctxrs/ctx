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
    let ordinal: Option<u64> = spool.db.query_row("SELECT ordinal FROM rows WHERE segment=?1 AND ordinal<?2 AND pos<?3 ORDER BY ordinal DESC LIMIT 1",params![base,end,writer.pos],|r|r.get(0)).optional()?;
    let tip = ordinal
        .map(|n| spool.entity(&format!("tip:{base}:{n}")))
        .transpose()?
        .flatten()
        .unwrap_or(Value::Null);
    writer.emit(
        json!({"type":"rewind","to":tip,"reason":"rollback"}),
        source,
    )?;
    Ok(())
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
            break;
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
