use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params_from_iter, types::Value as SqlValue};

use super::*;

pub(super) const SHELLEY_QUERY_BATCH_ROWS: usize = 16;
type ShelleyScannedUnit = (ShelleyUnit<ShelleyMessage>, [u8; 32]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ShelleyQueryCounters {
    pub(super) candidate_set_reads: u64,
    pub(super) message_set_reads: u64,
    pub(super) conversation_candidate_set_reads: u64,
    pub(super) conversation_set_reads: u64,
    pub(super) relationship_set_reads: u64,
    pub(super) rows_projected: u64,
    pub(super) pages_emitted: u64,
    pub(super) peak_buffered_rows: u64,
    pub(super) peak_buffered_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static SHELLEY_QUERY_COUNTERS: std::cell::Cell<ShelleyQueryCounters> =
        const { std::cell::Cell::new(ShelleyQueryCounters {
            candidate_set_reads: 0,
            message_set_reads: 0,
            conversation_candidate_set_reads: 0,
            conversation_set_reads: 0,
            relationship_set_reads: 0,
            rows_projected: 0,
            pages_emitted: 0,
            peak_buffered_rows: 0,
            peak_buffered_bytes: 0,
        }) };
}

#[cfg(test)]
pub(super) fn reset_shelley_query_counters() {
    SHELLEY_QUERY_COUNTERS.set(ShelleyQueryCounters::default());
}

#[cfg(test)]
pub(super) fn shelley_query_counters() -> ShelleyQueryCounters {
    SHELLEY_QUERY_COUNTERS.get()
}

fn record_query(update: impl FnOnce(&mut ShelleyQueryCounters)) {
    #[cfg(test)]
    SHELLEY_QUERY_COUNTERS.with(|slot| {
        let mut counters = slot.get();
        update(&mut counters);
        slot.set(counters);
    });
    #[cfg(not(test))]
    let _ = update;
}

#[cfg(test)]
pub(super) fn record_shelley_page_emission(buffered_rows: usize, buffered_bytes: usize) {
    record_query(|counters| {
        counters.pages_emitted = counters.pages_emitted.saturating_add(1);
        counters.peak_buffered_rows = counters
            .peak_buffered_rows
            .max(u64::try_from(buffered_rows).unwrap_or(u64::MAX));
        counters.peak_buffered_bytes = counters
            .peak_buffered_bytes
            .max(u64::try_from(buffered_bytes).unwrap_or(u64::MAX));
    });
}

#[cfg(test)]
pub(super) fn record_shelley_buffered_results(buffered_rows: usize, buffered_bytes: usize) {
    record_query(|counters| {
        counters.peak_buffered_rows = counters
            .peak_buffered_rows
            .max(u64::try_from(buffered_rows).unwrap_or(u64::MAX));
        counters.peak_buffered_bytes = counters
            .peak_buffered_bytes
            .max(u64::try_from(buffered_bytes).unwrap_or(u64::MAX));
    });
}

#[derive(Clone, Copy)]
struct Candidate {
    rowid: i64,
    retained_bytes: usize,
}

// The batch is capped at 16 rows; boxing either path would add allocation to
// every accepted row or every local rejection for a small, fixed upper bound.
#[allow(clippy::large_enum_variant)]
enum PreparedMessage {
    Complete((ShelleyUnit<ShelleyMessage>, [u8; 32])),
    Decoded {
        candidate: Candidate,
        values: Vec<NativeSqliteValue>,
        message: ShelleyMessageRow,
    },
}

struct AcceptedMessage {
    candidate: Candidate,
    values: Vec<NativeSqliteValue>,
    message: ShelleyMessageRow,
    conversation: ShelleyConversationRow,
    parent_digest: [u8; 32],
    parent_bytes: usize,
}

enum PreparedUnit {
    Complete((ShelleyUnit<ShelleyMessage>, [u8; 32])),
    Accepted(AcceptedMessage),
}

pub(super) fn next_message_units(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    has_sequence_id: bool,
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Vec<(ShelleyUnit<ShelleyMessage>, [u8; 32])>> {
    let candidates = next_candidates(conn, "messages", "m", message_select, after, through)?;
    project_message_candidates(
        conn,
        message_select,
        conversation_select,
        has_sequence_id,
        candidates,
        through.is_some(),
    )
}

fn project_message_candidates(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    has_sequence_id: bool,
    candidates: Vec<Candidate>,
    single_row_lookup: bool,
) -> Result<Vec<ShelleyScannedUnit>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    record_query(|counters| {
        counters.rows_projected = counters
            .rows_projected
            .saturating_add(candidates.len() as u64);
    });

    let message_rowids = candidates
        .iter()
        .map(|candidate| candidate.rowid)
        .collect::<Vec<_>>();
    let mut message_values =
        query_row_values_set(conn, "messages", "m", message_select, &message_rowids, true)?;
    let mut prepared = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let values = message_values
            .remove(&candidate.rowid)
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        match decode_shelley_message(&values) {
            Ok(message) => prepared.push(PreparedMessage::Decoded {
                candidate,
                values,
                message,
            }),
            Err(error) => {
                let digest = values_row_digest(b'm', candidate.rowid, &values, None);
                prepared.push(PreparedMessage::Complete((
                    ShelleyUnit::Rejected {
                        rowid: candidate.rowid,
                        retained_bytes: candidate.retained_bytes.saturating_add(256),
                        reason: error.to_string(),
                    },
                    digest,
                )));
            }
        }
    }

    let conversation_ids = prepared
        .iter()
        .filter_map(|prepared| match prepared {
            PreparedMessage::Decoded { message, .. } => Some(message.conversation_id.clone()),
            PreparedMessage::Complete(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let conversation_candidates =
        load_conversation_candidates(conn, conversation_select, &conversation_ids)?;
    let conversation_rowids = conversation_candidates
        .values()
        .filter_map(|candidates| match candidates.as_slice() {
            [candidate] => Some(candidate.rowid),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let conversation_values = query_row_values_set(
        conn,
        "conversations",
        "c",
        conversation_select,
        &conversation_rowids,
        false,
    )?;

    let mut units = Vec::with_capacity(prepared.len());
    for prepared in prepared {
        let PreparedMessage::Decoded {
            candidate,
            values,
            message,
        } = prepared
        else {
            let PreparedMessage::Complete(unit) = prepared else {
                unreachable!();
            };
            units.push(PreparedUnit::Complete(unit));
            continue;
        };
        let parent_candidates = conversation_candidates
            .get(&message.conversation_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let [parent_candidate] = parent_candidates else {
            let reason = if parent_candidates.is_empty() {
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
            let parent_digest = rejected_row_digest(b'p', 0, parent_candidates.len(), &reason);
            let row_digest =
                values_row_digest(b'm', candidate.rowid, &values, Some(&parent_digest));
            units.push(PreparedUnit::Complete((
                ShelleyUnit::Rejected {
                    rowid: candidate.rowid,
                    retained_bytes: candidate.retained_bytes.saturating_add(256),
                    reason,
                },
                row_digest,
            )));
            continue;
        };
        let parent_values = conversation_values
            .get(&parent_candidate.rowid)
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let conversation = match decode_shelley_conversation(parent_values) {
            Ok(conversation) => conversation,
            Err(error) => {
                let parent_digest =
                    values_row_digest(b'p', parent_candidate.rowid, parent_values, None);
                let row_digest =
                    values_row_digest(b'm', candidate.rowid, &values, Some(&parent_digest));
                units.push(PreparedUnit::Complete((
                    ShelleyUnit::Rejected {
                        rowid: candidate.rowid,
                        retained_bytes: candidate.retained_bytes.saturating_add(256),
                        reason: error.to_string(),
                    },
                    row_digest,
                )));
                continue;
            }
        };
        units.push(PreparedUnit::Accepted(AcceptedMessage {
            candidate,
            values,
            message,
            conversation,
            parent_digest: values_row_digest(b'p', parent_candidate.rowid, parent_values, None),
            parent_bytes: parent_candidate.retained_bytes,
        }));
    }

    let accepted_messages = units
        .iter()
        .filter_map(|unit| match unit {
            PreparedUnit::Accepted(accepted) => Some(accepted.message.clone()),
            PreparedUnit::Complete(_) => None,
        })
        .collect::<Vec<_>>();
    let parent_bearing = load_parent_bearing(conn, &accepted_messages)?;
    if has_sequence_id && !accepted_messages.is_empty() {
        record_query(|counters| counters.relationship_set_reads += 1);
    }
    let event_indices = if single_row_lookup && accepted_messages.len() == 1 {
        let message = &accepted_messages[0];
        BTreeMap::from([(
            message.rowid,
            shelley_stable_event_index(conn, message, has_sequence_id)?,
        )])
    } else {
        super::super::relationships::shelley_stable_event_indices(
            conn,
            &accepted_messages,
            has_sequence_id,
        )?
    };

    units
        .into_iter()
        .map(|unit| match unit {
            PreparedUnit::Complete(unit) => Ok(unit),
            PreparedUnit::Accepted(accepted) => {
                let parent_bearing = *parent_bearing
                    .get(&accepted.candidate.rowid)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?;
                let provider_event_index = *event_indices
                    .get(&accepted.candidate.rowid)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?;
                let row_digest = values_row_digest(
                    b'm',
                    accepted.candidate.rowid,
                    &accepted.values,
                    Some(&accepted.parent_digest),
                );
                Ok((
                    ShelleyUnit::Accepted {
                        rowid: accepted.candidate.rowid,
                        retained_bytes: accepted
                            .candidate
                            .retained_bytes
                            .saturating_add(accepted.parent_bytes)
                            .saturating_add(1_024),
                        value: ShelleyMessage {
                            message: accepted.message,
                            conversation: accepted.conversation,
                            parent_bearing,
                            provider_event_index,
                        },
                    },
                    row_digest,
                ))
            }
        })
        .collect()
}

fn next_candidates(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Vec<Candidate>> {
    record_query(|counters| counters.candidate_set_reads += 1);
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
          order by {alias}.rowid
          limit {SHELLEY_QUERY_BATCH_ROWS}"
    );
    let candidates: Vec<(i64, i64)> = with_shelley_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        if through.is_some() {
            return match (after, through) {
                (Some(after), Some(through)) => statement
                    .query_row(rusqlite::params![after, through], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()
                    .map(|candidate| candidate.into_iter().collect()),
                (None, Some(through)) => statement
                    .query_row(rusqlite::params![through], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()
                    .map(|candidate| candidate.into_iter().collect()),
                _ => unreachable!(),
            };
        }
        let mut collect = |parameters| {
            statement
                .query_map(parameters, |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        };
        match (after, through) {
            (Some(after), None) => collect(rusqlite::params![after]),
            (None, None) => collect(rusqlite::params![]),
            _ => unreachable!(),
        }
    })?;
    candidates
        .into_iter()
        .map(|(rowid, retained)| {
            let retained_bytes = usize::try_from(retained).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "Shelley {table} retained byte count must be nonnegative"
                ))
            })?;
            Ok(Candidate {
                rowid,
                retained_bytes: retained_bytes.saturating_add(select.len() * 16),
            })
        })
        .collect()
}

fn load_conversation_candidates(
    conn: &Connection,
    select: &[String],
    conversation_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<Candidate>>> {
    if conversation_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    record_query(|counters| counters.conversation_candidate_set_reads += 1);
    let placeholders = std::iter::repeat_n("?", conversation_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let lengths = shelley_retained_length_expr(select);
    let sql = format!(
        "with ranked as (
             select c.conversation_id, c.rowid, {lengths} as retained_bytes,
                    row_number() over (
                        partition by c.conversation_id order by c.rowid
                    ) as candidate_rank
               from conversations c
              where typeof(c.conversation_id) = 'text'
                and c.conversation_id in ({placeholders})
         )
         select conversation_id, rowid, retained_bytes
           from ranked
          where candidate_rank <= 2
          order by cast(conversation_id as blob), rowid"
    );
    let parameters = conversation_ids.iter().cloned().map(SqlValue::Text);
    with_shelley_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut candidates = BTreeMap::<String, Vec<Candidate>>::new();
        for row in rows {
            let (conversation_id, rowid, retained) = row?;
            let retained_bytes = usize::try_from(retained)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, retained))?;
            candidates
                .entry(conversation_id)
                .or_default()
                .push(Candidate {
                    rowid,
                    retained_bytes,
                });
        }
        Ok(candidates)
    })
}

fn query_row_values_set(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    rowids: &[i64],
    messages: bool,
) -> Result<BTreeMap<i64, Vec<NativeSqliteValue>>> {
    if rowids.is_empty() {
        return Ok(BTreeMap::new());
    }
    record_query(|counters| {
        if messages {
            counters.message_set_reads += 1;
        } else {
            counters.conversation_set_reads += 1;
        }
    });
    let placeholders = std::iter::repeat_n("?", rowids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "select {} from {table} {alias}
          where {alias}.rowid in ({placeholders})
          order by {alias}.rowid",
        select.join(", ")
    );
    let parameters = rowids.iter().copied().map(SqlValue::Integer);
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        let rowid = row.get(0)?;
        let values = (0..select.len())
            .map(|index| row.get_ref(index).map(native_value))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rowid, values))
    })?;
    let values = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    if values.len() != rowids.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(values)
}

fn load_parent_bearing(
    conn: &Connection,
    messages: &[ShelleyMessageRow],
) -> Result<BTreeMap<i64, bool>> {
    if messages.is_empty() {
        return Ok(BTreeMap::new());
    }
    record_query(|counters| counters.relationship_set_reads += 1);
    let values = std::iter::repeat_n("(?, ?)", messages.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "with requested(rowid, conversation_id) as (values {values})
         select requested.rowid,
                not exists (
                    select 1 from messages previous
                     where typeof(previous.conversation_id) = 'text'
                       and previous.conversation_id = requested.conversation_id
                       and previous.rowid < requested.rowid
                )
           from requested
          order by requested.rowid"
    );
    let parameters = messages.iter().flat_map(|message| {
        [
            SqlValue::Integer(message.rowid),
            SqlValue::Text(message.conversation_id.clone()),
        ]
    });
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
    })?;
    rows.collect::<rusqlite::Result<_>>()
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
