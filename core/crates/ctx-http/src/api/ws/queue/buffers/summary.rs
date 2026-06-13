use super::types::{SummaryBatchEvent, SummaryBatchPushError, SummaryBatchPushOutcome};
use super::*;

struct SummaryBatchState {
    total_len: usize,
    session_events: HashMap<SessionId, SummaryBatchEvent>,
}

pub(crate) struct SummaryBatchBuffer {
    state: Mutex<SummaryBatchState>,
    notify: Notify,
    limit: usize,
}

impl SummaryBatchBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(SummaryBatchState {
                total_len: 0,
                session_events: HashMap::new(),
            }),
            notify: Notify::new(),
            limit,
        }
    }

    pub(crate) async fn push(
        &self,
        event: WorkspaceActiveSnapshotEvent,
    ) -> Result<SummaryBatchPushOutcome, SummaryBatchPushError> {
        self.push_with_source(event, WorkspaceActiveSnapshotStreamSource::Live)
            .await
    }

    pub(crate) async fn push_with_source(
        &self,
        event: WorkspaceActiveSnapshotEvent,
        stream_source: WorkspaceActiveSnapshotStreamSource,
    ) -> Result<SummaryBatchPushOutcome, SummaryBatchPushError> {
        let mut state = self.state.lock().await;
        match &event {
            WorkspaceActiveSnapshotEvent::SessionSummaryDelta { delta, .. } => {
                if let std::collections::hash_map::Entry::Occupied(mut entry) =
                    state.session_events.entry(delta.session_id)
                {
                    entry.insert(SummaryBatchEvent {
                        event,
                        stream_source,
                    });
                    self.notify.notify_one();
                    return Ok(SummaryBatchPushOutcome::Replaced);
                }
                if state.total_len >= self.limit {
                    return Err(SummaryBatchPushError::TotalLimit { limit: self.limit });
                }
                state.session_events.insert(
                    delta.session_id,
                    SummaryBatchEvent {
                        event,
                        stream_source,
                    },
                );
            }
            _ => return Ok(SummaryBatchPushOutcome::Enqueued),
        }
        state.total_len += 1;
        self.notify.notify_one();
        Ok(SummaryBatchPushOutcome::Enqueued)
    }

    pub(crate) async fn take(&self) -> Vec<SummaryBatchEvent> {
        let mut state = self.state.lock().await;
        if state.session_events.is_empty() {
            state.total_len = 0;
            return Vec::new();
        }
        let mut events = Vec::with_capacity(state.total_len);
        for (_, event) in state.session_events.drain() {
            events.push(event);
        }
        state.total_len = 0;
        events
    }

    pub(crate) async fn clear(&self) {
        let mut state = self.state.lock().await;
        state.session_events.clear();
        state.total_len = 0;
    }

    pub(crate) async fn drop_session_events_at_or_before<F>(
        &self,
        session_id: SessionId,
        cursor: SessionReplayCursor,
        is_delta_after_cursor: F,
    ) where
        F: Fn(&SessionSummaryDelta, SessionReplayCursor) -> bool,
    {
        let mut state = self.state.lock().await;
        let Some(queued) = state.session_events.get(&session_id) else {
            return;
        };
        let WorkspaceActiveSnapshotEvent::SessionSummaryDelta { delta, .. } = &queued.event else {
            return;
        };
        if is_delta_after_cursor(delta, cursor) {
            return;
        }
        state.session_events.remove(&session_id);
        state.total_len = state.total_len.saturating_sub(1);
    }

    pub(crate) async fn is_empty(&self) -> bool {
        let state = self.state.lock().await;
        state.session_events.is_empty()
    }

    pub(crate) fn notify(&self) -> &Notify {
        &self.notify
    }

    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }
}
