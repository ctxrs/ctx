use crate::{Error, Result};
use rusqlite::{types::ValueRef, Connection, OpenFlags, Params};
use serde_json::{json, Map, Value};
use std::{path::Path, time::Duration};

pub(crate) fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-4096; BEGIN")?;
    Ok(db)
}
pub(crate) fn has(db: &Connection, table: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
pub(crate) fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    // Callers use compile-time table names, never source-controlled SQL.
    let mut stmt = db.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let values = stmt
        .query_map([], |r| r.get(1))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(values)
}
pub(crate) fn scan(
    db: &Connection,
    sql: &str,
    params: impl Params,
    limit: usize,
    mut visit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let mut stmt = db.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
    let mut rows = stmt.query(params)?;
    while let Some(row) = rows.next()? {
        let mut size = 0usize;
        for i in 0..names.len() {
            size = size.saturating_add(match row.get_ref(i)? {
                ValueRef::Text(v) | ValueRef::Blob(v) => v.len(),
                _ => 16,
            });
        }
        if size > limit {
            return Err(Error::Invalid(
                "native SQLite row exceeds max-record-bytes".into(),
            ));
        }
        let mut value = Map::new();
        for (i, name) in names.iter().enumerate() {
            let v = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => json!(std::str::from_utf8(v)
                    .map_err(|_| Error::Invalid("native SQLite text is not UTF-8".into()))?),
                ValueRef::Blob(_) => {
                    return Err(Error::Invalid(
                        "binary column needs its native decoder".into(),
                    ))
                }
            };
            value.insert(name.clone(), v);
        }
        visit(Value::Object(value))?;
    }
    Ok(())
}
pub(crate) fn one(db: &Connection, sql: &str, params: impl Params, limit: usize) -> Result<Value> {
    let mut found = Value::Null;
    scan(db, sql, params, limit, |v| {
        found = v;
        Ok(())
    })?;
    Ok(found)
}
pub(crate) fn safe_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}
