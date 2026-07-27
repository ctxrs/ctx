#[cfg(test)]
use std::cell::Cell;

use rusqlite::{Connection, OptionalExtension};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRow;
use crate::captured_batch::{CapturedSqliteValue, NativePosition, ProviderRecordKind};
use crate::{CaptureError, Result};

use super::codec::{
    astrbot_captured_error, astrbot_captured_optional_integer, astrbot_conversation_values,
    astrbot_locator, astrbot_oversize_limit, astrbot_platform_message_values,
    decode_astrbot_position, encode_astrbot_position, AstrBotConversationRow, AstrBotKeyset,
    AstrBotParserCheckpoint, AstrBotPhase, AstrBotPlatformMessageLink, AstrBotPlatformMessageRow,
    ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND, ASTRBOT_CONVERSATION_RECORD_KIND,
    ASTRBOT_PLATFORM_MESSAGE_ORDER_VIOLATION_RECORD_KIND, ASTRBOT_PLATFORM_MESSAGE_RECORD_KIND,
};
use super::relationships::{
    astrbot_relationship_projection_exists, ASTRBOT_RELATIONSHIP_LOOKUP_SQL,
    ASTRBOT_RELATIONSHIP_RETAINED_BYTES_SQL,
};
use super::source::{with_astrbot_length_preflight, AstrBotSql};

const ASTRBOT_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 32 * 24;

#[cfg(test)]
thread_local! {
    static ASTRBOT_CONVERSATION_HYDRATION_TEST_COUNT: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn astrbot_hydrate_conversation(
    conn: &Connection,
    hydration_sql: &str,
    physical_rowid: i64,
) -> Result<AstrBotConversationRow> {
    #[cfg(test)]
    ASTRBOT_CONVERSATION_HYDRATION_TEST_COUNT
        .with(|count| count.set(count.get().saturating_add(1)));
    conn.query_row(
        hydration_sql,
        [physical_rowid],
        astrbot_conversation_from_row,
    )
    .map_err(CaptureError::from)
}

#[cfg(test)]
pub(super) fn astrbot_reset_conversation_hydration_test_count() {
    ASTRBOT_CONVERSATION_HYDRATION_TEST_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn astrbot_conversation_hydration_test_count() -> usize {
    ASTRBOT_CONVERSATION_HYDRATION_TEST_COUNT.with(Cell::get)
}

pub(super) struct AstrBotRowCandidate {
    pub(super) physical_rowid: i64,
    pub(super) retained_bytes: i64,
    pub(super) legacy_order: AstrBotLegacyOrderKey,
}

impl AstrBotRowCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        let retained = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "AstrBot SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        ASTRBOT_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(retained)
            .ok_or(CaptureError::SystemInvariant(
                "AstrBot SQLite retained byte count overflowed",
            ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AstrBotLegacyOrderKey {
    pub(super) timestamp_is_present: bool,
    pub(super) timestamp: i64,
    pub(super) logical_id: i64,
    pub(super) physical_rowid: i64,
}

pub(super) struct AstrBotRowFetcher<'connection> {
    conn: &'connection Connection,
    sql: AstrBotSql,
    relationship_projection_prepared: bool,
    previous_conversation_order: Option<AstrBotLegacyOrderKey>,
    previous_platform_message_order: Option<AstrBotLegacyOrderKey>,
    conversation_record_kind: ProviderRecordKind,
    platform_message_record_kind: ProviderRecordKind,
    conversation_order_violation_record_kind: ProviderRecordKind,
    platform_message_order_violation_record_kind: ProviderRecordKind,
}

impl<'connection> AstrBotRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        sql: AstrBotSql,
        checkpoint: AstrBotParserCheckpoint,
    ) -> Result<Self> {
        checkpoint.validate()?;
        if !checkpoint.source_shape_validated {
            return Err(CaptureError::SystemInvariant(
                "AstrBot row fetcher requires a validated source shape",
            ));
        }
        Ok(Self {
            conn,
            sql,
            relationship_projection_prepared: astrbot_relationship_projection_exists(conn)?,
            previous_conversation_order: None,
            previous_platform_message_order: None,
            conversation_record_kind: ProviderRecordKind::new(ASTRBOT_CONVERSATION_RECORD_KIND)
                .map_err(astrbot_captured_error)?,
            platform_message_record_kind: ProviderRecordKind::new(
                ASTRBOT_PLATFORM_MESSAGE_RECORD_KIND,
            )
            .map_err(astrbot_captured_error)?,
            conversation_order_violation_record_kind: ProviderRecordKind::new(
                ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND,
            )
            .map_err(astrbot_captured_error)?,
            platform_message_order_violation_record_kind: ProviderRecordKind::new(
                ASTRBOT_PLATFORM_MESSAGE_ORDER_VIOLATION_RECORD_KIND,
            )
            .map_err(astrbot_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_astrbot_position(&after)?;
        let ordinal = keyset.map_or(0, |value| value.next_ordinal);
        match keyset.map(|value| value.phase) {
            None | Some(AstrBotPhase::Conversations) => {
                let after_rowid = keyset.map(|value| value.physical_rowid);
                if let Some(candidate) = self.next_conversation_candidate(after_rowid)? {
                    if !self.validate_conversation_order(&candidate, after_rowid)? {
                        return self
                            .order_violation_row(candidate, ordinal, AstrBotPhase::Conversations)
                            .map(Some);
                    }
                    return self.hydrate_conversation(candidate, ordinal).map(Some);
                }
                let Some(candidate) = self.next_platform_message_candidate(None)? else {
                    return Ok(None);
                };
                if !self.validate_platform_message_order(&candidate, None)? {
                    return self
                        .order_violation_row(candidate, ordinal, AstrBotPhase::PlatformMessages)
                        .map(Some);
                }
                self.hydrate_platform_message(candidate, ordinal).map(Some)
            }
            Some(AstrBotPhase::PlatformMessages) => {
                let after_rowid = keyset.map(|value| value.physical_rowid);
                let Some(candidate) = self.next_platform_message_candidate(after_rowid)? else {
                    return Ok(None);
                };
                if !self.validate_platform_message_order(&candidate, after_rowid)? {
                    return self
                        .order_violation_row(candidate, ordinal, AstrBotPhase::PlatformMessages)
                        .map(Some);
                }
                self.hydrate_platform_message(candidate, ordinal).map(Some)
            }
        }
    }

    pub(super) fn validate_conversation_order(
        &mut self,
        candidate: &AstrBotRowCandidate,
        after_rowid: Option<i64>,
    ) -> Result<bool> {
        let previous = match self.previous_conversation_order {
            Some(previous) => Some(previous),
            None => {
                astrbot_fetch_order_at(self.conn, &self.sql.conversation_order_at, after_rowid)?
            }
        };
        let valid = astrbot_frontier_order_is_valid(previous, candidate.legacy_order);
        self.previous_conversation_order = Some(candidate.legacy_order);
        Ok(valid)
    }

    pub(super) fn validate_platform_message_order(
        &mut self,
        candidate: &AstrBotRowCandidate,
        after_rowid: Option<i64>,
    ) -> Result<bool> {
        let Some(order_at) = self.sql.platform_message_order_at.as_deref() else {
            return Err(CaptureError::SystemInvariant(
                "AstrBot platform-message order SQL is missing",
            ));
        };
        let previous = match self.previous_platform_message_order {
            Some(previous) => Some(previous),
            None => astrbot_fetch_order_at(self.conn, order_at, after_rowid)?,
        };
        let valid = astrbot_frontier_order_is_valid(previous, candidate.legacy_order);
        self.previous_platform_message_order = Some(candidate.legacy_order);
        Ok(valid)
    }

    pub(super) fn order_violation_row(
        &self,
        candidate: AstrBotRowCandidate,
        ordinal: u64,
        phase: AstrBotPhase,
    ) -> Result<SqliteLogicalRow> {
        let next = encode_astrbot_position(AstrBotKeyset {
            phase,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "AstrBot captured row ordinal overflowed",
            ))?,
            physical_rowid: candidate.physical_rowid,
        })?;
        let locator = astrbot_locator(phase, candidate.physical_rowid)?;
        let record_kind = match phase {
            AstrBotPhase::Conversations => self.conversation_order_violation_record_kind.clone(),
            AstrBotPhase::PlatformMessages => {
                self.platform_message_order_violation_record_kind.clone()
            }
        };
        SqliteLogicalRow::values(next, ordinal, locator, record_kind, Vec::new())
            .map_err(astrbot_captured_error)
    }

    pub(super) fn next_conversation_candidate(
        &self,
        after_rowid: Option<i64>,
    ) -> Result<Option<AstrBotRowCandidate>> {
        astrbot_fetch_candidate(
            self.conn,
            &self.sql.conversation_candidate_initial,
            &self.sql.conversation_candidate_after,
            after_rowid,
        )
    }

    pub(super) fn next_platform_message_candidate(
        &self,
        after_rowid: Option<i64>,
    ) -> Result<Option<AstrBotRowCandidate>> {
        let (Some(initial_sql), Some(after_sql)) = (
            self.sql.platform_message_candidate_initial.as_deref(),
            self.sql.platform_message_candidate_after.as_deref(),
        ) else {
            return Ok(None);
        };
        astrbot_fetch_candidate(self.conn, initial_sql, after_sql, after_rowid)
    }

    pub(super) fn hydrate_conversation(
        &mut self,
        candidate: AstrBotRowCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next = encode_astrbot_position(AstrBotKeyset {
            phase: AstrBotPhase::Conversations,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "AstrBot captured row ordinal overflowed",
            ))?,
            physical_rowid: candidate.physical_rowid,
        })?;
        let locator = astrbot_locator(AstrBotPhase::Conversations, candidate.physical_rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > astrbot_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next,
                ordinal,
                locator,
                self.conversation_record_kind.clone(),
                observed_bytes,
            )
            .map_err(astrbot_captured_error);
        }
        let conversation = astrbot_hydrate_conversation(
            self.conn,
            &self.sql.conversation_hydration,
            candidate.physical_rowid,
        )?;
        SqliteLogicalRow::values(
            next,
            ordinal,
            locator,
            self.conversation_record_kind.clone(),
            astrbot_conversation_values(conversation),
        )
        .map_err(astrbot_captured_error)
    }

    pub(super) fn hydrate_platform_message(
        &mut self,
        candidate: AstrBotRowCandidate,
        ordinal: u64,
    ) -> Result<SqliteLogicalRow> {
        let next = encode_astrbot_position(AstrBotKeyset {
            phase: AstrBotPhase::PlatformMessages,
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "AstrBot captured row ordinal overflowed",
            ))?,
            physical_rowid: candidate.physical_rowid,
        })?;
        let locator = astrbot_locator(AstrBotPhase::PlatformMessages, candidate.physical_rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > astrbot_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next,
                ordinal,
                locator,
                self.platform_message_record_kind.clone(),
                observed_bytes,
            )
            .map_err(astrbot_captured_error);
        }
        let hydration =
            self.sql
                .platform_message_hydration
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "AstrBot platform-message hydration SQL is missing",
                ))?;
        let message = self.conn.query_row(
            hydration,
            [candidate.physical_rowid],
            astrbot_platform_message_from_row,
        )?;
        let linked_bytes = self
            .linked_provider_session_retained_bytes(message.llm_checkpoint_id.as_deref())?
            .map_or(Ok(0_u64), |bytes| {
                u64::try_from(bytes).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "AstrBot linked provider-session byte count must be nonnegative".to_owned(),
                    )
                })
            })?;
        let combined_bytes =
            observed_bytes
                .checked_add(linked_bytes)
                .ok_or(CaptureError::SystemInvariant(
                    "AstrBot joined retained byte count overflowed",
                ))?;
        if combined_bytes > astrbot_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next,
                ordinal,
                locator,
                self.platform_message_record_kind.clone(),
                combined_bytes,
            )
            .map_err(astrbot_captured_error);
        }
        let link = self.linked_platform_message_parent(message.llm_checkpoint_id.as_deref())?;
        let mut values = astrbot_platform_message_values(message);
        match link {
            Some(link) => {
                values.push(CapturedSqliteValue::Text(link.provider_session_id));
                values.push(astrbot_captured_optional_integer(link.parent_created_at));
            }
            None => {
                values.push(CapturedSqliteValue::Null);
                values.push(CapturedSqliteValue::Null);
            }
        }
        SqliteLogicalRow::values(
            next,
            ordinal,
            locator,
            self.platform_message_record_kind.clone(),
            values,
        )
        .map_err(astrbot_captured_error)
    }

    pub(super) fn linked_provider_session_retained_bytes(
        &self,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<i64>> {
        self.linked_provider_session_value(checkpoint_id, ASTRBOT_RELATIONSHIP_RETAINED_BYTES_SQL)
    }

    pub(super) fn linked_platform_message_parent(
        &self,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<AstrBotPlatformMessageLink>> {
        let Some(checkpoint_id) = checkpoint_id else {
            return Ok(None);
        };
        if !self.relationship_projection_prepared {
            return Err(CaptureError::SystemInvariant(
                "AstrBot relationship projection was not prepared for a linked platform message",
            ));
        }
        self.conn
            .query_row(ASTRBOT_RELATIONSHIP_LOOKUP_SQL, [checkpoint_id], |row| {
                Ok(AstrBotPlatformMessageLink {
                    provider_session_id: row.get(0)?,
                    parent_created_at: row.get(1)?,
                })
            })
            .optional()
            .map_err(CaptureError::from)
    }

    pub(super) fn linked_provider_session_value<T>(
        &self,
        checkpoint_id: Option<&str>,
        sql: &str,
    ) -> Result<Option<T>>
    where
        T: rusqlite::types::FromSql,
    {
        let Some(checkpoint_id) = checkpoint_id else {
            return Ok(None);
        };
        if !self.relationship_projection_prepared {
            return Err(CaptureError::SystemInvariant(
                "AstrBot relationship projection was not prepared for a linked platform message",
            ));
        }
        self.conn
            .query_row(sql, [checkpoint_id], |row| row.get(0))
            .optional()
            .map_err(CaptureError::from)
    }
}

pub(super) fn astrbot_fetch_candidate(
    conn: &Connection,
    initial_sql: &str,
    after_sql: &str,
    after_rowid: Option<i64>,
) -> Result<Option<AstrBotRowCandidate>> {
    let map_row = |row: &rusqlite::Row<'_>| {
        let physical_rowid = row.get(0)?;
        let timestamp = row.get::<_, Option<i64>>(2)?;
        Ok(AstrBotRowCandidate {
            physical_rowid,
            retained_bytes: row.get(1)?,
            legacy_order: AstrBotLegacyOrderKey {
                timestamp_is_present: timestamp.is_some(),
                timestamp: timestamp.unwrap_or(0),
                logical_id: row.get(3)?,
                physical_rowid,
            },
        })
    };
    with_astrbot_length_preflight(conn, || {
        match after_rowid {
            Some(rowid) => conn.query_row(after_sql, [rowid], map_row),
            None => conn.query_row(initial_sql, [], map_row),
        }
        .optional()
    })
}

pub(super) fn astrbot_fetch_order_at(
    conn: &Connection,
    sql: &str,
    physical_rowid: Option<i64>,
) -> Result<Option<AstrBotLegacyOrderKey>> {
    let Some(physical_rowid) = physical_rowid else {
        return Ok(None);
    };
    conn.query_row(sql, [physical_rowid], |row| {
        let physical_rowid = row.get(0)?;
        let timestamp = row.get::<_, Option<i64>>(1)?;
        Ok(AstrBotLegacyOrderKey {
            timestamp_is_present: timestamp.is_some(),
            timestamp: timestamp.unwrap_or(0),
            logical_id: row.get(2)?,
            physical_rowid,
        })
    })
    .optional()
    .map_err(CaptureError::from)
}

pub(super) fn astrbot_frontier_order_is_valid(
    previous: Option<AstrBotLegacyOrderKey>,
    current: AstrBotLegacyOrderKey,
) -> bool {
    previous.is_none_or(|previous| previous <= current)
}

pub(super) fn astrbot_conversation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AstrBotConversationRow> {
    Ok(AstrBotConversationRow {
        row_id: row.get(0)?,
        inner_conversation_id: row.get(1)?,
        conversation_id: row.get(2)?,
        platform_id: row.get(3)?,
        user_id: row.get(4)?,
        content: row.get(5)?,
        title: row.get(6)?,
        persona_id: row.get(7)?,
        token_usage: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

pub(super) fn astrbot_platform_message_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AstrBotPlatformMessageRow> {
    Ok(AstrBotPlatformMessageRow {
        id: row.get(0)?,
        platform_id: row.get(1)?,
        user_id: row.get(2)?,
        sender_id: row.get(3)?,
        sender_name: row.get(4)?,
        content: row.get(5)?,
        llm_checkpoint_id: row.get(6)?,
        created_at: row.get(7)?,
    })
}
