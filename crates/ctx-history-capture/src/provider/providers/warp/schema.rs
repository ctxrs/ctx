use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

const WARP_CAPABILITY_DIGEST_DOMAIN: &[u8] = b"ctx-warp-sqlite-capability-v2-u64le\0";
const WARP_CAPABILITY_TABLES: &[&str] = &["agent_conversations", "agent_tasks", "ai_queries"];

pub(super) struct WarpSqliteSchema {
    pub(super) task_keyset_index: String,
    pub(super) capability_digest: String,
}

impl WarpSqliteSchema {
    pub(super) fn detect(conn: &Connection) -> Result<Self> {
        warp_validate_schema(conn)?;
        Ok(Self {
            task_keyset_index: warp_task_keyset_index(conn)?,
            capability_digest: warp_capability_digest(conn)?,
        })
    }
}

fn warp_validate_schema(conn: &Connection) -> Result<()> {
    if !sqlite_table_exists(conn, "agent_conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_conversations table".into(),
        ));
    }
    let conversation_columns = sqlite_table_columns(conn, "agent_conversations")?;
    ensure_sqlite_table_columns(
        &conversation_columns,
        "Warp agent_conversations table",
        &["conversation_id", "conversation_data", "last_modified_at"],
    )?;
    if !sqlite_table_exists(conn, "agent_tasks")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_tasks table".into(),
        ));
    }
    let task_columns = sqlite_table_columns(conn, "agent_tasks")?;
    ensure_sqlite_table_columns(
        &task_columns,
        "Warp agent_tasks table",
        &["conversation_id", "task_id", "task", "last_modified_at"],
    )
}

pub(super) fn warp_task_keyset_index(conn: &Connection) -> Result<String> {
    let task_id_not_null: i64 = conn.query_row(
        "select count(*) from pragma_table_info('agent_tasks') \
         where name = 'task_id' and \"notnull\" = 1",
        [],
        |row| row.get(0),
    )?;
    if task_id_not_null != 1 {
        return Err(CaptureError::InvalidPayload(
            "Warp agent_tasks task_id must be declared NOT NULL for bounded keyset traversal"
                .to_owned(),
        ));
    }

    let mut indexes = conn.prepare(
        "select name, \"unique\", partial from pragma_index_list('agent_tasks') order by seq",
    )?;
    let indexes = indexes
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, i64>(2)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (name, unique, partial) in indexes {
        if !unique || partial {
            continue;
        }
        let mut columns = conn.prepare(
            "select seqno, name, \"desc\", coll from pragma_index_xinfo(?1) \
             where key = 1 order by seqno",
        )?;
        let columns = columns
            .query_map([name.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let supported = matches!(
            columns.as_slice(),
            [(0, Some(task_id), false, collation)]
                if task_id == "task_id" && collation.eq_ignore_ascii_case("binary")
        );
        if supported {
            return Ok(name);
        }
    }
    Err(CaptureError::InvalidPayload(
        "Warp agent_tasks requires a non-partial ascending UNIQUE BINARY index on task_id for bounded global keyset traversal"
            .to_owned(),
    ))
}

fn warp_capability_digest(conn: &Connection) -> Result<String> {
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let mut hasher = Sha256::new();
    hasher.update(WARP_CAPABILITY_DIGEST_DOMAIN);
    hash_i64(&mut hasher, user_version);

    for table in WARP_CAPABILITY_TABLES {
        let exists = sqlite_table_exists(conn, table)?;
        hash_text(&mut hasher, table)?;
        hash_bool(&mut hasher, exists);
        if !exists {
            continue;
        }

        let pragma = format!("pragma table_xinfo({})", warp_quote_identifier(table));
        let mut columns = conn.prepare(&pragma)?;
        let columns = columns.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        for column in columns {
            let (cid, name, ty, not_null, default_value, primary_key, hidden) = column?;
            hash_i64(&mut hasher, cid);
            hash_text(&mut hasher, &name)?;
            hash_text(&mut hasher, &ty)?;
            hash_i64(&mut hasher, not_null);
            hash_optional_text(&mut hasher, default_value.as_deref())?;
            hash_i64(&mut hasher, primary_key);
            hash_i64(&mut hasher, hidden);
        }
    }

    let mut indexes = conn.prepare(
        "select name, \"unique\", origin, partial \
         from pragma_index_list('agent_tasks') order by name",
    )?;
    let indexes = indexes
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (name, unique, origin, partial) in indexes {
        hash_text(&mut hasher, &name)?;
        hash_i64(&mut hasher, unique);
        hash_text(&mut hasher, &origin)?;
        hash_i64(&mut hasher, partial);
        let mut columns = conn.prepare(
            "select seqno, cid, name, \"desc\", coll, key \
             from pragma_index_xinfo(?1) order by seqno",
        )?;
        let columns = columns.query_map([name.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for column in columns {
            let (seqno, cid, column_name, descending, collation, key) = column?;
            hash_i64(&mut hasher, seqno);
            hash_i64(&mut hasher, cid);
            hash_optional_text(&mut hasher, column_name.as_deref())?;
            hash_i64(&mut hasher, descending);
            hash_text(&mut hasher, &collation)?;
            hash_i64(&mut hasher, key);
        }
    }

    Ok(hex_digest(hasher.finalize().into()))
}

pub(super) fn warp_quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn hash_bool(hasher: &mut Sha256, value: bool) {
    hasher.update([u8::from(value)]);
}

fn hash_i64(hasher: &mut Sha256, value: i64) {
    hasher.update(value.to_le_bytes());
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) -> Result<()> {
    hash_bool(hasher, value.is_some());
    if let Some(value) = value {
        hash_text(hasher, value)?;
    }
    Ok(())
}

fn hash_text(hasher: &mut Sha256, value: &str) -> Result<()> {
    hasher.update(capability_text_length_bytes(value.len())?);
    hasher.update(value.as_bytes());
    Ok(())
}

fn capability_text_length_bytes(length: usize) -> Result<[u8; 8]> {
    let length = u64::try_from(length)
        .map_err(|_| CaptureError::SystemInvariant("Warp capability text length exceeds u64"))?;
    Ok(length.to_le_bytes())
}

#[cfg(test)]
fn capability_text_authority_bytes(value: &str) -> Result<Vec<u8>> {
    let mut authority = capability_text_length_bytes(value.len())?.to_vec();
    authority.extend_from_slice(value.as_bytes());
    Ok(authority)
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
