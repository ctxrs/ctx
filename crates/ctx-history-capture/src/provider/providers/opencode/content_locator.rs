//! Stable complete-content locator mechanics shared by the NativePath reader
//! and the released SQLite resolver.

use rusqlite::{Connection, OptionalExtension};

use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{CaptureError, Result};

use super::schema::{OpenCodeCapturedShape, OpenCodeRowSql};

pub(crate) const OPENCODE_LOCATOR_KIND: &str = "opencode-sqlite-logical-row-v1";
const OPENCODE_MESSAGE_PHASE: u8 = 2;

pub(super) fn opencode_message_locator(
    shape: OpenCodeCapturedShape,
    rowid: i64,
) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(10);
    value.push(shape.tag());
    value.extend_from_slice(&ordered_i64(rowid).to_be_bytes());
    value.push(OPENCODE_MESSAGE_PHASE);
    NativeLocator::new(OPENCODE_LOCATOR_KIND, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(crate) fn decode_opencode_message_locator(
    locator: &NativeLocator,
) -> Result<(OpenCodeCapturedShape, i64)> {
    if locator.kind() != OPENCODE_LOCATOR_KIND
        || locator.value().len() != 10
        || locator.value()[9] != OPENCODE_MESSAGE_PHASE
    {
        return Err(CaptureError::InvalidPayload(
            "OpenCode complete-content locator has an invalid shape".into(),
        ));
    }
    let shape = OpenCodeCapturedShape::from_tag(locator.value()[0])?;
    let bytes: [u8; 8] = locator.value()[1..9].try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode complete-content locator rowid has an invalid width".to_owned(),
        )
    })?;
    Ok((shape, unordered_i64(u64::from_be_bytes(bytes))))
}

pub(super) fn opencode_values_at_rowid(
    conn: &Connection,
    shape: OpenCodeCapturedShape,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let row = OpenCodeRowSql::for_shape(conn, shape)?;
    let structural = conn
        .query_row(
            &format!(
                "select coalesce(cast({message_id} as text), ''), \
                        coalesce(cast({session_id} as text), '') \
                 from {from_clause} where {alias}.rowid = ?1",
                message_id = row.message_id,
                session_id = row.session_id,
                from_clause = row.from_clause,
                alias = row.source_alias,
            ),
            [rowid],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((message_id, source_session_id)) = structural else {
        return Ok(None);
    };
    let (relationship_valid, resolved_session_id) = if shape == OpenCodeCapturedShape::MessagePart {
        let message = conn
            .query_row(
                "select rowid, coalesce(cast(session_id as text), '') \
                 from message where id = ?1 order by rowid limit 1",
                [message_id.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        match message {
            Some((_, message_session_id)) if source_session_id.trim().is_empty() => {
                (true, message_session_id)
            }
            Some((_, message_session_id)) => (
                message_session_id == source_session_id,
                source_session_id.clone(),
            ),
            None => (false, source_session_id.clone()),
        }
    } else {
        (true, source_session_id.clone())
    };
    let parent_available = relationship_valid && !resolved_session_id.trim().is_empty();
    let part_ordinal: i64 = conn.query_row(
        &format!(
            "select count(*) from {} where {}.rowid < ?1",
            row.candidate_from_clause, row.source_alias,
        ),
        [rowid],
        |row| row.get(0),
    )?;
    let mut values = Vec::with_capacity(14);
    values.push(NativeSqliteValue::Integer(part_ordinal));
    values.push(NativeSqliteValue::Integer(i64::from(parent_available)));
    conn.query_row(&row.hydration_sql(shape), [rowid], |row| {
        values.push(NativeSqliteValue::Text(row.get(0)?));
        values.push(NativeSqliteValue::Text(resolved_session_id.clone()));
        values.push(NativeSqliteValue::Text(row.get(2)?));
        values.push(NativeSqliteValue::Integer(row.get(3)?));
        values.push(NativeSqliteValue::Integer(row.get(4)?));
        values.push(NativeSqliteValue::Integer(row.get(5)?));
        values.push(NativeSqliteValue::Integer(row.get(6)?));
        for index in 7..=11 {
            values.push(NativeSqliteValue::Text(row.get(index)?));
        }
        Ok(())
    })?;
    Ok(Some(values))
}

fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}
