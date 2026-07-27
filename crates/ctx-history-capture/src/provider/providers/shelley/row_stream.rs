use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::source::{
    shelley_conversation_candidate_sql, shelley_conversation_columns,
    shelley_conversation_select_expressions, shelley_has_conversations,
    shelley_later_conversation_message_candidate_sql, shelley_later_sequence_message_candidate_sql,
    shelley_message_candidate_sql, shelley_message_columns, shelley_message_key_candidate_sql,
    shelley_message_select_expressions, shelley_observed_bytes,
    shelley_previous_message_same_conversation_sql, shelley_require_message_index,
    shelley_retained_length_expr, shelley_same_group_message_candidate_sql,
    with_shelley_length_preflight,
};
use super::{
    SHELLEY_CONVERSATION_RECORD_KIND, SHELLEY_LOCATOR_KIND, SHELLEY_MESSAGE_CHILD_RECORD_KIND,
    SHELLEY_MESSAGE_KEY_MARKER_KIND, SHELLEY_MESSAGE_KEY_REJECTION_KIND,
    SHELLEY_MESSAGE_RECORD_KIND, SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND,
    SHELLEY_OVERSIZE_SESSION_RECORD_KIND, SHELLEY_POSITION_BYTES, SHELLEY_POSITION_KIND,
    SHELLEY_TERMINAL_MARKER_KIND,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShelleyCapturePhase {
    MessageKeyClassification,
    Messages,
    Conversations,
}

impl ShelleyCapturePhase {
    fn tag(self) -> u8 {
        match self {
            Self::MessageKeyClassification => 1,
            Self::Messages => 2,
            Self::Conversations => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::MessageKeyClassification),
            2 => Ok(Self::Messages),
            3 => Ok(Self::Conversations),
            _ => Err(CaptureError::InvalidPayload(
                "Shelley cursor has an unknown capture phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ShelleyKeyset {
    pub(super) phase: ShelleyCapturePhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
    pub(super) exhausted: bool,
    pub(super) pending_oversize_session: bool,
    pub(super) classification_has_valid_message: bool,
    pub(super) classification_all_keys_valid: bool,
}

struct ShelleyRowCandidate {
    rowid: i64,
    retained_bytes: i64,
    exhausted: bool,
    skip_projection: bool,
}

struct ShelleyMessageCandidate {
    rowid: i64,
    retained_bytes: i64,
    exhausted: bool,
    conversation_rowid: Option<i64>,
}

struct ShelleyMessageKeyCandidate {
    rowid: i64,
    conversation_id_bytes: i64,
    conversation_id_is_text: bool,
    last: bool,
}

struct ShelleyMessageAnchor {
    conversation_id: String,
    sequence_id: i64,
}

struct ShelleyConversationCandidate {
    rowid: i64,
    retained_bytes: i64,
}

impl ShelleyRowCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        shelley_observed_bytes(self.retained_bytes)
    }
}

impl ShelleyMessageCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        shelley_observed_bytes(self.retained_bytes)
    }
}

pub(super) struct ShelleyRowFetcher<'connection> {
    conn: &'connection Connection,
    first_message_key_candidate: Statement<'connection>,
    next_message_key_candidate: Statement<'connection>,
    first_message_candidate: Statement<'connection>,
    next_message_candidate: Statement<'connection>,
    message_anchor_all_valid: Statement<'connection>,
    next_message_same_group_all_valid: Statement<'connection>,
    next_message_later_sequence_all_valid: Option<Statement<'connection>>,
    next_message_later_conversation_all_valid: Statement<'connection>,
    message_conversation_candidate: Statement<'connection>,
    previous_message_same_conversation: Statement<'connection>,
    message_hydration: Statement<'connection>,
    pending_session_candidate: Statement<'connection>,
    first_conversation_candidate: Statement<'connection>,
    next_conversation_candidate: Statement<'connection>,
    conversation_hydration: Statement<'connection>,
    has_sequence: bool,
    active_conversation_rowid: Option<i64>,
    #[cfg(test)]
    message_parent_hydrations: usize,
    message_record_kind: ProviderRecordKind,
    message_child_record_kind: ProviderRecordKind,
    message_key_marker_kind: ProviderRecordKind,
    message_key_rejection_kind: ProviderRecordKind,
    terminal_marker_kind: ProviderRecordKind,
    conversation_record_kind: ProviderRecordKind,
    oversize_session_record_kind: ProviderRecordKind,
    nonempty_conversation_record_kind: ProviderRecordKind,
}

impl<'connection> ShelleyRowFetcher<'connection> {
    pub(super) fn new(conn: &'connection Connection) -> Result<Self> {
        let conversation_columns = shelley_conversation_columns(conn)?;
        let message_columns = shelley_message_columns(conn)?;
        let has_sequence = message_columns.contains("sequence_id");
        shelley_require_message_index(conn, has_sequence)?;
        let conversation_expressions =
            shelley_conversation_select_expressions(&conversation_columns, "c");
        let message_expressions = shelley_message_select_expressions(&message_columns, "m");
        let message_lengths = shelley_retained_length_expr(&message_expressions);
        let conversation_lengths = shelley_retained_length_expr(&conversation_expressions);
        let message_select = message_expressions.join(", ");
        let conversation_select = conversation_expressions.join(", ");
        // Persist only the native INTEGER PRIMARY KEY. Once the paced classification
        // phase has proved every conversation_id bounded, hydrate the native-order
        // anchor by rowid and bind it into a separate indexed seek. A malformed
        // oversized value must never cross SQLite's connection limit.
        Ok(Self {
            conn,
            first_message_key_candidate: conn.prepare(&shelley_message_key_candidate_sql(false))?,
            next_message_key_candidate: conn.prepare(&shelley_message_key_candidate_sql(true))?,
            first_message_candidate: conn.prepare(&shelley_message_candidate_sql(
                &message_lengths,
                false,
                has_sequence,
            ))?,
            next_message_candidate: conn.prepare(&shelley_message_candidate_sql(
                &message_lengths,
                true,
                has_sequence,
            ))?,
            message_anchor_all_valid: conn.prepare(if has_sequence {
                "select conversation_id, sequence_id from messages where rowid = ?1"
            } else {
                "select conversation_id, rowid from messages where rowid = ?1"
            })?,
            next_message_same_group_all_valid: conn.prepare(
                &shelley_same_group_message_candidate_sql(&message_lengths, has_sequence),
            )?,
            next_message_later_sequence_all_valid: has_sequence
                .then(|| {
                    conn.prepare(&shelley_later_sequence_message_candidate_sql(
                        &message_lengths,
                    ))
                })
                .transpose()?,
            next_message_later_conversation_all_valid: conn.prepare(
                &shelley_later_conversation_message_candidate_sql(&message_lengths, has_sequence),
            )?,
            message_conversation_candidate: conn.prepare(&format!(
                "select c.rowid, {conversation_lengths}
                 from conversations c where c.rowid = ?1"
            ))?,
            previous_message_same_conversation: conn.prepare(
                &shelley_previous_message_same_conversation_sql(has_sequence),
            )?,
            message_hydration: conn.prepare(&format!(
                "select {message_select} from messages m where m.rowid = ?1"
            ))?,
            pending_session_candidate: conn.prepare(&format!(
                "select c.rowid, {conversation_lengths}, 0, 0
                 from messages m join conversations c
                   on c.conversation_id = m.conversation_id
                 where m.rowid = ?1"
            ))?,
            first_conversation_candidate: conn.prepare(&shelley_conversation_candidate_sql(
                &conversation_lengths,
                false,
            ))?,
            next_conversation_candidate: conn.prepare(&shelley_conversation_candidate_sql(
                &conversation_lengths,
                true,
            ))?,
            conversation_hydration: conn.prepare(&format!(
                "select {conversation_select} from conversations c where c.rowid = ?1"
            ))?,
            has_sequence,
            active_conversation_rowid: None,
            #[cfg(test)]
            message_parent_hydrations: 0,
            message_record_kind: ProviderRecordKind::new(SHELLEY_MESSAGE_RECORD_KIND)
                .map_err(shelley_captured_error)?,
            message_child_record_kind: ProviderRecordKind::new(SHELLEY_MESSAGE_CHILD_RECORD_KIND)
                .map_err(shelley_captured_error)?,
            message_key_marker_kind: ProviderRecordKind::new(SHELLEY_MESSAGE_KEY_MARKER_KIND)
                .map_err(shelley_captured_error)?,
            message_key_rejection_kind: ProviderRecordKind::new(SHELLEY_MESSAGE_KEY_REJECTION_KIND)
                .map_err(shelley_captured_error)?,
            terminal_marker_kind: ProviderRecordKind::new(SHELLEY_TERMINAL_MARKER_KIND)
                .map_err(shelley_captured_error)?,
            conversation_record_kind: ProviderRecordKind::new(SHELLEY_CONVERSATION_RECORD_KIND)
                .map_err(shelley_captured_error)?,
            oversize_session_record_kind: ProviderRecordKind::new(
                SHELLEY_OVERSIZE_SESSION_RECORD_KIND,
            )
            .map_err(shelley_captured_error)?,
            nonempty_conversation_record_kind: ProviderRecordKind::new(
                SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND,
            )
            .map_err(shelley_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_shelley_position(&after)?;
        if keyset.is_some_and(|value| value.exhausted) {
            return Ok(None);
        }
        let ordinal = keyset.map_or(0, |value| value.next_ordinal);
        if let Some(keyset) = keyset.filter(|value| value.pending_oversize_session) {
            return self
                .hydrate_pending_oversize_session(keyset, ordinal)
                .map(Some);
        }
        match keyset.map(|value| value.phase) {
            None | Some(ShelleyCapturePhase::MessageKeyClassification) => {
                let selected = match keyset {
                    Some(value) => shelley_fetch_next_message_key_candidate(
                        self.conn,
                        &mut self.next_message_key_candidate,
                        value.rowid,
                    )?,
                    None => shelley_fetch_first_message_key_candidate(
                        self.conn,
                        &mut self.first_message_key_candidate,
                    )?,
                };
                if let Some(candidate) = selected {
                    return self
                        .hydrate_message_key(candidate, keyset, ordinal)
                        .map(Some);
                }
                if keyset.is_some_and(|value| value.classification_has_valid_message) {
                    return self.fetch_first_message(
                        ordinal,
                        keyset.is_some_and(|value| value.classification_all_keys_valid),
                    );
                }
                self.fetch_first_conversation(ordinal)
            }
            Some(ShelleyCapturePhase::Messages) => {
                let selected = if keyset.is_some_and(|value| value.classification_all_keys_valid) {
                    shelley_fetch_next_all_valid_message_candidate(
                        self.conn,
                        &mut self.message_anchor_all_valid,
                        &mut self.next_message_same_group_all_valid,
                        self.next_message_later_sequence_all_valid.as_mut(),
                        &mut self.next_message_later_conversation_all_valid,
                        keyset.map_or(0, |value| value.rowid),
                        self.has_sequence,
                    )?
                } else {
                    shelley_fetch_next_message_candidate(
                        self.conn,
                        &mut self.next_message_candidate,
                        keyset.map_or(0, |value| value.rowid),
                    )?
                };
                match selected {
                    Some(candidate) => self
                        .hydrate_message(
                            candidate,
                            ordinal,
                            keyset.is_some_and(|value| value.classification_all_keys_valid),
                        )
                        .map(Some),
                    None => self.fetch_first_conversation(ordinal),
                }
            }
            Some(ShelleyCapturePhase::Conversations) => {
                let after_rowid = keyset.map_or(0, |value| value.rowid);
                shelley_fetch_next_candidate(
                    self.conn,
                    &mut self.next_conversation_candidate,
                    after_rowid,
                )?
                .map_or(Ok(None), |candidate| {
                    self.hydrate_conversation(candidate, ordinal).map(Some)
                })
            }
        }
    }

    fn fetch_first_message(
        &mut self,
        ordinal: u64,
        all_keys_valid: bool,
    ) -> Result<Option<SqliteLogicalRow>> {
        let selected =
            shelley_fetch_first_message_candidate(self.conn, &mut self.first_message_candidate)?;
        match selected {
            Some(candidate) => self
                .hydrate_message(candidate, ordinal, all_keys_valid)
                .map(Some),
            None => self.fetch_first_conversation(ordinal),
        }
    }

    fn hydrate_message_key(
        &mut self,
        candidate: ShelleyMessageKeyCandidate,
        previous: Option<ShelleyKeyset>,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let maximum_key_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("Shelley SQLite value limit exceeds i64 range")
        })?;
        let valid = candidate.conversation_id_is_text
            && (0..=maximum_key_bytes).contains(&candidate.conversation_id_bytes);
        let classification_has_valid_message =
            valid || previous.is_some_and(|value| value.classification_has_valid_message);
        let classification_all_keys_valid =
            valid && previous.is_none_or(|value| value.classification_all_keys_valid);
        let exhausted = candidate.last
            && !classification_has_valid_message
            && !shelley_has_conversations(self.conn)?;
        let next_position = encode_shelley_position(ShelleyKeyset {
            phase: ShelleyCapturePhase::MessageKeyClassification,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Shelley captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
            exhausted,
            pending_oversize_session: false,
            classification_has_valid_message,
            classification_all_keys_valid,
        })?;
        let locator = shelley_locator(
            ShelleyCapturePhase::MessageKeyClassification,
            candidate.rowid,
        )?;
        if !valid {
            if candidate.conversation_id_is_text {
                return SqliteLogicalRow::oversize(
                    next_position,
                    ordinal,
                    locator,
                    self.message_record_kind.clone(),
                    shelley_observed_bytes(candidate.conversation_id_bytes)?,
                )
                .map_err(shelley_captured_error);
            }
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.message_key_rejection_kind.clone(),
                vec![
                    CapturedSqliteValue::Integer(candidate.rowid),
                    CapturedSqliteValue::Integer(candidate.conversation_id_bytes),
                ],
            )
            .map_err(shelley_captured_error);
        }
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.message_key_marker_kind.clone(),
            vec![CapturedSqliteValue::Integer(candidate.rowid)],
        )
        .map_err(shelley_captured_error)
    }

    fn fetch_first_conversation(&mut self, ordinal: u64) -> Result<Option<SqliteLogicalRow>> {
        let selected =
            shelley_fetch_first_candidate(self.conn, &mut self.first_conversation_candidate)?;
        match selected {
            Some(candidate) => self.hydrate_conversation(candidate, ordinal).map(Some),
            None if ordinal == 0 => Ok(None),
            None => self.terminal_marker(ordinal).map(Some),
        }
    }

    fn terminal_marker(&self, ordinal: u64) -> Result<SqliteLogicalRow> {
        let rowid = i64::MIN;
        let next_position = encode_shelley_position(ShelleyKeyset {
            phase: ShelleyCapturePhase::Conversations,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Shelley captured row ordinal overflowed",
            ))?,
            rowid,
            exhausted: true,
            pending_oversize_session: false,
            classification_has_valid_message: false,
            classification_all_keys_valid: false,
        })?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            shelley_locator(ShelleyCapturePhase::Conversations, rowid)?,
            self.terminal_marker_kind.clone(),
            vec![CapturedSqliteValue::Integer(rowid)],
        )
        .map_err(shelley_captured_error)
    }

    fn hydrate_message(
        &mut self,
        candidate: ShelleyMessageCandidate,
        ordinal: u64,
        all_keys_valid: bool,
    ) -> Result<SqliteLogicalRow> {
        let message_observed_bytes = candidate.observed_bytes()?;
        if message_observed_bytes > shelley_oversize_limit()? {
            let pending_oversize_session = candidate.conversation_rowid.is_some()
                && !self.previous_message_same_conversation(candidate.rowid)?;
            let next_position = encode_shelley_position(ShelleyKeyset {
                phase: ShelleyCapturePhase::Messages,
                next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Shelley captured row ordinal overflowed",
                ))?,
                rowid: candidate.rowid,
                exhausted: candidate.exhausted && !pending_oversize_session,
                pending_oversize_session,
                classification_has_valid_message: false,
                classification_all_keys_valid: all_keys_valid,
            })?;
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                shelley_locator(ShelleyCapturePhase::Messages, candidate.rowid)?,
                self.message_record_kind.clone(),
                message_observed_bytes,
            )
            .map_err(shelley_captured_error);
        }
        let parent_is_active = candidate.conversation_rowid.is_some()
            && candidate.conversation_rowid == self.active_conversation_rowid;
        let conversation_candidate = if parent_is_active {
            None
        } else {
            candidate
                .conversation_rowid
                .map(|rowid| self.fetch_message_conversation_candidate(rowid))
                .transpose()?
        };
        let observed_bytes =
            conversation_candidate
                .as_ref()
                .map_or(Ok(message_observed_bytes), |conversation| {
                    candidate
                        .retained_bytes
                        .checked_add(conversation.retained_bytes)
                        .ok_or(CaptureError::SystemInvariant(
                            "Shelley SQLite retained byte count overflowed",
                        ))
                        .and_then(shelley_observed_bytes)
                })?;
        let projectable = observed_bytes <= shelley_oversize_limit()?;
        let pending_oversize_session = !projectable
            && candidate.conversation_rowid.is_some()
            && !self.previous_message_same_conversation(candidate.rowid)?;
        let next_position = encode_shelley_position(ShelleyKeyset {
            phase: ShelleyCapturePhase::Messages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Shelley captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
            exhausted: candidate.exhausted && !pending_oversize_session,
            pending_oversize_session,
            classification_has_valid_message: false,
            classification_all_keys_valid: all_keys_valid,
        })?;
        let locator = shelley_locator(ShelleyCapturePhase::Messages, candidate.rowid)?;
        if !projectable {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.message_record_kind.clone(),
                observed_bytes,
            )
            .map_err(shelley_captured_error);
        }
        let mut values = self
            .message_hydration
            .query_row([candidate.rowid], shelley_message_values)?;
        let record_kind = if let Some(conversation) = conversation_candidate {
            #[cfg(test)]
            {
                self.message_parent_hydrations = self.message_parent_hydrations.saturating_add(1);
            }
            let conversation_values = self
                .conversation_hydration
                .query_row([conversation.rowid], shelley_conversation_values)?;
            values.extend(conversation_values);
            self.active_conversation_rowid = Some(conversation.rowid);
            self.message_record_kind.clone()
        } else {
            if candidate.conversation_rowid.is_none() {
                self.active_conversation_rowid = None;
            }
            values.push(
                candidate
                    .conversation_rowid
                    .map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer),
            );
            self.message_child_record_kind.clone()
        };
        SqliteLogicalRow::values(next_position, ordinal, locator, record_kind, values)
            .map_err(shelley_captured_error)
    }

    fn fetch_message_conversation_candidate(
        &mut self,
        rowid: i64,
    ) -> Result<ShelleyConversationCandidate> {
        with_shelley_length_preflight(self.conn, || {
            self.message_conversation_candidate
                .query_row([rowid], shelley_conversation_candidate)
        })
    }

    fn previous_message_same_conversation(&mut self, rowid: i64) -> Result<bool> {
        with_shelley_length_preflight(self.conn, || {
            self.previous_message_same_conversation
                .query_row([rowid], |row| row.get::<_, bool>(0))
        })
    }

    #[cfg(test)]
    pub(super) fn message_parent_hydrations(&self) -> usize {
        self.message_parent_hydrations
    }

    fn hydrate_pending_oversize_session(
        &mut self,
        keyset: ShelleyKeyset,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let candidate = with_shelley_length_preflight(self.conn, || {
            self.pending_session_candidate
                .query_row([keyset.rowid], shelley_row_candidate)
        })?;
        let next_position = encode_shelley_position(ShelleyKeyset {
            phase: ShelleyCapturePhase::Messages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Shelley captured row ordinal overflowed",
            ))?,
            rowid: keyset.rowid,
            exhausted: false,
            pending_oversize_session: false,
            classification_has_valid_message: false,
            classification_all_keys_valid: keyset.classification_all_keys_valid,
        })?;
        let locator = shelley_oversize_session_locator(keyset.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > shelley_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.oversize_session_record_kind.clone(),
                observed_bytes,
            )
            .map_err(shelley_captured_error);
        }
        let values = self
            .conversation_hydration
            .query_row([candidate.rowid], shelley_conversation_values)?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.oversize_session_record_kind.clone(),
            values,
        )
        .map_err(shelley_captured_error)
    }

    fn hydrate_conversation(
        &mut self,
        candidate: ShelleyRowCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next_position = encode_shelley_position(ShelleyKeyset {
            phase: ShelleyCapturePhase::Conversations,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Shelley captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
            exhausted: candidate.exhausted,
            pending_oversize_session: false,
            classification_has_valid_message: false,
            classification_all_keys_valid: false,
        })?;
        let locator = shelley_locator(ShelleyCapturePhase::Conversations, candidate.rowid)?;
        if candidate.skip_projection {
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.nonempty_conversation_record_kind.clone(),
                vec![CapturedSqliteValue::Integer(candidate.rowid)],
            )
            .map_err(shelley_captured_error);
        }
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > shelley_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.conversation_record_kind.clone(),
                observed_bytes,
            )
            .map_err(shelley_captured_error);
        }
        let values = self
            .conversation_hydration
            .query_row([candidate.rowid], shelley_conversation_values)?;
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.conversation_record_kind.clone(),
            values,
        )
        .map_err(shelley_captured_error)
    }
}

fn shelley_fetch_first_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
) -> Result<Option<ShelleyRowCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement.query_row([], shelley_row_candidate).optional()
    })
}

fn shelley_fetch_next_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: i64,
) -> Result<Option<ShelleyRowCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement
            .query_row([after_rowid], shelley_row_candidate)
            .optional()
    })
}

fn shelley_fetch_first_message_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
) -> Result<Option<ShelleyMessageCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement
            .query_row([], shelley_message_candidate)
            .optional()
    })
}

fn shelley_fetch_first_message_key_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
) -> Result<Option<ShelleyMessageKeyCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement
            .query_row([], shelley_message_key_candidate)
            .optional()
    })
}

fn shelley_fetch_next_message_key_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: i64,
) -> Result<Option<ShelleyMessageKeyCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement
            .query_row([after_rowid], shelley_message_key_candidate)
            .optional()
    })
}

fn shelley_fetch_next_message_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: i64,
) -> Result<Option<ShelleyMessageCandidate>> {
    with_shelley_length_preflight(conn, || {
        statement
            .query_row([after_rowid], shelley_message_candidate)
            .optional()
    })
}

fn shelley_fetch_next_all_valid_message_candidate(
    conn: &Connection,
    anchor_statement: &mut Statement<'_>,
    same_group_statement: &mut Statement<'_>,
    later_sequence_statement: Option<&mut Statement<'_>>,
    later_conversation_statement: &mut Statement<'_>,
    after_rowid: i64,
    has_sequence: bool,
) -> Result<Option<ShelleyMessageCandidate>> {
    let anchor = with_shelley_length_preflight(conn, || {
        anchor_statement.query_row([after_rowid], |row| {
            Ok(ShelleyMessageAnchor {
                conversation_id: row.get(0)?,
                sequence_id: row.get(1)?,
            })
        })
    })?;
    if has_sequence {
        let same_group = with_shelley_length_preflight(conn, || {
            same_group_statement
                .query_row(
                    rusqlite::params![&anchor.conversation_id, anchor.sequence_id, after_rowid],
                    shelley_message_candidate,
                )
                .optional()
        })?;
        if same_group.is_some() {
            return Ok(same_group);
        }
        let later_sequence_statement = later_sequence_statement.ok_or(
            CaptureError::SystemInvariant("Shelley sequence seek statement is missing"),
        )?;
        let later_sequence = with_shelley_length_preflight(conn, || {
            later_sequence_statement
                .query_row(
                    rusqlite::params![&anchor.conversation_id, anchor.sequence_id],
                    shelley_message_candidate,
                )
                .optional()
        })?;
        if later_sequence.is_some() {
            return Ok(later_sequence);
        }
    } else {
        let same_group = with_shelley_length_preflight(conn, || {
            same_group_statement
                .query_row(
                    rusqlite::params![&anchor.conversation_id, after_rowid],
                    shelley_message_candidate,
                )
                .optional()
        })?;
        if same_group.is_some() {
            return Ok(same_group);
        }
    }
    with_shelley_length_preflight(conn, || {
        later_conversation_statement
            .query_row(
                rusqlite::params![anchor.conversation_id],
                shelley_message_candidate,
            )
            .optional()
    })
}

fn shelley_row_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShelleyRowCandidate> {
    Ok(ShelleyRowCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
        exhausted: row.get::<_, i64>(2)? != 0,
        skip_projection: row.get::<_, i64>(3)? != 0,
    })
}

fn shelley_message_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShelleyMessageCandidate> {
    Ok(ShelleyMessageCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
        exhausted: row.get::<_, i64>(2)? != 0,
        conversation_rowid: row.get(3)?,
    })
}

fn shelley_message_key_candidate(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ShelleyMessageKeyCandidate> {
    Ok(ShelleyMessageKeyCandidate {
        rowid: row.get(0)?,
        conversation_id_bytes: row.get(1)?,
        conversation_id_is_text: row.get::<_, i64>(2)? != 0,
        last: row.get::<_, i64>(3)? != 0,
    })
}

fn shelley_conversation_candidate(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ShelleyConversationCandidate> {
    Ok(ShelleyConversationCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
    })
}

fn shelley_message_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Text(row.get(1)?),
        CapturedSqliteValue::Text(row.get(2)?),
        CapturedSqliteValue::Integer(row.get(3)?),
        CapturedSqliteValue::Text(row.get(4)?),
        shelley_optional_text_value(row.get(5)?),
        shelley_optional_text_value(row.get(6)?),
        shelley_optional_text_value(row.get(7)?),
        shelley_optional_text_value(row.get(8)?),
        shelley_optional_text_value(row.get(9)?),
        shelley_optional_integer_value(row.get(10)?),
        shelley_optional_integer_value(row.get(11)?),
        shelley_optional_text_value(row.get(12)?),
        shelley_optional_text_value(row.get(13)?),
        shelley_optional_text_value(row.get(14)?),
    ])
}

fn shelley_conversation_values(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    shelley_conversation_values_at(row, 0)
}

fn shelley_conversation_values_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        shelley_optional_text_value(row.get(offset)?),
        shelley_optional_text_value(row.get(offset + 1)?),
        shelley_optional_integer_value(row.get(offset + 2)?),
        shelley_optional_text_value(row.get(offset + 3)?),
        shelley_optional_text_value(row.get(offset + 4)?),
        shelley_optional_text_value(row.get(offset + 5)?),
        shelley_optional_integer_value(row.get(offset + 6)?),
        shelley_optional_text_value(row.get(offset + 7)?),
        shelley_optional_text_value(row.get(offset + 8)?),
        shelley_optional_text_value(row.get(offset + 9)?),
        shelley_optional_integer_value(row.get(offset + 10)?),
        shelley_optional_integer_value(row.get(offset + 11)?),
        shelley_optional_text_value(row.get(offset + 12)?),
        shelley_optional_integer_value(row.get(offset + 13)?),
        shelley_optional_text_value(row.get(offset + 14)?),
        shelley_optional_text_value(row.get(offset + 15)?),
    ])
}

fn shelley_optional_text_value(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn shelley_optional_integer_value(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

pub(super) fn initial_shelley_position() -> Result<NativePosition> {
    NativePosition::new(SHELLEY_POSITION_KIND, vec![0]).map_err(shelley_captured_error)
}

pub(super) fn encode_shelley_position(keyset: ShelleyKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(SHELLEY_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&shelley_ordered_i64(keyset.rowid).to_be_bytes());
    value.push(u8::from(keyset.exhausted));
    value.push(u8::from(keyset.pending_oversize_session));
    value.push(u8::from(keyset.classification_has_valid_message));
    value.push(u8::from(keyset.classification_all_keys_valid));
    NativePosition::new(SHELLEY_POSITION_KIND, value).map_err(shelley_captured_error)
}

pub(super) fn decode_shelley_position(position: &NativePosition) -> Result<Option<ShelleyKeyset>> {
    if position.kind() != SHELLEY_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != SHELLEY_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(ShelleyKeyset {
        phase: ShelleyCapturePhase::from_tag(position.value()[0])?,
        next_ordinal: shelley_decode_u64(&position.value()[1..9])?,
        rowid: shelley_unordered_i64(shelley_decode_u64(&position.value()[9..17])?),
        exhausted: shelley_decode_flag(position.value()[17], "exhaustion")?,
        pending_oversize_session: shelley_decode_flag(
            position.value()[18],
            "pending-oversize-session",
        )?,
        classification_has_valid_message: shelley_decode_flag(
            position.value()[19],
            "classification-has-valid-message",
        )?,
        classification_all_keys_valid: shelley_decode_flag(
            position.value()[20],
            "classification-all-keys-valid",
        )?,
    }))
}

fn shelley_oversize_session_locator(rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(3);
    value.extend_from_slice(&shelley_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(SHELLEY_LOCATOR_KIND, value).map_err(shelley_captured_error)
}

fn shelley_decode_flag(value: u8, label: &str) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley cursor has an invalid {label} flag"
        ))),
    }
}

pub(super) fn shelley_locator(phase: ShelleyCapturePhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&shelley_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(SHELLEY_LOCATOR_KIND, value).map_err(shelley_captured_error)
}

fn shelley_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Shelley cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn shelley_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn shelley_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn shelley_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Shelley byte limit exceeds u64"))
}

pub(super) fn shelley_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn shelley_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
