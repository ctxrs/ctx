use super::super::super::partials::try_coalesce_partial_delta_tail;
use super::super::types::HeadBatchPushError;
use super::{HeadBatchDrain, HEAD_BATCH_TOTAL_LIMIT};
use crate::api::ws::HEAD_BATCH_SESSION_LIMIT;
use ctx_core::ids::SessionId;
use ctx_core::models::{SessionHeadDelta, WorkspaceActiveSnapshotStreamSource};
use ctx_workspace_active_snapshot::SessionReplayCursor;
use std::collections::HashMap;
use std::time::Instant;

struct QueuedHeadDelta {
    enqueued_at: Instant,
    delta: SessionHeadDelta,
    stream_source: WorkspaceActiveSnapshotStreamSource,
}

pub(super) struct HeadBatchState {
    snapshot_rev: i64,
    total_len: usize,
    deltas: HashMap<SessionId, Vec<QueuedHeadDelta>>,
}

impl HeadBatchState {
    pub(super) fn new() -> Self {
        Self {
            snapshot_rev: 0,
            total_len: 0,
            deltas: HashMap::new(),
        }
    }

    pub(super) fn push(
        &mut self,
        snapshot_rev: i64,
        delta: SessionHeadDelta,
        stream_source: WorkspaceActiveSnapshotStreamSource,
    ) -> Result<(), HeadBatchPushError> {
        let session_id = delta.session_id;
        self.snapshot_rev = self.snapshot_rev.max(snapshot_rev);
        if let Some(entry) = self.deltas.get_mut(&session_id) {
            if let Some(prev) = entry.last_mut() {
                if prev.stream_source == stream_source
                    && try_coalesce_partial_delta_tail(&mut prev.delta, &delta)
                {
                    return Ok(());
                }
            }
        }
        if self.total_len >= HEAD_BATCH_TOTAL_LIMIT {
            return Err(HeadBatchPushError::TotalLimit {
                limit: HEAD_BATCH_TOTAL_LIMIT,
            });
        }
        {
            let entry = self.deltas.entry(session_id).or_default();
            if entry.len() >= HEAD_BATCH_SESSION_LIMIT {
                return Err(HeadBatchPushError::SessionLimit {
                    session_id,
                    limit: HEAD_BATCH_SESSION_LIMIT,
                });
            }
            entry.push(QueuedHeadDelta {
                enqueued_at: Instant::now(),
                delta,
                stream_source,
            });
        }
        self.total_len += 1;
        Ok(())
    }

    pub(super) fn take_chunk_with_meta(&mut self, limit: usize) -> HeadBatchDrain {
        if self.deltas.is_empty() {
            self.total_len = 0;
            return HeadBatchDrain {
                snapshot_rev: self.snapshot_rev,
                deltas: Vec::new(),
                oldest_queued_ms: 0,
                stream_source: WorkspaceActiveSnapshotStreamSource::Live,
            };
        }
        if limit == 0 {
            return HeadBatchDrain {
                snapshot_rev: self.snapshot_rev,
                deltas: Vec::new(),
                oldest_queued_ms: 0,
                stream_source: WorkspaceActiveSnapshotStreamSource::Live,
            };
        }
        let snapshot_rev = self.snapshot_rev;
        let stream_source = self
            .deltas
            .values()
            .find_map(|per_session| per_session.first().map(|queued| queued.stream_source))
            .unwrap_or(WorkspaceActiveSnapshotStreamSource::Live);
        let mut deltas = Vec::with_capacity(self.total_len.min(limit));
        let mut oldest_enqueued_at: Option<Instant> = None;
        let mut empty_sessions = Vec::new();
        let session_ids: Vec<SessionId> = self.deltas.keys().copied().collect();
        for session_id in session_ids {
            if deltas.len() >= limit {
                break;
            }
            let Some(per_session) = self.deltas.get_mut(&session_id) else {
                continue;
            };
            let source_prefix = per_session
                .iter()
                .take_while(|queued| queued.stream_source == stream_source)
                .count();
            let take_count = (limit - deltas.len()).min(source_prefix);
            for queued in per_session.drain(..take_count) {
                if oldest_enqueued_at
                    .map(|current| queued.enqueued_at < current)
                    .unwrap_or(true)
                {
                    oldest_enqueued_at = Some(queued.enqueued_at);
                }
                deltas.push(queued.delta);
            }
            if per_session.is_empty() {
                empty_sessions.push(session_id);
            }
        }
        for session_id in empty_sessions {
            self.deltas.remove(&session_id);
        }
        self.total_len = self.total_len.saturating_sub(deltas.len());
        if self.total_len == 0 {
            self.snapshot_rev = 0;
        }
        HeadBatchDrain {
            snapshot_rev,
            deltas,
            oldest_queued_ms: oldest_enqueued_at
                .map(|enqueued_at| enqueued_at.elapsed().as_millis())
                .unwrap_or(0),
            stream_source,
        }
    }

    pub(super) fn clear(&mut self) {
        self.deltas.clear();
        self.total_len = 0;
        self.snapshot_rev = 0;
    }

    pub(super) fn drop_session_deltas_at_or_before<F>(
        &mut self,
        session_id: SessionId,
        cursor: SessionReplayCursor,
        is_delta_after_cursor: F,
    ) where
        F: Fn(&SessionHeadDelta, SessionReplayCursor) -> bool,
    {
        let mut remove_entry = false;
        let removed = if let Some(entry) = self.deltas.get_mut(&session_id) {
            let before = entry.len();
            entry.retain(|queued| is_delta_after_cursor(&queued.delta, cursor));
            remove_entry = entry.is_empty();
            before.saturating_sub(entry.len())
        } else {
            0
        };
        if remove_entry {
            self.deltas.remove(&session_id);
        }
        self.total_len = self.total_len.saturating_sub(removed);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }
}
