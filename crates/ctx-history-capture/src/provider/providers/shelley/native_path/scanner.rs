use super::*;

pub(super) fn next_message_unit(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    has_sequence_id: bool,
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(ShelleyUnit<ShelleyMessage>, [u8; 32])>> {
    let Some((rowid, retained_bytes)) =
        next_candidate(conn, "messages", "m", message_select, after, through)?
    else {
        return Ok(None);
    };
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason =
            format!("Shelley message row {rowid} exceeds the source-backed row byte limit");
        return Ok(Some((
            ShelleyUnit::Rejected {
                rowid,
                retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD.min(SHELLEY_ROW_MAX_BYTES),
                reason: reason.clone(),
            },
            rejected_row_digest(b'm', rowid, retained_bytes, &reason),
        )));
    }
    let values = query_row_values(conn, "messages", "m", message_select, rowid)?;
    let message = match decode_shelley_message(&values) {
        Ok(message) => message,
        Err(error) => {
            let digest = values_row_digest(b'm', rowid, &values, None);
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason: error.to_string(),
                },
                digest,
            )));
        }
    };
    let parent = load_conversation_for_message(conn, conversation_select, &message)?;
    let (conversation, parent_values, parent_bytes) = match parent {
        ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        } => (conversation, values, retained_bytes),
        ParentConversation::Rejected { reason, digest } => {
            let row_digest = values_row_digest(b'm', rowid, &values, Some(&digest));
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason,
                },
                row_digest,
            )));
        }
    };
    let parent_bearing: bool = conn.query_row(
        "select not exists (
             select 1 from messages previous
             where typeof(previous.conversation_id) = 'text'
               and previous.conversation_id = ?1
               and previous.rowid < ?2
         )",
        rusqlite::params![message.conversation_id, rowid],
        |row| row.get(0),
    )?;
    let parent_digest = values_row_digest(b'p', conversation.rowid, &parent_values, None);
    let row_digest = values_row_digest(b'm', rowid, &values, Some(&parent_digest));
    let provider_event_index = shelley_stable_event_index(conn, &message, has_sequence_id)?;
    Ok(Some((
        ShelleyUnit::Accepted {
            rowid,
            retained_bytes: retained_bytes
                .saturating_add(parent_bytes)
                .saturating_add(1_024),
            value: ShelleyMessage {
                message,
                conversation,
                parent_bearing,
                provider_event_index,
            },
        },
        row_digest,
    )))
}

// This is constructed for every message; boxing the accepted row would add hot-path allocation.
#[allow(clippy::large_enum_variant)]
enum ParentConversation {
    Accepted {
        conversation: ShelleyConversationRow,
        values: Vec<NativeSqliteValue>,
        retained_bytes: usize,
    },
    Rejected {
        reason: String,
        digest: [u8; 32],
    },
}

fn load_conversation_for_message(
    conn: &Connection,
    select: &[String],
    message: &ShelleyMessageRow,
) -> Result<ParentConversation> {
    let lengths = shelley_retained_length_expr(select);
    let sql = format!(
        "select c.rowid, {lengths}
         from conversations c
         where typeof(c.conversation_id) = 'text' and c.conversation_id = ?1
         order by c.rowid limit 2"
    );
    let candidates = with_shelley_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map([message.conversation_id.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let [(rowid, retained)] = candidates.as_slice() else {
        let reason = if candidates.is_empty() {
            format!(
                "Shelley message {} references missing conversation {}",
                message.message_id, message.conversation_id
            )
        } else {
            format!(
                "Shelley message {} references duplicate conversation {}",
                message.message_id, message.conversation_id
            )
        };
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', 0, candidates.len(), &reason),
            reason,
        });
    };
    let retained_bytes = usize::try_from(*retained).map_err(|_| {
        CaptureError::InvalidPayload(
            "Shelley conversation retained byte count must be nonnegative".to_owned(),
        )
    })?;
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason = format!(
            "Shelley message {} parent conversation exceeds the source-backed row byte limit",
            message.message_id
        );
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', *rowid, retained_bytes, &reason),
            reason,
        });
    }
    let values = query_row_values(conn, "conversations", "c", select, *rowid)?;
    match decode_shelley_conversation(&values) {
        Ok(conversation) => Ok(ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        }),
        Err(error) => Ok(ParentConversation::Rejected {
            digest: values_row_digest(b'p', *rowid, &values, None),
            reason: error.to_string(),
        }),
    }
}

fn next_candidate(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(i64, usize)>> {
    let lengths = shelley_retained_length_expr(select);
    let lower = after.map_or_else(String::new, |_| format!("and {alias}.rowid > ?1"));
    let upper_parameter = if after.is_some() { "?2" } else { "?1" };
    let upper = through.map_or_else(String::new, |_| {
        format!("and {alias}.rowid <= {upper_parameter}")
    });
    let sql = format!(
        "select {alias}.rowid, {lengths}
         from {table} {alias}
         where 1 = 1 {lower} {upper}
         order by {alias}.rowid limit 1"
    );
    let candidate: Option<(i64, i64)> =
        with_shelley_length_preflight(conn, || match (after, through) {
            (Some(after), Some(through)) => conn
                .query_row(&sql, rusqlite::params![after, through], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional(),
            (Some(after), None) => conn
                .query_row(&sql, [after], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, Some(through)) => conn
                .query_row(&sql, [through], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, None) => conn
                .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
        })?;
    candidate
        .map(|(rowid, retained)| {
            let retained = usize::try_from(retained).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "Shelley {table} retained byte count must be nonnegative"
                ))
            })?;
            Ok((rowid, retained.saturating_add(select.len() * 16)))
        })
        .transpose()
}

fn query_row_values(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    let sql = format!(
        "select {} from {table} {alias} where {alias}.rowid = ?1",
        select.join(", ")
    );
    conn.query_row(&sql, [rowid], |row| {
        (0..select.len())
            .map(|index| row.get_ref(index).map(native_value))
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .map_err(CaptureError::from)
}

fn native_value(value: ValueRef<'_>) -> NativeSqliteValue {
    match value {
        ValueRef::Null => NativeSqliteValue::Null,
        ValueRef::Integer(value) => NativeSqliteValue::Integer(value),
        ValueRef::Real(value) => NativeSqliteValue::from_real(value),
        ValueRef::Text(value) => std::str::from_utf8(value).map_or_else(
            |_| NativeSqliteValue::Blob(value.to_vec()),
            |value| NativeSqliteValue::Text(value.to_owned()),
        ),
        ValueRef::Blob(value) => NativeSqliteValue::Blob(value.to_vec()),
    }
}

fn values_row_digest(
    kind: u8,
    rowid: i64,
    values: &[NativeSqliteValue],
    parent: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                hash_bytes(&mut digest, value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                hash_bytes(&mut digest, value);
            }
        }
    }
    if let Some(parent) = parent {
        digest.update([1]);
        digest.update(parent);
    } else {
        digest.update([0]);
    }
    digest.finalize().into()
}

fn rejected_row_digest(kind: u8, rowid: i64, retained_bytes: usize, reason: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((retained_bytes as u64).to_le_bytes());
    hash_bytes(&mut digest, reason.as_bytes());
    digest.finalize().into()
}

fn hash_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
