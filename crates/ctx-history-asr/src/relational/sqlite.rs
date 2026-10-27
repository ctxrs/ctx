//! Read-only native SQLite snapshots. SQL identifiers come only from schema metadata.
use crate::{Error, Result};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};

pub(super) fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-4096; BEGIN;",
    )?;
    Ok(db)
}
pub(super) fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
pub(super) fn columns(db: &Connection, table: &str) -> Result<BTreeSet<String>> {
    Ok(db
        .prepare(&format!("PRAGMA table_info({})", quote(table)))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}
pub(super) fn exists(db: &Connection, table: &str) -> Result<bool> {
    Ok(!columns(db, table)?.is_empty())
}
pub(super) fn visit(
    db: &Connection,
    table: &str,
    filter: Option<(&str, &str)>,
    order: &str,
    omit: &[&str],
    limit: usize,
    visitor: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<()> {
    let names = columns(db, table)?;
    if names.is_empty() {
        return Ok(());
    }
    let selected: Vec<_> = names
        .iter()
        .filter(|n| !omit.contains(&n.as_str()))
        .collect();
    // Preflight each value without asking SQLite to copy oversized TEXT/BLOB values.
    let projection = selected.iter().map(|n| {
        let q=quote(n);
        format!("CASE WHEN typeof({q}) IN ('text','blob') AND octet_length({q})>{limit} THEN NULL ELSE {q} END, CASE WHEN typeof({q}) IN ('text','blob') THEN octet_length({q}) ELSE 0 END")
    }).collect::<Vec<_>>().join(",");
    let where_sql = filter
        .map(|(k, _)| format!(" WHERE {}=?1", quote(k)))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {projection} FROM {}{where_sql} ORDER BY {order}",
        quote(table)
    );
    let mut stmt = db.prepare(&sql)?;
    let mut rows = if let Some((_, v)) = filter {
        stmt.query([v])?
    } else {
        stmt.query([])?
    };
    while let Some(row) = rows.next()? {
        let mut value = json!({});
        for (i, name) in selected.iter().enumerate() {
            let size: i64 = row.get(i * 2 + 1)?;
            if size > limit as i64 {
                value["_oversized"][name.as_str()] = json!(size);
                continue;
            }
            value[name.as_str()] = match row.get_ref(i * 2)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(n) => json!(n),
                ValueRef::Text(v) => json!(std::str::from_utf8(v)
                    .map_err(|_| Error::Invalid("native SQLite text is not UTF-8".into()))?),
                ValueRef::Blob(v) => {
                    use base64::Engine;
                    json!({"encoding":"base64","data":base64::engine::general_purpose::STANDARD.encode(v)})
                }
            };
        }
        visitor(value)?;
    }
    Ok(())
}
pub(super) fn one(
    db: &Connection,
    table: &str,
    key: &str,
    id: &str,
    omit: &[&str],
    limit: usize,
) -> Result<Value> {
    let mut found = None;
    visit(
        db,
        table,
        Some((key, id)),
        &quote(key),
        omit,
        limit,
        &mut |v| {
            if found.is_some() {
                return Err(Error::Invalid(
                    "native session identity is ambiguous".into(),
                ));
            }
            found = Some(v);
            Ok(())
        },
    )?;
    found.ok_or_else(|| {
        Error::Invalid("native session disappeared during export; retry capture".into())
    })
}
