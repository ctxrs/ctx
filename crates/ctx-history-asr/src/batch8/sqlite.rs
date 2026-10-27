//! Bounded, read-only native SQLite cursors. No Core projection is involved.
use crate::{Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};
use std::path::Path;

pub(super) fn read<T>(path: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    use ctx_history_source_sqlite::{
        open_root_handle_sqlite_source_snapshot_with_limits,
        retain_sqlite_source_directory_authority, SqliteSourceSnapshotLimits,
    };
    let scratch = tempfile::tempdir()?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("database has no parent".into()))?;
    let root = ctx_history_source_io::ProviderSourceRoot::open(parent)?;
    let handle = root.directory()?.try_clone_authority_handle()?;
    let authority = retain_sqlite_source_directory_authority(scratch.path(), &handle, parent)?;
    let snapshot = open_root_handle_sqlite_source_snapshot_with_limits(
        &authority,
        path.file_name()
            .ok_or_else(|| Error::Invalid("database has no name".into()))?,
        SqliteSourceSnapshotLimits::new(60 * 1024 * 1024 * 1024),
    )?;
    let result = f(snapshot.connection()?)?;
    snapshot.finish()?;
    Ok(result)
}
pub(super) fn exists(db: &Connection, table: &str) -> Result<bool> {
    if table == "sqlite_schema" {
        return Ok(true);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Admission happens in SQL, before SQLite transfers any unbounded payload.
/// The bound covers the whole row, not each field independently.
pub(super) fn rows(
    db: &Connection,
    table: &str,
    selection: &str,
    order: &str,
    args: &[&str],
    limit: usize,
    emit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<()> {
    if !exists(db, table)? {
        return Ok(());
    }
    let columns = db
        .prepare(&format!("PRAGMA table_info({})", quote(table)))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let length = columns
        .iter()
        .map(|c| format!("coalesce(length(cast({} as blob)),0)", quote(c)))
        .collect::<Vec<_>>()
        .join("+");
    let selected = columns
        .iter()
        .map(|c| format!("CASE WHEN ({length})<=?1 THEN {} END", quote(c)))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT rowid,({length}),{selected} FROM {} {selection} ORDER BY {order}",
        quote(table)
    );
    let mut params = vec![rusqlite::types::Value::Integer(limit as i64)];
    params.extend(
        args.iter()
            .map(|s| rusqlite::types::Value::Text((*s).into())),
    );
    let mut stmt = db.prepare(&sql)?;
    let mut cursor = stmt.query(rusqlite::params_from_iter(params))?;
    while let Some(row) = cursor.next()? {
        let mut value = Map::new();
        value.insert("_rowid".into(), json!(row.get::<_, i64>(0)?));
        if row.get::<_, u64>(1)? > limit as u64 {
            value.insert("_oversized".into(), json!(true));
        } else {
            for (i, name) in columns.iter().enumerate() {
                let v = match row.get_ref(i + 2)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) if n.is_finite() => json!(n),
                    ValueRef::Real(_) => json!({"sqliteType":"real","nonfinite":true}),
                    ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                        Ok(s) => json!(s),
                        Err(_) => json!({"sqliteType":"text","base64":STANDARD.encode(bytes)}),
                    },
                    ValueRef::Blob(bytes) => {
                        json!({"sqliteType":"blob","base64":STANDARD.encode(bytes)})
                    }
                };
                value.insert(name.clone(), v);
            }
        }
        emit(Value::Object(value))?;
    }
    Ok(())
}
pub(super) fn json_field(row: &Value, field: &str) -> Result<Value> {
    let Some(raw) = row[field].as_str() else {
        return Ok(Value::Null);
    };
    exact_json(raw).ok_or_else(|| Error::Invalid(format!("invalid or inexact JSON in {field}")))
}
pub(super) fn blob(row: &Value, field: &str) -> Result<Vec<u8>> {
    let raw = row[field]["base64"]
        .as_str()
        .ok_or_else(|| Error::Invalid(format!("missing {field} blob")))?;
    STANDARD
        .decode(raw)
        .map_err(|_| Error::Invalid("invalid internal blob encoding".into()))
}
pub(super) fn stamp(path: &Path) -> Result<String> {
    let mut stamp = String::new();
    for p in [
        path.to_owned(),
        path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap_or_default().to_string_lossy()
        )),
    ] {
        match std::fs::metadata(p) {
            Ok(m) => stamp.push_str(&format!("{}:{:?};", m.len(), m.modified()?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => stamp.push_str("absent;"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(stamp)
}

/// Reject integer lexemes serde_json would silently convert to a rounded float.
/// The original SQLite string remains available to the gap/vendor projection.
pub(crate) fn exact_json(raw: &str) -> Option<Value> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if bytes[i] == b'-' || bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && matches!(bytes[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                i += 1;
            }
            let token = &raw[start..i];
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return None;
            }
        } else {
            i += 1;
        }
    }
    ctx_history_capture_model::exact_json_value(raw)
}
