use rusqlite::{params_from_iter, types::ValueRef, Connection, Row};

use crate::MAX_PROVIDER_SQLITE_VALUE_BYTES;

use super::{
    super::history::KiroConversationRow,
    source_backed::{KiroSourceBackedErrorV0, KiroSourceBackedResultV0},
    KiroPhase,
};

pub(super) fn stream_rows(
    connection: &Connection,
    phase: KiroPhase,
    visit: &mut dyn FnMut(KiroConversationRow) -> KiroSourceBackedResultV0<()>,
) -> KiroSourceBackedResultV0<u64> {
    let sql = format!(
        "select {} from {} order by typeof(key), key collate binary, rowid",
        selected_columns(phase),
        phase.table()
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut decoded = 0_u64;
    while let Some(row) = rows.next()? {
        let row = decode_row(row, phase)?;
        decoded = decoded
            .checked_add(1)
            .ok_or(KiroSourceBackedErrorV0::CountOverflow)?;
        visit(row)?;
    }
    Ok(decoded)
}

pub(super) fn load_key_batch(
    connection: &Connection,
    phase: KiroPhase,
    keys: &[String],
) -> KiroSourceBackedResultV0<Vec<KiroConversationRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=keys.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "with matched as (
             select {}, row_number() over (
                 partition by key collate binary order by rowid
             ) as requested_ordinal
             from {}
             where typeof(key) = 'text'
               and key collate binary in ({placeholders})
         )
         select {} from matched where requested_ordinal <= 2
         order by key collate binary, rowid",
        selected_columns(phase),
        phase.table(),
        selected_columns(phase),
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(keys))?;
    let mut decoded = Vec::with_capacity(keys.len());
    while let Some(row) = rows.next()? {
        decoded.push(decode_row(row, phase)?);
    }
    Ok(decoded)
}

fn selected_columns(phase: KiroPhase) -> &'static str {
    match phase {
        KiroPhase::V2 => "rowid, key, conversation_id, value, created_at, updated_at",
        KiroPhase::Legacy => "rowid, key, value",
    }
}

fn decode_row(row: &Row<'_>, phase: KiroPhase) -> KiroSourceBackedResultV0<KiroConversationRow> {
    let rowid = row.get::<_, i64>(0)?;
    let decoded = match phase {
        KiroPhase::V2 => KiroConversationRow {
            table: phase.table(),
            rowid,
            key: required_text(row, 1, phase, rowid, "key")?,
            conversation_id: Some(required_text(row, 2, phase, rowid, "conversation_id")?),
            value: required_text(row, 3, phase, rowid, "value")?,
            created_at: optional_integer(row, 4, phase, rowid, "created_at")?,
            updated_at: optional_integer(row, 5, phase, rowid, "updated_at")?,
        },
        KiroPhase::Legacy => KiroConversationRow {
            table: phase.table(),
            rowid,
            key: required_text(row, 1, phase, rowid, "key")?,
            conversation_id: None,
            value: required_text(row, 2, phase, rowid, "value")?,
            created_at: None,
            updated_at: None,
        },
    };
    let retained_bytes = decoded
        .key
        .len()
        .checked_add(decoded.value.len())
        .and_then(|bytes| match decoded.conversation_id.as_ref() {
            Some(value) => bytes.checked_add(value.len()),
            None => Some(bytes),
        })
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)?;
    if retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: "row exceeds the provider SQLite value bound",
        });
    }
    Ok(decoded)
}

fn required_text(
    row: &Row<'_>,
    index: usize,
    phase: KiroPhase,
    rowid: i64,
    field: &'static str,
) -> KiroSourceBackedResultV0<String> {
    match row.get_ref(index)? {
        ValueRef::Text(value) => std::str::from_utf8(value).map(str::to_owned).map_err(|_| {
            KiroSourceBackedErrorV0::UncertifiableRow {
                relation: phase.table(),
                rowid,
                reason: "text column contains invalid UTF-8",
            }
        }),
        _ => Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: match field {
                "key" => "Kiro conversation key has an unsupported SQLite storage class",
                "conversation_id" => {
                    "Kiro conversations_v2.conversation_id has an unsupported SQLite storage class"
                }
                _ => "Kiro conversation value has an unsupported SQLite storage class",
            },
        }),
    }
}

fn optional_integer(
    row: &Row<'_>,
    index: usize,
    phase: KiroPhase,
    rowid: i64,
    field: &'static str,
) -> KiroSourceBackedResultV0<Option<i64>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(value) => Ok(Some(value)),
        _ => Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: match field {
                "created_at" => {
                    "Kiro conversations_v2.created_at has an unsupported SQLite storage class"
                }
                _ => "Kiro conversations_v2.updated_at has an unsupported SQLite storage class",
            },
        }),
    }
}
