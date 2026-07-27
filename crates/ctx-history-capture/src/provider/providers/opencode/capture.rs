use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, Result};

use super::schema::{
    opencode_session_candidate_sql, opencode_session_hydration_sql, opencode_session_retained_text,
    OpenCodeCapturedShape, OpenCodeRowSql, OpenCodeRowidSeek, OpenCodeSessionSql,
};

pub(super) const OPENCODE_POSITION_KIND: &str = "opencode-sqlite-rowid-keyset-v6";
pub(crate) const OPENCODE_LOCATOR_KIND: &str = "opencode-sqlite-logical-row-v1";
pub(super) const OPENCODE_RECORD_KIND: &str = "opencode-sqlite-message-v1";
pub(super) const OPENCODE_SESSION_PARENT_RECORD_KIND: &str = "opencode-sqlite-session-parent-v1";
pub(super) const OPENCODE_MESSAGE_PART_RECORD_KIND: &str = "opencode-sqlite-message-part-v1";
pub(super) const OPENCODE_END_RECORD_KIND: &str = "opencode-sqlite-end-v1";
const OPENCODE_POSITION_BYTES: usize = 1 + 1 + 8 + 1 + 8 + 1 + 8;

pub(super) struct OpenCodeRowFetcher<'connection> {
    conn: &'connection Connection,
    candidate_first: Statement<'connection>,
    candidate_next: Statement<'connection>,
    hydration: Statement<'connection>,
    child_parent_key: Statement<'connection>,
    message_lookup: Option<Statement<'connection>>,
    session_candidate_first: Statement<'connection>,
    session_candidate_next: Statement<'connection>,
    session_hydration: Statement<'connection>,
    shape: OpenCodeCapturedShape,
    record_kind: ProviderRecordKind,
    parent_record_kind: ProviderRecordKind,
    part_record_kind: ProviderRecordKind,
}

impl<'connection> OpenCodeRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        shape: OpenCodeCapturedShape,
        record_kind: ProviderRecordKind,
    ) -> Result<Self> {
        let row = OpenCodeRowSql::for_shape(conn, shape)?;
        let session = OpenCodeSessionSql::new(conn)?;
        let message_part = shape == OpenCodeCapturedShape::MessagePart;
        let session_retained_text = opencode_session_retained_text(&session);
        Ok(Self {
            conn,
            candidate_first: conn.prepare(&row.candidate_sql(OpenCodeRowidSeek::First))?,
            candidate_next: conn.prepare(&row.candidate_sql(OpenCodeRowidSeek::Next))?,
            hydration: conn.prepare(&row.hydration_sql(shape))?,
            child_parent_key: conn.prepare(&format!(
                "select coalesce(cast({message_id} as text), ''), \
                        coalesce(cast({session_id} as text), '') \
                 from {from_clause} where {alias}.rowid = ?1",
                message_id = row.message_id,
                session_id = row.session_id,
                from_clause = row.from_clause,
                alias = row.source_alias,
            ))?,
            message_lookup: message_part
                .then(|| {
                    conn.prepare(
                        "select rowid, coalesce(cast(session_id as text), '') \
                         from message where id = ?1 order by rowid limit 1",
                    )
                })
                .transpose()?,
            session_candidate_first: conn.prepare(&opencode_session_candidate_sql(
                &session_retained_text,
                OpenCodeRowidSeek::First,
            ))?,
            session_candidate_next: conn.prepare(&opencode_session_candidate_sql(
                &session_retained_text,
                OpenCodeRowidSeek::Next,
            ))?,
            session_hydration: conn.prepare(&opencode_session_hydration_sql(&session))?,
            shape,
            record_kind,
            parent_record_kind: ProviderRecordKind::new(OPENCODE_SESSION_PARENT_RECORD_KIND)
                .map_err(opencode_captured_error)?,
            part_record_kind: ProviderRecordKind::new(OPENCODE_MESSAGE_PART_RECORD_KIND)
                .map_err(opencode_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_opencode_position(&after, self.shape)?;
        if keyset.phase == OpenCodePositionPhase::Exhausted {
            return Ok(None);
        }
        if keyset.phase == OpenCodePositionPhase::Parent {
            return Err(CaptureError::InvalidPayload(
                "OpenCode transient parent position cannot be resumed".to_owned(),
            ));
        }
        if keyset.phase == OpenCodePositionPhase::ParentEnd {
            if let Some(candidate) = self.session_candidate(keyset.has_after, keyset.rowid)? {
                return self.hydrate_session(candidate, keyset).map(Some);
            }
        }
        let (has_after, after_rowid) = if keyset.phase == OpenCodePositionPhase::Child {
            (keyset.has_after, keyset.rowid)
        } else {
            (false, 0)
        };
        let candidate = self.child_candidate(has_after, after_rowid)?;
        let Some(candidate) = candidate else {
            return self.terminal_row(keyset).map(Some);
        };
        let observed_bytes = opencode_observed_bytes(candidate.retained_bytes)?;
        if observed_bytes > opencode_record_limit()? {
            return self
                .child_oversize(candidate, keyset, observed_bytes)
                .map(Some);
        }
        let structural_key = self.child_structural_key(candidate.rowid)?;
        let observed_bytes = observed_bytes
            .checked_add(structural_key.additional_retained_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode child retained byte count overflowed",
            ))?;
        if observed_bytes > opencode_record_limit()? {
            return self
                .child_oversize(candidate, keyset, observed_bytes)
                .map(Some);
        }
        self.hydrate_child(candidate, structural_key, keyset)
            .map(Some)
    }

    fn terminal_row(&self, keyset: OpenCodeKeyset) -> Result<SqliteLogicalRow> {
        let next_ordinal =
            keyset
                .next_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenCode terminal row ordinal overflowed",
                ))?;
        SqliteLogicalRow::values(
            encode_opencode_position(OpenCodeKeyset {
                shape: self.shape,
                next_ordinal,
                has_after: keyset.has_after,
                rowid: keyset.rowid,
                phase: OpenCodePositionPhase::Exhausted,
                next_part_ordinal: keyset.next_part_ordinal,
            })?,
            keyset.next_ordinal,
            opencode_locator(self.shape, keyset.rowid, OpenCodePositionPhase::Exhausted)?,
            ProviderRecordKind::new(OPENCODE_END_RECORD_KIND).map_err(opencode_captured_error)?,
            Vec::new(),
        )
        .map_err(opencode_captured_error)
    }

    fn child_candidate(
        &mut self,
        has_after: bool,
        rowid: i64,
    ) -> Result<Option<OpenCodeRowCandidate>> {
        let (statement, seek) = if has_after {
            (&mut self.candidate_next, OpenCodeRowidSeek::Next)
        } else {
            (&mut self.candidate_first, OpenCodeRowidSeek::First)
        };
        with_opencode_length_preflight(self.conn, || {
            statement
                .query_row([seek.bound(rowid)], |row| {
                    Ok(OpenCodeRowCandidate {
                        rowid: row.get(0)?,
                        retained_bytes: row.get(1)?,
                    })
                })
                .optional()
        })
    }

    fn session_candidate(
        &mut self,
        has_after: bool,
        rowid: i64,
    ) -> Result<Option<OpenCodeRowCandidate>> {
        let (statement, seek) = if has_after {
            (&mut self.session_candidate_next, OpenCodeRowidSeek::Next)
        } else {
            (&mut self.session_candidate_first, OpenCodeRowidSeek::First)
        };
        with_opencode_length_preflight(self.conn, || {
            statement
                .query_row([seek.bound(rowid)], |row| {
                    Ok(OpenCodeRowCandidate {
                        rowid: row.get(0)?,
                        retained_bytes: row.get(1)?,
                    })
                })
                .optional()
        })
    }

    fn child_structural_key(
        &mut self,
        child_rowid: i64,
    ) -> Result<OpenCodeMessagePartStructuralKey> {
        let (message_id, source_session_id): (String, String) = self
            .child_parent_key
            .query_row([child_rowid], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let (relationship_valid, resolved_session_id) =
            if self.shape == OpenCodeCapturedShape::MessagePart {
                let message: Option<(i64, String)> = self
                    .message_lookup
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenCode message-part lookup is unavailable",
                    ))?
                    .query_row([message_id], |row| Ok((row.get(0)?, row.get(1)?)))
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
        let additional_retained_bytes =
            opencode_additional_session_id_bytes(&source_session_id, &resolved_session_id)?;
        Ok(OpenCodeMessagePartStructuralKey {
            parent_available,
            session_id: resolved_session_id,
            additional_retained_bytes,
        })
    }

    fn hydrate_session(
        &mut self,
        candidate: OpenCodeRowCandidate,
        keyset: OpenCodeKeyset,
    ) -> Result<SqliteLogicalRow> {
        let observed_bytes = opencode_observed_bytes(candidate.retained_bytes)?;
        let (next_position, ordinal, locator) = self.record_identity(
            &keyset,
            candidate.rowid,
            OpenCodePositionPhase::ParentEnd,
            false,
        )?;
        if observed_bytes > opencode_record_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.parent_record_kind.clone(),
                observed_bytes,
            )
            .map_err(opencode_captured_error);
        }
        let mut values = Vec::with_capacity(14);
        values.push(CapturedSqliteValue::Integer(candidate.rowid));
        self.session_hydration.query_row([candidate.rowid], |row| {
            for index in 0..=5 {
                values.push(CapturedSqliteValue::Text(row.get(index)?));
            }
            for index in 6..=12 {
                values.push(CapturedSqliteValue::Integer(row.get(index)?));
            }
            Ok(())
        })?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.parent_record_kind.clone(),
            values,
        )
        .map_err(opencode_captured_error)
    }

    fn hydrate_child(
        &mut self,
        candidate: OpenCodeRowCandidate,
        structural_key: OpenCodeMessagePartStructuralKey,
        keyset: OpenCodeKeyset,
    ) -> Result<SqliteLogicalRow> {
        let observed_bytes = opencode_observed_bytes(candidate.retained_bytes)?;
        if observed_bytes > opencode_record_limit()? {
            return self.child_oversize(candidate, keyset, observed_bytes);
        }
        let part_ordinal = i64::try_from(keyset.next_part_ordinal).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode captured part ordinal exceeds i64")
        })?;
        let mut values = Vec::with_capacity(14);
        values.push(CapturedSqliteValue::Integer(part_ordinal));
        values.push(CapturedSqliteValue::Integer(i64::from(
            structural_key.parent_available,
        )));
        self.hydration.query_row([candidate.rowid], |row| {
            values.push(CapturedSqliteValue::Text(row.get(0)?));
            values.push(CapturedSqliteValue::Text(structural_key.session_id.clone()));
            values.push(CapturedSqliteValue::Text(row.get(2)?));
            values.push(CapturedSqliteValue::Integer(row.get(3)?));
            values.push(CapturedSqliteValue::Integer(row.get(4)?));
            values.push(CapturedSqliteValue::Integer(row.get(5)?));
            values.push(CapturedSqliteValue::Integer(row.get(6)?));
            for index in 7..=11 {
                values.push(CapturedSqliteValue::Text(row.get(index)?));
            }
            Ok(())
        })?;
        let (next_position, ordinal, locator) =
            self.record_identity(&keyset, candidate.rowid, OpenCodePositionPhase::Child, true)?;
        let record_kind = if self.shape == OpenCodeCapturedShape::MessagePart {
            &self.part_record_kind
        } else {
            &self.record_kind
        };
        SqliteLogicalRow::values(next_position, ordinal, locator, record_kind.clone(), values)
            .map_err(opencode_captured_error)
    }

    fn child_oversize(
        &self,
        candidate: OpenCodeRowCandidate,
        keyset: OpenCodeKeyset,
        observed_bytes: u64,
    ) -> Result<SqliteLogicalRow> {
        let (next_position, ordinal, locator) =
            self.record_identity(&keyset, candidate.rowid, OpenCodePositionPhase::Child, true)?;
        let record_kind = if self.shape == OpenCodeCapturedShape::MessagePart {
            &self.part_record_kind
        } else {
            &self.record_kind
        };
        SqliteLogicalRow::oversize(
            next_position,
            ordinal,
            locator,
            record_kind.clone(),
            observed_bytes,
        )
        .map_err(opencode_captured_error)
    }

    fn record_identity(
        &self,
        keyset: &OpenCodeKeyset,
        rowid: i64,
        phase: OpenCodePositionPhase,
        advances_part: bool,
    ) -> Result<(NativePosition, u64, NativeLocator)> {
        let next_ordinal =
            keyset
                .next_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenCode captured row ordinal overflowed",
                ))?;
        let next_part_ordinal = if advances_part {
            keyset
                .next_part_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenCode captured part ordinal overflowed",
                ))?
        } else {
            keyset.next_part_ordinal
        };
        Ok((
            encode_opencode_position(OpenCodeKeyset {
                shape: self.shape,
                next_ordinal,
                has_after: true,
                rowid,
                phase,
                next_part_ordinal,
            })?,
            keyset.next_ordinal,
            opencode_locator(self.shape, rowid, phase)?,
        ))
    }
}

pub(super) fn opencode_values_at_rowid(
    conn: &Connection,
    shape: OpenCodeCapturedShape,
    rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
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
    values.push(CapturedSqliteValue::Integer(part_ordinal));
    values.push(CapturedSqliteValue::Integer(i64::from(parent_available)));
    conn.query_row(&row.hydration_sql(shape), [rowid], |row| {
        values.push(CapturedSqliteValue::Text(row.get(0)?));
        values.push(CapturedSqliteValue::Text(resolved_session_id.clone()));
        values.push(CapturedSqliteValue::Text(row.get(2)?));
        values.push(CapturedSqliteValue::Integer(row.get(3)?));
        values.push(CapturedSqliteValue::Integer(row.get(4)?));
        values.push(CapturedSqliteValue::Integer(row.get(5)?));
        values.push(CapturedSqliteValue::Integer(row.get(6)?));
        for index in 7..=11 {
            values.push(CapturedSqliteValue::Text(row.get(index)?));
        }
        Ok(())
    })?;
    Ok(Some(values))
}

pub(super) fn with_opencode_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH also rejects integer-only octet_length inspection of an oversized
    // stored value. Candidate SQL returns only rowids and lengths, so lift the limit only for
    // that preflight and restore the provider cap before any raw TEXT hydration can execute.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

struct OpenCodeRowCandidate {
    rowid: i64,
    retained_bytes: i64,
}

struct OpenCodeMessagePartStructuralKey {
    parent_available: bool,
    session_id: String,
    additional_retained_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenCodePositionPhase {
    Parent,
    Child,
    ParentEnd,
    Exhausted,
}

impl OpenCodePositionPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Parent => 1,
            Self::Child => 2,
            Self::ParentEnd => 3,
            Self::Exhausted => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Parent),
            2 => Ok(Self::Child),
            3 => Ok(Self::ParentEnd),
            4 => Ok(Self::Exhausted),
            _ => Err(CaptureError::InvalidPayload(
                "OpenCode cursor has an unknown logical-row phase".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OpenCodeKeyset {
    pub(super) shape: OpenCodeCapturedShape,
    pub(super) next_ordinal: u64,
    pub(super) has_after: bool,
    pub(super) rowid: i64,
    pub(super) phase: OpenCodePositionPhase,
    pub(super) next_part_ordinal: u64,
}

pub(super) fn initial_opencode_position(shape: OpenCodeCapturedShape) -> Result<NativePosition> {
    encode_opencode_position(OpenCodeKeyset {
        shape,
        next_ordinal: 0,
        has_after: false,
        rowid: 0,
        phase: OpenCodePositionPhase::ParentEnd,
        next_part_ordinal: 0,
    })
}

pub(super) fn encode_opencode_position(keyset: OpenCodeKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(OPENCODE_POSITION_BYTES);
    value.push(6);
    value.push(keyset.shape.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.push(u8::from(keyset.has_after));
    value.extend_from_slice(&opencode_ordered_i64(keyset.rowid).to_be_bytes());
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_part_ordinal.to_be_bytes());
    NativePosition::new(OPENCODE_POSITION_KIND, value).map_err(opencode_captured_error)
}

pub(super) fn decode_opencode_position(
    position: &NativePosition,
    shape: OpenCodeCapturedShape,
) -> Result<OpenCodeKeyset> {
    if position.kind() != OPENCODE_POSITION_KIND
        || position.value().len() != OPENCODE_POSITION_BYTES
        || position.value()[0] != 6
        || position.value()[1] != shape.tag()
        || position.value()[10] > 1
    {
        return Err(CaptureError::InvalidPayload(
            "OpenCode cursor has an invalid native-position shape".to_owned(),
        ));
    }
    let next_ordinal = opencode_decode_u64(&position.value()[2..10])?;
    let has_after = position.value()[10] != 0;
    let rowid = opencode_unordered_i64(opencode_decode_u64(&position.value()[11..19])?);
    let phase = OpenCodePositionPhase::from_tag(position.value()[19])?;
    let next_part_ordinal = opencode_decode_u64(&position.value()[20..28])?;
    let keyset = OpenCodeKeyset {
        shape,
        next_ordinal,
        has_after,
        rowid,
        phase,
        next_part_ordinal,
    };
    let valid_empty_keyset = rowid == 0
        && next_part_ordinal == 0
        && matches!(
            (phase, next_ordinal),
            (OpenCodePositionPhase::ParentEnd, 0) | (OpenCodePositionPhase::Exhausted, 1)
        );
    if !has_after && !valid_empty_keyset {
        return Err(CaptureError::InvalidPayload(
            "OpenCode cursor has invalid empty-source keyset state".to_owned(),
        ));
    }
    Ok(keyset)
}

pub(super) fn validate_opencode_resume_position(
    position: &NativePosition,
    shape: OpenCodeCapturedShape,
) -> Result<OpenCodeKeyset> {
    let keyset = decode_opencode_position(position, shape)?;
    if keyset.has_after && keyset.phase == OpenCodePositionPhase::Parent {
        return Err(CaptureError::InvalidPayload(
            "OpenCode certified cursor stops inside a transient parent group".to_owned(),
        ));
    }
    Ok(keyset)
}

pub(super) fn opencode_observed_bytes(retained_bytes: i64) -> Result<u64> {
    u64::try_from(retained_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode SQLite retained byte count must be nonnegative".to_owned(),
        )
    })
}

pub(super) fn opencode_additional_session_id_bytes(source: &str, resolved: &str) -> Result<u64> {
    u64::try_from(resolved.len().saturating_sub(source.len()))
        .map_err(|_| CaptureError::SystemInvariant("OpenCode session-id length exceeds u64"))
}

pub(super) fn opencode_record_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode captured record limit exceeds u64"))
}

pub(super) fn opencode_parent_ordinal(rowid: i64) -> u64 {
    // SQLite rowids cover the full signed range. Reinterpreting and wrapping keeps rowid 1 at the
    // historical zero-based line while providing a stable, one-to-one value for sparse, zero, and
    // negative rowids too.
    (rowid as u64).wrapping_sub(1)
}

pub(super) fn opencode_locator(
    shape: OpenCodeCapturedShape,
    rowid: i64,
    phase: OpenCodePositionPhase,
) -> Result<NativeLocator> {
    let mut locator_value = Vec::with_capacity(10);
    locator_value.push(shape.tag());
    locator_value.extend_from_slice(&opencode_ordered_i64(rowid).to_be_bytes());
    locator_value.push(phase.tag());
    NativeLocator::new(OPENCODE_LOCATOR_KIND, locator_value).map_err(opencode_captured_error)
}

pub(crate) fn decode_opencode_message_locator(
    locator: &NativeLocator,
) -> Result<(OpenCodeCapturedShape, i64)> {
    if locator.kind() != OPENCODE_LOCATOR_KIND
        || locator.value().len() != 10
        || locator.value()[9] != OpenCodePositionPhase::Child.tag()
    {
        return Err(CaptureError::InvalidPayload(
            "OpenCode complete-content locator has an invalid shape".into(),
        ));
    }
    let shape = OpenCodeCapturedShape::from_tag(locator.value()[0])?;
    let encoded = opencode_decode_u64(&locator.value()[1..9])?;
    Ok((shape, opencode_unordered_i64(encoded)))
}

pub(super) fn opencode_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("OpenCode cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn opencode_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

pub(super) fn opencode_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn opencode_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn opencode_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
