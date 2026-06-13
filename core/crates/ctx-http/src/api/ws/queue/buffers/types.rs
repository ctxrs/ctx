use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeadBatchLane {
    Foreground,
    Background,
}

impl HeadBatchLane {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}

pub(crate) enum NextWorkspaceStreamItem {
    Control(StreamQueueEntry<WorkspaceActiveSnapshotStreamMessage>),
    HeadsBatch {
        lane: HeadBatchLane,
        snapshot_rev: i64,
        deltas: Vec<SessionHeadDelta>,
        oldest_queued_ms: u128,
        stream_source: WorkspaceActiveSnapshotStreamSource,
    },
    SummaryBatch {
        events: Vec<SummaryBatchEvent>,
    },
}

pub(crate) struct SummaryBatchEvent {
    pub(crate) event: WorkspaceActiveSnapshotEvent,
    pub(crate) stream_source: WorkspaceActiveSnapshotStreamSource,
}

#[derive(Debug)]
pub(crate) enum HeadBatchPushError {
    SessionLimit { session_id: SessionId, limit: usize },
    TotalLimit { limit: usize },
}

#[derive(Debug)]
pub(crate) enum SummaryBatchPushError {
    TotalLimit { limit: usize },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SummaryBatchPushOutcome {
    Enqueued,
    Replaced,
}
