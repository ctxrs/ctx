//! Resolve a Claude native prefix boundary to the actual last emitted record.
use crate::{spool::Spool, Provider, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
pub(super) fn prepare(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TEMP TABLE pending_sessions AS SELECT DISTINCT agent,session FROM sources;
        CREATE UNIQUE INDEX pending_identity ON pending_sessions(agent,session);
        CREATE TEMP TABLE dependencies(agent TEXT,session TEXT,parent TEXT,PRIMARY KEY(agent,session));
        CREATE TABLE IF NOT EXISTS projected_boundaries(agent TEXT,session TEXT,native TEXT,record TEXT,PRIMARY KEY(agent,session,native));")?;
    Ok(())
}
pub(super) fn next(db: &Connection) -> Result<Option<(String, String)>> {
    Ok(db.query_row("SELECT p.agent,p.session FROM pending_sessions p ORDER BY EXISTS(SELECT 1 FROM dependencies d JOIN pending_sessions parent ON parent.agent=d.agent AND parent.session=d.parent WHERE d.agent=p.agent AND d.session=p.session),p.agent,p.session LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
}
pub(super) fn defer(
    db: &Connection,
    p: Provider,
    session: &str,
    header: &mut Value,
) -> Result<bool> {
    if p != Provider::Claude {
        return Ok(false);
    }
    let Some(parent) = header
        .pointer("/_forkRef/parentSessionId")
        .and_then(Value::as_str)
    else {
        return Ok(false);
    };
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM pending_sessions WHERE agent=?1 AND session=?2) AND NOT EXISTS(SELECT 1 FROM dependencies WHERE agent=?1 AND session=?3)",params![p.agent(),parent,session],|r|r.get(0))?;
    if pending && parent != session {
        db.execute(
            "INSERT INTO dependencies VALUES(?1,?2,?3)",
            params![p.agent(), session, parent],
        )?;
        return Ok(true);
    }
    if let Some(native) = header
        .pointer("/_forkRef/parentLastUuid")
        .and_then(Value::as_str)
    {
        let record:Option<String>=db.query_row("SELECT record FROM projected_boundaries WHERE agent=?1 AND session=?2 AND native=?3",params![p.agent(),parent,native],|r|r.get(0)).optional()?;
        if let Some(record) = record {
            header["_forkThroughId"] = json!(record);
        }
    }
    Ok(false)
}
pub(super) fn remember(db: &Connection, spool: &Spool, p: Provider, session: &str) -> Result<()> {
    if p != Provider::Claude {
        return Ok(());
    }
    db.execute(
        "DELETE FROM projected_boundaries WHERE agent=?1 AND session=?2",
        params![p.agent(), session],
    )?;
    let mut stmt = spool.db.prepare(
        "SELECT substr(key,10),json_extract(value,'$') FROM entities WHERE key LIKE 'boundary:%'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        db.execute(
            "INSERT INTO projected_boundaries VALUES(?1,?2,?3,?4)",
            params![
                p.agent(),
                session,
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            ],
        )?;
    }
    Ok(())
}
