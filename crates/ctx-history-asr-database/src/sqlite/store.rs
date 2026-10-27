use crate::{source::Source, spool::Spool, Error, Input, Provider, Result};
use rusqlite::{params, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{fs, path::Path};

pub(super) fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA cache_size=-4096; PRAGMA temp_store=FILE; BEGIN;",
    )?;
    Ok(db)
}
pub(super) fn exists(db: &Connection, table: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
fn stamp(path: &Path) -> Result<String> {
    let mut stamp = String::new();
    for path in [
        path.to_owned(),
        path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap_or_default().to_string_lossy()
        )),
    ] {
        match fs::metadata(path) {
            Ok(m) => stamp.push_str(&format!("{}:{:?};", m.len(), m.modified()?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => stamp.push_str("absent;"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(stamp)
}
pub fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if !input.path.exists() {
        return Ok(());
    }
    if input.path.is_dir() {
        for entry in fs::read_dir(&input.path)? {
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
                visit(
                    &Input {
                        provider: input.provider,
                        path,
                    },
                    limit,
                    visitor,
                    unidentified,
                )?;
            }
        }
        return Ok(());
    }
    let path = fs::canonicalize(&input.path)?;
    let db = open(&path)?;
    let (table, id) = match input.provider {
        Provider::Crush | Provider::Goose => ("sessions", "id"),
        Provider::Lingma => ("chat_record", "session_id"),
        Provider::Warp => ("agent_conversations", "conversation_id"),
        _ => return Err(Error::Invalid("not a SQLite provider".into())),
    };
    if !exists(&db, table)? {
        *unidentified += 1;
        return Ok(());
    }
    let stamp = stamp(&path)?;
    let mut stmt = db.prepare(&format!("SELECT DISTINCT {id} FROM {table} ORDER BY {id}"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?;
        let ValueRef::Text(bytes) = id else {
            *unidentified += 1;
            continue;
        };
        let Ok(session) = std::str::from_utf8(bytes) else {
            *unidentified += 1;
            continue;
        };
        if session.is_empty() || session.len() > limit {
            *unidentified += 1;
            continue;
        }
        visitor(Source {
            provider: input.provider,
            path: path.clone(),
            session: session.to_owned(),
            header: json!({}),
            stamp: stamp.clone(),
            segment: session.to_owned(),
        })?;
    }
    Ok(())
}
pub fn namespace(db: &Connection, namespace: Option<&str>) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS asr_source_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
    )?;
    if let Some(value) = namespace {
        if value.is_empty() {
            return Err(Error::Invalid("ASR namespace must not be empty".into()));
        }
        let old: Option<String> = db
            .query_row(
                "SELECT value FROM asr_source_settings WHERE key='goose_namespace'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if old.as_deref().is_some_and(|old| old != value) {
            return Err(Error::Invalid("this destination belongs to a different Goose namespace; select a separate destination".into()));
        }
        db.execute(
            "INSERT OR IGNORE INTO asr_source_settings VALUES('goose_namespace',?1)",
            [value],
        )?;
    }
    Ok(())
}
pub fn check_source(db: &Connection, source: &Source, namespace: Option<&str>) -> Result<()> {
    if source.provider == Provider::Goose && namespace.is_none() {
        return Err(Error::Invalid("Goose session counters require --namespace with a stable installation/profile identifier; reuse it across exports and relocation".into()));
    }
    let other: Option<String> = db
        .query_row(
            "SELECT path FROM sources WHERE agent=?1 AND session=?2 AND path<>?3 LIMIT 1",
            params![
                source.provider.agent(),
                source.session,
                source.path.to_string_lossy()
            ],
            |r| r.get(0),
        )
        .optional()?;
    if other.is_some() {
        return Err(Error::Invalid("colliding native SQLite session identities in different databases; export each installation separately".into()));
    }
    Ok(())
}
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
/// Preflight every cell before hydrating a row. One row, never a whole database, enters memory.
pub(super) fn rows(
    db: &Connection,
    table: &str,
    predicate: &str,
    session: &str,
    order: &str,
    limit: usize,
    mut visitor: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    if !exists(db, table)? {
        return Ok(());
    }
    let mut stmt = db.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
    let columns = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let size = columns
        .iter()
        .map(|c| format!("coalesce(length(CAST({} AS BLOB)),0)", quote(c)))
        .collect::<Vec<_>>()
        .join("+");
    let projections = columns
        .iter()
        .map(|c| format!("CASE WHEN ({size})<=?2 THEN {} END", quote(c)))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT rowid,({size}),{projections} FROM {} WHERE {predicate} ORDER BY {order}",
        quote(table)
    );
    let mut stmt = db.prepare(&sql)?;
    let mut cursor = stmt.query(params![session, limit as u64])?;
    while let Some(row) = cursor.next()? {
        let mut v = json!({"_table":table,"_rowid":row.get::<_,i64>(0)?.to_string(),"_bytes":row.get::<_,u64>(1)?});
        if row.get::<_, u64>(1)? > limit as u64 {
            v["_gap"] = json!("conversion_loss");
        } else {
            for (i, col) in columns.iter().enumerate() {
                let value = match row.get_ref(i + 2)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(b) => match std::str::from_utf8(b) {
                        Ok(s) => json!(s),
                        Err(_) => {
                            v["_gap"] = json!("unparseable");
                            continue;
                        }
                    },
                    ValueRef::Blob(b) => {
                        // Warp tasks are consumed below as exact bytes; hex is an internal spool encoding.
                        json!({"sqliteBlobHex":b.iter().map(|x|format!("{x:02x}")).collect::<String>()})
                    }
                };
                v[col] = value;
            }
        }
        visitor(v)?;
    }
    Ok(())
}
pub fn ingest(spool: &mut Spool, path: &Path, provider: Provider, session: &str) -> Result<()> {
    let db = open(path)?;
    let mut pos = 0;
    let (table, column) = if provider == Provider::Warp {
        ("agent_conversations", "conversation_id")
    } else {
        ("sessions", "id")
    };
    let mut header = json!({});
    if provider != Provider::Lingma {
        rows(
            &db,
            table,
            &format!("{column}=?1"),
            session,
            "rowid",
            spool.limit,
            |row| {
                header = row;
                Ok(())
            },
        )?;
    }
    spool.save("sqlite:header", &header)?;
    if provider == Provider::Goose
        && exists(&db, "usage_ledger")?
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_ledger WHERE session_id=?1)",
            [session],
            |r| r.get::<_, bool>(0),
        )?
    {
        spool.save("goose:has-ledger", &json!(true))?;
    }
    let transcript = match provider {
        Provider::Lingma => ("chat_record", "session_id=?1", "rowid"),
        Provider::Warp => (
            "agent_tasks",
            "conversation_id=?1",
            "task_id COLLATE BINARY",
        ),
        Provider::Goose => ("messages", "session_id=?1", "created_timestamp,id"),
        _ => ("messages", "session_id=?1", "created_at,rowid"),
    };
    let id_field = match provider {
        Provider::Goose => "message_id",
        Provider::Lingma => "request_id",
        _ => "id",
    };
    if provider != Provider::Warp && exists(&db, transcript.0)? {
        spool
            .db
            .execute_batch("CREATE TABLE asr_native_ids(id TEXT PRIMARY KEY,n INTEGER)")?;
        let mut ids = db.prepare(&format!("SELECT {id_field},count(*) FROM {} WHERE session_id=?1 AND typeof({id_field})='text' AND length(CAST({id_field} AS BLOB))<=?2 GROUP BY {id_field}", transcript.0))?;
        let mut cursor = ids.query(params![session, spool.limit as u64])?;
        while let Some(row) = cursor.next()? {
            spool
                .db
                .prepare_cached("INSERT INTO asr_native_ids VALUES(?1,?2)")?
                .execute(params![row.get::<_, String>(0)?, row.get::<_, u64>(1)?])?;
        }
    }
    rows(
        &db,
        transcript.0,
        transcript.1,
        session,
        transcript.2,
        spool.limit,
        |mut row| {
            if provider == Provider::Warp {
                return super::warp::ingest(spool, &mut pos, row);
            }
            let native = row[id_field].as_str().filter(|s| !s.is_empty());
            let unique = if let Some(id) = native {
                spool
                    .db
                    .prepare_cached("SELECT n=1 FROM asr_native_ids WHERE id=?1")?
                    .query_row([id], |r| r.get::<_, bool>(0))
                    .optional()?
                    .unwrap_or(false)
            } else {
                false
            };
            row["_key"] = json!(if unique {
                format!("{}:{}", transcript.0, native.unwrap_or_default())
            } else {
                format!(
                    "{}:position:{}",
                    transcript.0,
                    row["_rowid"].as_str().unwrap_or_default()
                )
            });
            if !unique {
                row["_capture"] = json!(true);
            }
            if provider == Provider::Crush {
                super::crush::index_calls(spool, &row)?;
            }
            if provider == Provider::Goose {
                super::goose::index_calls(spool, &row)?;
            }
            push(spool, &mut pos, row)
        },
    )?;
    // Independent immutable side logs retain their own deterministic order; current state is last.
    let side_tables: &[(&str, &str)] = match provider {
        Provider::Crush => &[("files", "path,version,id")],
        Provider::Goose => &[("usage_ledger", "id")],
        _ => &[],
    };
    for (table, order) in side_tables {
        rows(
            &db,
            table,
            "session_id=?1",
            session,
            order,
            spool.limit,
            |mut row| {
                if row["id"].is_null() {
                    row["_key"] = json!(format!("{table}:position:{}", row["_rowid"]));
                    row["_capture"] = json!(true);
                } else {
                    row["_key"] = json!(format!("{table}:{}", row["id"]));
                }
                push(spool, &mut pos, row)
            },
        )?;
    }
    if provider == Provider::Goose {
        rows(
            &db,
            "thread_messages",
            "session_id=?1",
            session,
            "id",
            spool.limit,
            |mut row| {
                row["_key"] = json!(format!("thread_messages:position:{}", row["_rowid"]));
                row["_capture"] = json!(true);
                super::goose::index_calls(spool, &row)?;
                push(spool, &mut pos, row)
            },
        )?;
    }
    if provider == Provider::Warp {
        rows(
            &db,
            "ai_queries",
            "conversation_id=?1",
            session,
            "id",
            spool.limit,
            |mut row| {
                row["_key"] = json!(format!("ai_queries:position:{}", row["_rowid"]));
                row["_capture"] = json!(true);
                push(spool, &mut pos, row)
            },
        )?;
    }
    if provider == Provider::Crush {
        rows(
            &db,
            "read_files",
            "session_id=?1",
            session,
            "path",
            spool.limit,
            |mut row| {
                row["_key"] = json!(format!("read_files:{}", row["path"]));
                if row["path"].is_null() {
                    row["_key"] = json!(format!("read_files:position:{}", row["_rowid"]));
                    row["_capture"] = json!(true);
                }
                push(spool, &mut pos, row)
            },
        )?;
    }
    if provider == Provider::Warp {
        let mut key = String::from("warp-audit:");
        loop {
            let next: Option<(String,String)> = spool.db.query_row("SELECT key,value FROM entities WHERE key LIKE 'warp-audit:%' AND key>?1 ORDER BY key LIMIT 1", [&key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((next, value)) = next else {
                break;
            };
            key = next;
            push(spool, &mut pos, serde_json::from_str(&value)?)?;
        }
    }
    if provider != Provider::Lingma {
        header["_key"] = json!("session-current-state");
        header["_capture"] = json!(true);
        push(spool, &mut pos, header)?;
    }
    Ok(())
}
pub(super) fn push(spool: &mut Spool, pos: &mut u64, row: Value) -> Result<()> {
    *pos += 1;
    spool.source_bytes += row["_bytes"].as_u64().unwrap_or(0);
    spool.push_sqlite(*pos, &row)
}
