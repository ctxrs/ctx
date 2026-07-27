//! Persistent two-phase row production and bounded parent metadata caches.

use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRow;
use crate::captured_batch::{CapturedSqliteValue, NativePosition, ProviderRecordKind};
use crate::{CaptureError, ProviderAdapterContext, Result};

use super::cursor::{
    decode_deepagents_position, encode_deepagents_position, DeepAgentsPhase, DeepAgentsPosition,
    DeepAgentsPositionKey,
};
use super::ledger::{DeepAgentsMessageLedger, DeepAgentsWritePlan};
use super::message::deepagents_messages_from_blob;
use super::record::{
    deepagents_encode_event_indices, deepagents_encode_offsets, deepagents_locator,
    deepagents_thread_values, deepagents_write_values,
};
use super::source::{
    deepagents_checkpoint_time, deepagents_has_valid_thread, deepagents_hydrate_write,
    deepagents_next_thread_candidate, deepagents_next_write_candidate, deepagents_thread_summary,
    DeepAgentsThreadSummary, DeepAgentsWriteCandidate, DeepAgentsWriteKey,
};
use super::{
    deepagents_captured_error, deepagents_oversize_limit, DEEPAGENTS_REJECTED_WRITE_RECORD_KIND,
    DEEPAGENTS_THREAD_LOCATOR_KIND, DEEPAGENTS_THREAD_RECORD_KIND, DEEPAGENTS_WRITE_LOCATOR_KIND,
    DEEPAGENTS_WRITE_RECORD_KIND,
};
#[cfg(test)]
use super::{deepagents_trace, DeepAgentsImportTraceEvent};

pub(super) struct DeepAgentsPreparedWrite {
    pub(super) next: NativePosition,
    pub(super) ordinal: u64,
    pub(super) candidate: DeepAgentsWriteCandidate,
    pub(super) occurred_at: Option<DateTime<Utc>>,
    pub(super) plan: DeepAgentsWritePlan,
    pub(super) value_type: Option<String>,
    pub(super) value: Vec<u8>,
}

pub(super) struct DeepAgentsPreparedThread {
    pub(super) next: NativePosition,
    pub(super) ordinal: u64,
    pub(super) summary: Option<DeepAgentsThreadSummary>,
    pub(super) rejection_reason: Option<String>,
}

pub(super) struct DeepAgentsRowContinuity {
    pub(super) before: NativePosition,
    pub(super) next: NativePosition,
}

pub(super) struct DeepAgentsCheckpointTimeCache {
    pub(super) checkpoint_id: String,
    pub(super) time: DateTime<Utc>,
}

pub(super) struct DeepAgentsThreadCache {
    pub(super) thread_id: String,
    pub(super) exists: bool,
    pub(super) current_checkpoint: Option<DeepAgentsCheckpointTimeCache>,
}

pub(super) struct DeepAgentsRowFetcher<'connection> {
    pub(super) conn: &'connection Connection,
    pub(super) context: ProviderAdapterContext,
    pub(super) message_ledger: DeepAgentsMessageLedger,
    pub(super) last_emitted: Option<DeepAgentsRowContinuity>,
    pub(super) thread_cache: Option<DeepAgentsThreadCache>,
    pub(super) write_record_kind: ProviderRecordKind,
    pub(super) rejected_write_record_kind: ProviderRecordKind,
    pub(super) thread_record_kind: ProviderRecordKind,
}

impl<'connection> DeepAgentsRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        context: ProviderAdapterContext,
        committed_store: Option<Store>,
    ) -> Result<Self> {
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|source_path| source_path.display().to_string());
        Ok(Self {
            conn,
            context,
            message_ledger: DeepAgentsMessageLedger::new(committed_store, raw_source_path),
            last_emitted: None,
            thread_cache: None,
            write_record_kind: ProviderRecordKind::new(DEEPAGENTS_WRITE_RECORD_KIND)
                .map_err(deepagents_captured_error)?,
            rejected_write_record_kind: ProviderRecordKind::new(
                DEEPAGENTS_REJECTED_WRITE_RECORD_KIND,
            )
            .map_err(deepagents_captured_error)?,
            thread_record_kind: ProviderRecordKind::new(DEEPAGENTS_THREAD_RECORD_KIND)
                .map_err(deepagents_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        if let Some(last_emitted) = self.last_emitted.as_ref() {
            if last_emitted.before == after {
                return Err(CaptureError::SystemInvariant(
                    "Deep Agents row fetcher cannot rehydrate an emitted position",
                ));
            }
            if last_emitted.next != after {
                return Err(CaptureError::SystemInvariant(
                    "Deep Agents row fetcher received a noncontiguous position",
                ));
            }
        }

        let decoded = decode_deepagents_position(&after)?;
        let next_ordinal = decoded.as_ref().map_or(0, |position| position.next_ordinal);
        let phase = decoded
            .as_ref()
            .map_or(DeepAgentsPhase::Threads, |position| match &position.key {
                DeepAgentsPositionKey::Write { .. } => DeepAgentsPhase::Writes,
                DeepAgentsPositionKey::Thread { .. } => DeepAgentsPhase::Threads,
            });
        if phase == DeepAgentsPhase::Threads {
            let after_thread = decoded.as_ref().and_then(|position| match &position.key {
                DeepAgentsPositionKey::Thread { rowid } => Some(*rowid),
                DeepAgentsPositionKey::Write { .. } => None,
            });
            if let Some(candidate) = deepagents_next_thread_candidate(self.conn, after_thread)? {
                self.message_ledger.begin_row();
                let summary = match candidate.thread_id.as_deref() {
                    Some(thread_id) => {
                        deepagents_thread_summary(self.conn, &self.context, thread_id, None)?
                    }
                    None => None,
                };
                let rejection_reason = candidate.rejection_reason.or_else(|| {
                    summary.is_none().then(|| {
                        "Deep Agents thread has no valid bounded checkpoint metadata".to_owned()
                    })
                });
                if let Some(thread_id) = candidate.thread_id.as_ref() {
                    self.thread_cache = Some(DeepAgentsThreadCache {
                        thread_id: thread_id.clone(),
                        exists: summary.is_some(),
                        current_checkpoint: summary.as_ref().and_then(|summary| {
                            summary
                                .thread
                                .latest_checkpoint_id
                                .clone()
                                .map(|checkpoint_id| DeepAgentsCheckpointTimeCache {
                                    checkpoint_id,
                                    time: summary.thread.updated_at,
                                })
                        }),
                    });
                }
                let next = encode_deepagents_position(DeepAgentsPosition {
                    next_ordinal: next_ordinal.checked_add(1).ok_or(
                        CaptureError::SystemInvariant("Deep Agents row ordinal overflowed"),
                    )?,
                    key: DeepAgentsPositionKey::Thread {
                        rowid: candidate.rowid,
                    },
                })?;
                let prepared = DeepAgentsPreparedThread {
                    next,
                    ordinal: next_ordinal,
                    summary,
                    rejection_reason,
                };
                let next = prepared.next.clone();
                let row = self.thread_row(prepared)?;
                self.last_emitted = Some(DeepAgentsRowContinuity {
                    before: after,
                    next,
                });
                return Ok(Some(row));
            }
        }

        let after_rowid = decoded.as_ref().and_then(|position| match &position.key {
            DeepAgentsPositionKey::Write { rowid, .. } => Some(*rowid),
            DeepAgentsPositionKey::Thread { .. } => None,
        });
        let Some(candidate) = deepagents_next_write_candidate(self.conn, after_rowid)? else {
            return Ok(None);
        };
        let start_event_index = decoded.as_ref().map_or(1, |position| match &position.key {
            DeepAgentsPositionKey::Write {
                next_event_index, ..
            } if candidate.same_thread_as_prior => *next_event_index,
            _ => 1,
        });
        self.message_ledger.begin_row();
        let prepared = self.prepare_write(next_ordinal, candidate, start_event_index)?;
        let next = prepared.next.clone();
        let row = self.write_row(prepared)?;
        self.last_emitted = Some(DeepAgentsRowContinuity {
            before: after,
            next,
        });
        Ok(Some(row))
    }

    fn prepare_write(
        &mut self,
        ordinal: u64,
        candidate: DeepAgentsWriteCandidate,
        start_event_index: u64,
    ) -> Result<DeepAgentsPreparedWrite> {
        let preflight_bytes = candidate.observed_bytes()?;
        let mut occurred_at = None;
        let mut value_type = None;
        let mut value = Vec::new();
        let plan = if preflight_bytes > deepagents_oversize_limit()? {
            DeepAgentsWritePlan::Oversize {
                observed_bytes: preflight_bytes,
            }
        } else if let Some(reason) = candidate.rejection_reason.clone() {
            DeepAgentsWritePlan::RejectedKey(reason)
        } else {
            let key = candidate.key.as_ref().ok_or(CaptureError::SystemInvariant(
                "Deep Agents accepted write candidate is missing its key",
            ))?;
            occurred_at = self.checkpoint_time_for_write(key)?;
            if occurred_at.is_none() {
                DeepAgentsWritePlan::UnknownThread
            } else {
                (value_type, value) = deepagents_hydrate_write(self.conn, candidate.rowid)?;
                #[cfg(test)]
                deepagents_trace(DeepAgentsImportTraceEvent::WriteHydrated(candidate.rowid));
                match deepagents_messages_from_blob(value_type.as_deref(), &value) {
                    Ok(messages) => self.message_ledger.plan_messages(
                        &key.thread_id,
                        &messages,
                        preflight_bytes,
                        start_event_index,
                    )?,
                    Err(_) => DeepAgentsWritePlan::DecodeRejected,
                }
            }
        };
        let next_event_index = match &plan {
            DeepAgentsWritePlan::Accepted {
                next_event_index, ..
            } => *next_event_index,
            DeepAgentsWritePlan::UnknownThread
            | DeepAgentsWritePlan::DecodeRejected
            | DeepAgentsWritePlan::RejectedKey(_)
            | DeepAgentsWritePlan::Oversize { .. } => start_event_index,
        };
        let next = encode_deepagents_position(DeepAgentsPosition {
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Deep Agents row ordinal overflowed",
            ))?,
            key: DeepAgentsPositionKey::Write {
                rowid: candidate.rowid,
                next_event_index,
            },
        })?;
        Ok(DeepAgentsPreparedWrite {
            next,
            ordinal,
            candidate,
            occurred_at,
            plan,
            value_type,
            value,
        })
    }

    pub(super) fn reset_for_batch_request(&mut self) {
        self.message_ledger.reset_for_batch_request();
    }

    #[cfg(test)]
    pub(super) fn retained_dedupe_key_counts(&self) -> (usize, usize) {
        self.message_ledger.retained_key_counts()
    }

    fn write_row(&self, write: DeepAgentsPreparedWrite) -> Result<SqliteLogicalRow> {
        let locator = deepagents_locator(DEEPAGENTS_WRITE_LOCATOR_KIND, &write.candidate.rowid)?;
        if let DeepAgentsWritePlan::Oversize { observed_bytes } = &write.plan {
            return SqliteLogicalRow::oversize(
                write.next.clone(),
                write.ordinal,
                locator,
                self.write_record_kind.clone(),
                *observed_bytes,
            )
            .map_err(deepagents_captured_error);
        }
        if let DeepAgentsWritePlan::RejectedKey(reason) = &write.plan {
            return SqliteLogicalRow::values(
                write.next.clone(),
                write.ordinal,
                locator,
                self.rejected_write_record_kind.clone(),
                vec![
                    CapturedSqliteValue::Blob(
                        write.ordinal.saturating_add(1).to_be_bytes().to_vec(),
                    ),
                    CapturedSqliteValue::Text(reason.clone()),
                ],
            )
            .map_err(deepagents_captured_error);
        }
        let (accepted_event_indices, accepted_offsets) = match &write.plan {
            DeepAgentsWritePlan::Accepted {
                accepted_offsets,
                accepted_event_indices,
                ..
            } => (
                deepagents_encode_event_indices(accepted_event_indices),
                deepagents_encode_offsets(accepted_offsets),
            ),
            DeepAgentsWritePlan::UnknownThread | DeepAgentsWritePlan::DecodeRejected => {
                (Vec::new(), Vec::new())
            }
            DeepAgentsWritePlan::RejectedKey(_) => {
                return Err(CaptureError::SystemInvariant(
                    "Deep Agents rejected write reached accepted hydration",
                ));
            }
            DeepAgentsWritePlan::Oversize { .. } => {
                return Err(CaptureError::SystemInvariant(
                    "Deep Agents oversize write reached hydration",
                ));
            }
        };
        let values = deepagents_write_values(
            write.ordinal,
            &write.candidate,
            write.occurred_at,
            write.value_type,
            write.value,
            accepted_event_indices,
            accepted_offsets,
        )?;
        SqliteLogicalRow::values(
            write.next.clone(),
            write.ordinal,
            locator,
            self.write_record_kind.clone(),
            values,
        )
        .map_err(deepagents_captured_error)
    }

    fn thread_row(&self, record: DeepAgentsPreparedThread) -> Result<SqliteLogicalRow> {
        if let Some(reason) = record.rejection_reason {
            let locator = deepagents_locator(DEEPAGENTS_THREAD_LOCATOR_KIND, &record.ordinal)?;
            return SqliteLogicalRow::values(
                record.next.clone(),
                record.ordinal,
                locator,
                self.rejected_write_record_kind.clone(),
                vec![
                    CapturedSqliteValue::Blob(
                        record.ordinal.saturating_add(1).to_be_bytes().to_vec(),
                    ),
                    CapturedSqliteValue::Text(reason),
                ],
            )
            .map_err(deepagents_captured_error);
        }
        let summary = record.summary.ok_or(CaptureError::SystemInvariant(
            "Deep Agents accepted thread is missing its summary",
        ))?;
        let locator =
            deepagents_locator(DEEPAGENTS_THREAD_LOCATOR_KIND, &summary.thread.thread_id)?;
        SqliteLogicalRow::values(
            record.next.clone(),
            record.ordinal,
            locator,
            self.thread_record_kind.clone(),
            deepagents_thread_values(&summary),
        )
        .map_err(deepagents_captured_error)
    }

    fn checkpoint_time_for_write(
        &mut self,
        key: &DeepAgentsWriteKey,
    ) -> Result<Option<DateTime<Utc>>> {
        let cache_matches = self
            .thread_cache
            .as_ref()
            .is_some_and(|cache| cache.thread_id == key.thread_id);
        if !cache_matches {
            let exists = deepagents_has_valid_thread(self.conn, &key.thread_id)?;
            self.thread_cache = Some(DeepAgentsThreadCache {
                thread_id: key.thread_id.clone(),
                exists,
                current_checkpoint: None,
            });
        }
        let exists = self.thread_cache.as_ref().is_some_and(|cache| cache.exists);
        if !exists {
            return Ok(None);
        }
        let cached_checkpoint_time = self
            .thread_cache
            .as_ref()
            .and_then(|cache| cache.current_checkpoint.as_ref())
            .filter(|checkpoint| checkpoint.checkpoint_id == key.checkpoint_id)
            .map(|checkpoint| checkpoint.time);
        let current_checkpoint_time = match cached_checkpoint_time {
            Some(time) => time,
            None => {
                let time = deepagents_checkpoint_time(
                    self.conn,
                    &self.context,
                    &key.thread_id,
                    &key.checkpoint_id,
                )?
                .unwrap_or(self.context.imported_at);
                let cache = self
                    .thread_cache
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Deep Agents thread cache disappeared during checkpoint lookup",
                    ))?;
                cache.current_checkpoint = Some(DeepAgentsCheckpointTimeCache {
                    checkpoint_id: key.checkpoint_id.clone(),
                    time,
                });
                time
            }
        };
        Ok(Some(current_checkpoint_time))
    }
}
