use rusqlite::{params, OptionalExtension};

use crate::provider::sqlite::SqliteLengthPreflightGuard;

use super::*;

pub(super) fn build_page(
    conn: &Connection,
    expected: &FirebenderFrontier,
) -> Result<FirebenderPage> {
    expected.validate()?;
    if expected.terminal {
        return Ok(FirebenderPage {
            next: expected.clone(),
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: None,
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    }
    let Some(candidate) = fetch_candidate(conn, expected)? else {
        let mut next = expected.clone();
        next.terminal = true;
        return Ok(FirebenderPage {
            next,
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: None,
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    };
    let retained_bytes = candidate.retained_bytes()?;
    if retained_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        let oversize_authority = format!(
            "oversize:{}:{}:{}:{}",
            candidate.id_bytes,
            candidate.name_bytes,
            candidate.messages_bytes,
            candidate.metadata_bytes
        );
        let next = completed_row_frontier(
            conn,
            expected,
            candidate.rowid,
            candidate.updated_at,
            &oversize_authority,
        )?;
        return Ok(FirebenderPage {
            next,
            row: None,
            message_start: 0,
            message_end: 0,
            rejection: Some(format!(
                "Firebender session rowid {} exceeds the {NATIVE_PATH_MAX_RETAINED_PAGE_BYTES} byte NativePath page bound",
                candidate.rowid
            )),
            retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES,
        });
    }
    let (id, name, created_at, messages_json, metadata_json): (
        String,
        String,
        i64,
        String,
        String,
    ) = conn.query_row(
        "select id, name, cast(created_at as integer), messages_json, metadata_json \
         from chat_sessions \
         where rowid = ?1 and cast(updated_at as integer) = ?2",
        params![candidate.rowid, candidate.updated_at],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let parsed = serde_json::from_str::<Value>(&messages_json);
    let (messages, rejection) = match parsed {
        Ok(Value::Array(messages)) => (messages, None),
        Ok(_) => (
            Vec::new(),
            Some(format!(
                "Firebender session {} messages_json is not an array",
                id
            )),
        ),
        Err(error) => (
            Vec::new(),
            Some(format!(
                "Firebender session {} messages_json is invalid JSON: {error}",
                id
            )),
        ),
    };
    let start = if expected.next_message_index == 0 {
        0
    } else {
        if expected.rowid != candidate.rowid || expected.updated_at != candidate.updated_at {
            return Err(CaptureError::InvalidPayload(
                "Firebender source-backed frontier no longer addresses its active row".to_owned(),
            ));
        }
        usize::try_from(expected.next_message_index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender source-backed message frontier exceeds platform limits".to_owned(),
            )
        })?
    };
    if start > messages.len() {
        return Err(CaptureError::InvalidPayload(
            "Firebender source-backed message frontier exceeds its source row".to_owned(),
        ));
    }
    let end = start
        .saturating_add(FIREBENDER_SOURCE_BACKED_PAGE_MAX_MESSAGES)
        .min(messages.len());
    let row = FirebenderRow {
        rowid: candidate.rowid,
        id,
        name,
        created_at,
        updated_at: candidate.updated_at,
        messages_json,
        metadata_json,
        messages,
    };
    let next = if rejection.is_some() || end == row.messages.len() {
        completed_row_frontier(
            conn,
            expected,
            row.rowid,
            row.updated_at,
            &row.messages_json,
        )?
    } else {
        active_row_frontier(expected, &row, end)?
    };
    Ok(FirebenderPage {
        next,
        row: Some(row),
        message_start: start,
        message_end: end,
        rejection,
        retained_bytes,
    })
}

fn fetch_candidate(
    conn: &Connection,
    frontier: &FirebenderFrontier,
) -> Result<Option<FirebenderRowCandidate>> {
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    let deleted_filter = if columns.contains("deleted_at") {
        "deleted_at is null and"
    } else {
        ""
    };
    let active = frontier.next_message_index != 0;
    let sql = format!(
        "select rowid, cast(updated_at as integer), length(cast(id as blob)), \
                length(cast(name as blob)), \
                length(cast(messages_json as blob)), length(cast(metadata_json as blob)) \
         from chat_sessions where {deleted_filter} \
              ((?1 = 1 and rowid = ?2 and cast(updated_at as integer) = ?3) or \
               (?1 = 0 and (?4 = 0 or cast(updated_at as integer) > ?3 or \
                (cast(updated_at as integer) = ?3 and rowid > ?2)))) \
         order by cast(updated_at as integer), rowid limit 1"
    );
    let has_after = i64::from(frontier.row_ordinal != 0);
    let _length_guard = SqliteLengthPreflightGuard::new(conn);
    conn.query_row(
        &sql,
        params![
            i64::from(active),
            frontier.rowid,
            frontier.updated_at,
            has_after
        ],
        |row| {
            Ok(FirebenderRowCandidate {
                rowid: row.get(0)?,
                updated_at: row.get(1)?,
                id_bytes: row.get(2)?,
                name_bytes: row.get(3)?,
                messages_bytes: row.get(4)?,
                metadata_bytes: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}

fn active_row_frontier(
    expected: &FirebenderFrontier,
    row: &FirebenderRow,
    end: usize,
) -> Result<FirebenderFrontier> {
    let mut hasher = prefix_hasher(expected);
    hash_processed_messages(&mut hasher, row, expected.next_message_index, end);
    Ok(FirebenderFrontier {
        version: FIREBENDER_NATIVE_FRONTIER_VERSION,
        row_ordinal: expected.row_ordinal,
        updated_at: row.updated_at,
        rowid: row.rowid,
        next_message_index: u64::try_from(end).map_err(|_| {
            CaptureError::SystemInvariant("Firebender message frontier exceeds u64")
        })?,
        prefix_sha256: hasher.finalize().into(),
        terminal: false,
    })
}

fn completed_row_frontier(
    conn: &Connection,
    expected: &FirebenderFrontier,
    rowid: i64,
    updated_at: i64,
    semantic_row: &str,
) -> Result<FirebenderFrontier> {
    let mut hasher = prefix_hasher(expected);
    hasher.update(rowid.to_le_bytes());
    hasher.update(updated_at.to_le_bytes());
    hasher.update((semantic_row.len() as u64).to_le_bytes());
    hasher.update(semantic_row.as_bytes());
    let row_ordinal = expected
        .row_ordinal
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Firebender row ordinal exceeds u64",
        ))?;
    let mut next = FirebenderFrontier {
        version: FIREBENDER_NATIVE_FRONTIER_VERSION,
        row_ordinal,
        updated_at,
        rowid,
        next_message_index: 0,
        prefix_sha256: hasher.finalize().into(),
        terminal: false,
    };
    next.terminal = fetch_candidate(conn, &next)?.is_none();
    Ok(next)
}

fn prefix_hasher(frontier: &FirebenderFrontier) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(FIREBENDER_INITIAL_PREFIX_DOMAIN);
    hasher.update(frontier.prefix_sha256);
    hasher
}

fn hash_processed_messages(hasher: &mut Sha256, row: &FirebenderRow, prior_index: u64, end: usize) {
    let start = usize::try_from(prior_index).unwrap_or(usize::MAX);
    hasher.update(row.rowid.to_le_bytes());
    hasher.update(row.updated_at.to_le_bytes());
    hasher.update(prior_index.to_le_bytes());
    for message in row
        .messages
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        if let Ok(bytes) = serde_json::to_vec(message) {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
}
