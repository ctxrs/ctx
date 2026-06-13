use std::collections::VecDeque;

use ctx_core::ids::SessionId;
use ctx_core::models::{SessionHeadDelta, SessionHeadSnapshot};

pub(super) const SESSION_REPLAY_BUFFER_LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionReplayCursor {
    pub last_event_seq: i64,
    pub projection_rev: i64,
}

pub fn is_transient_session_delta(delta: &SessionHeadDelta) -> bool {
    matches!(
        delta.event.as_ref(),
        Some(event) if event.transient || event.seq < 0
    )
}

impl SessionReplayCursor {
    pub fn cover(self, other: Self) -> Self {
        Self {
            last_event_seq: self.last_event_seq.max(other.last_event_seq),
            projection_rev: self.projection_rev.max(other.projection_rev),
        }
    }

    pub fn from_delta(delta: &SessionHeadDelta) -> Self {
        Self {
            last_event_seq: delta.last_event_seq.max(0),
            projection_rev: delta.projection_rev.max(0),
        }
    }

    pub fn from_head(head: &SessionHeadSnapshot) -> Self {
        Self {
            last_event_seq: head.last_event_seq.max(0),
            projection_rev: head.projection_rev.max(0),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SessionReplayEntry {
    cursor: SessionReplayCursor,
    delta: SessionHeadDelta,
}

#[derive(Debug, Default, Clone)]
pub(super) struct SessionReplayState {
    pub(super) last_cursor: SessionReplayCursor,
    events: VecDeque<SessionReplayEntry>,
}

impl SessionReplayState {
    pub(super) fn record(&mut self, delta: &SessionHeadDelta) {
        let cursor = SessionReplayCursor::from_delta(delta);
        if !is_transient_session_delta(delta) {
            self.events.push_back(SessionReplayEntry {
                cursor,
                delta: delta.clone(),
            });
            while self.events.len() > SESSION_REPLAY_BUFFER_LIMIT {
                self.events.pop_front();
            }
        }
        self.last_cursor = self.last_cursor.max(cursor);
    }

    pub(super) fn seed(&mut self, cursor: SessionReplayCursor) {
        self.last_cursor = self.last_cursor.max(cursor);
    }

    pub(super) fn replay(
        &self,
        after_cursor: SessionReplayCursor,
        limit: usize,
    ) -> SessionReplayResult {
        let after_cursor = SessionReplayCursor {
            last_event_seq: after_cursor.last_event_seq.max(0),
            projection_rev: after_cursor.projection_rev.max(0),
        };
        if self.last_cursor <= after_cursor {
            return SessionReplayResult::Replay {
                deltas: Vec::new(),
                last_sent: after_cursor,
            };
        }
        let Some(oldest_cursor) = self.events.front().map(|entry| entry.cursor) else {
            return SessionReplayResult::Gap {
                last_known_seq: self.last_cursor.last_event_seq,
                reason: Some("missing_replay_events".to_string()),
            };
        };
        if after_cursor < oldest_cursor {
            return SessionReplayResult::Gap {
                last_known_seq: self.last_cursor.last_event_seq,
                reason: Some("replay_buffer_overflow".to_string()),
            };
        }
        let mut deltas = Vec::new();
        for entry in self.events.iter() {
            if entry.cursor > after_cursor {
                deltas.push(entry.delta.clone());
            }
        }
        if deltas.is_empty() && self.last_cursor > after_cursor {
            return SessionReplayResult::Gap {
                last_known_seq: self.last_cursor.last_event_seq,
                reason: Some("replay_gap".to_string()),
            };
        }
        if deltas.len() > limit {
            return SessionReplayResult::Gap {
                last_known_seq: self.last_cursor.last_event_seq,
                reason: Some("replay_limit_exceeded".to_string()),
            };
        }
        let last_sent = deltas
            .last()
            .map(SessionReplayCursor::from_delta)
            .unwrap_or(after_cursor);
        SessionReplayResult::Replay { deltas, last_sent }
    }

    pub(super) fn event_count(&self) -> usize {
        self.events.len()
    }

    pub(super) fn events(&self) -> impl Iterator<Item = &SessionHeadDelta> {
        self.events.iter().map(|entry| &entry.delta)
    }
}

#[derive(Debug)]
pub enum SessionReplayResult {
    Replay {
        deltas: Vec<SessionHeadDelta>,
        last_sent: SessionReplayCursor,
    },
    Gap {
        last_known_seq: i64,
        reason: Option<String>,
    },
    ResetRequired,
}

#[derive(Debug, Clone)]
pub enum WorkspaceSessionReplayItem {
    Delta(Box<SessionHeadDelta>),
    Gap {
        session_id: SessionId,
        after_seq: i64,
        reason: Option<String>,
    },
    Seed(Box<SessionHeadSnapshot>),
}

#[derive(Debug, Clone)]
pub enum WorkspaceSessionReplay {
    Replay {
        items: Vec<WorkspaceSessionReplayItem>,
        last_sent: SessionReplayCursor,
    },
    ResetRequired,
}
