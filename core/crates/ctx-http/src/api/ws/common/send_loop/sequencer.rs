use ctx_core::models::{
    SessionHeadDelta, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceActiveSnapshotEvent, WorkspaceActiveSnapshotStreamMessage,
    WorkspaceActiveSnapshotStreamSource,
};
use std::sync::atomic::{AtomicI64, Ordering};

use super::super::super::replay::with_stream_rev;
use super::super::bump_latest_snapshot_rev;
use super::runtime::WorkspaceStreamSendRuntime;

pub(in crate::api::ws) struct SequencedControlMessage {
    pub(in crate::api::ws) message: WorkspaceActiveSnapshotStreamMessage,
    pub(in crate::api::ws) is_snapshot: bool,
}

#[derive(Default)]
pub(in crate::api::ws) struct WorkspaceStreamSequencer {
    stream_seq: i64,
}

impl WorkspaceStreamSequencer {
    fn serialized_source(
        stream_source: WorkspaceActiveSnapshotStreamSource,
    ) -> Option<WorkspaceActiveSnapshotStreamSource> {
        match stream_source {
            WorkspaceActiveSnapshotStreamSource::Live => None,
            WorkspaceActiveSnapshotStreamSource::Replay => Some(stream_source),
        }
    }

    pub(in crate::api::ws) fn sequence_control_message(
        &mut self,
        runtime: &WorkspaceStreamSendRuntime,
        message: WorkspaceActiveSnapshotStreamMessage,
    ) -> SequencedControlMessage {
        let is_snapshot = matches!(
            message,
            WorkspaceActiveSnapshotStreamMessage::Snapshot { .. }
        );
        let message = match message {
            WorkspaceActiveSnapshotStreamMessage::ResetRequired { .. } => message,
            WorkspaceActiveSnapshotStreamMessage::Snapshot {
                active_snapshot,
                active_heads,
                ..
            } => {
                self.stream_seq += 1;
                WorkspaceActiveSnapshotStreamMessage::Snapshot {
                    rev: self.stream_seq,
                    active_snapshot: clamp_snapshot_payload_rev(
                        runtime.latest_snapshot_rev(),
                        active_snapshot,
                        active_heads.as_ref(),
                    ),
                    active_heads: active_heads.map(|heads| {
                        clamp_head_batch_payload_rev(runtime.latest_snapshot_rev(), heads)
                    }),
                }
            }
            other => {
                self.stream_seq += 1;
                with_stream_rev(
                    clamp_stream_message_snapshot_rev(runtime.latest_snapshot_rev(), other),
                    self.stream_seq,
                )
            }
        };
        SequencedControlMessage {
            message,
            is_snapshot,
        }
    }

    pub(in crate::api::ws) fn sequence_heads_batch(
        &mut self,
        runtime: &WorkspaceStreamSendRuntime,
        snapshot_rev: i64,
        deltas: Vec<SessionHeadDelta>,
        stream_source: WorkspaceActiveSnapshotStreamSource,
    ) -> WorkspaceActiveSnapshotStreamMessage {
        let latest_rev = runtime
            .latest_snapshot_rev()
            .load(std::sync::atomic::Ordering::Relaxed);
        let snapshot_rev = snapshot_rev.max(latest_rev);
        bump_latest_snapshot_rev(runtime.latest_snapshot_rev(), snapshot_rev);
        self.stream_seq += 1;
        WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            rev: self.stream_seq,
            snapshot_rev,
            deltas,
            stream_source: Self::serialized_source(stream_source),
        }
    }

    pub(in crate::api::ws) fn sequence_summary_event(
        &mut self,
        runtime: &WorkspaceStreamSendRuntime,
        event: WorkspaceActiveSnapshotEvent,
        stream_source: WorkspaceActiveSnapshotStreamSource,
    ) -> WorkspaceActiveSnapshotStreamMessage {
        self.stream_seq += 1;
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev: self.stream_seq,
            event: Box::new(clamp_event_snapshot_rev(
                runtime.latest_snapshot_rev(),
                event,
            )),
            stream_source: Self::serialized_source(stream_source),
        }
    }
}

fn clamp_snapshot_rev(latest: &AtomicI64, snapshot_rev: i64) -> i64 {
    let next = snapshot_rev.max(latest.load(Ordering::Relaxed));
    bump_latest_snapshot_rev(latest, next);
    next
}

fn clamp_snapshot_payload_rev(
    latest: &AtomicI64,
    mut active_snapshot: WorkspaceActiveSnapshot,
    active_heads: Option<&WorkspaceActiveHeadBatch>,
) -> WorkspaceActiveSnapshot {
    let source_rev = active_heads
        .map(|heads| active_snapshot.snapshot_rev.max(heads.snapshot_rev))
        .unwrap_or(active_snapshot.snapshot_rev);
    active_snapshot.snapshot_rev = clamp_snapshot_rev(latest, source_rev);
    active_snapshot
}

fn clamp_head_batch_payload_rev(
    latest: &AtomicI64,
    mut active_heads: WorkspaceActiveHeadBatch,
) -> WorkspaceActiveHeadBatch {
    active_heads.snapshot_rev = clamp_snapshot_rev(latest, active_heads.snapshot_rev);
    active_heads
}

fn clamp_stream_message_snapshot_rev(
    latest: &AtomicI64,
    message: WorkspaceActiveSnapshotStreamMessage,
) -> WorkspaceActiveSnapshotStreamMessage {
    match message {
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev,
            event,
            stream_source,
        } => WorkspaceActiveSnapshotStreamMessage::Event {
            rev,
            event: Box::new(clamp_event_snapshot_rev(latest, *event)),
            stream_source,
        },
        WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            rev,
            snapshot_rev,
            deltas,
            stream_source,
        } => WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            rev,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            deltas,
            stream_source,
        },
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev,
            active_snapshot,
            active_heads,
        } => WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev,
            active_snapshot: clamp_snapshot_payload_rev(
                latest,
                active_snapshot,
                active_heads.as_ref(),
            ),
            active_heads: active_heads.map(|heads| clamp_head_batch_payload_rev(latest, heads)),
        },
        WorkspaceActiveSnapshotStreamMessage::ResetRequired { latest_rev } => {
            WorkspaceActiveSnapshotStreamMessage::ResetRequired { latest_rev }
        }
    }
}

fn clamp_event_snapshot_rev(
    latest: &AtomicI64,
    event: WorkspaceActiveSnapshotEvent,
) -> WorkspaceActiveSnapshotEvent {
    match event {
        WorkspaceActiveSnapshotEvent::Ready {
            workspace_id,
            snapshot_rev,
            archived_rev,
        } => WorkspaceActiveSnapshotEvent::Ready {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            archived_rev,
        },
        WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev,
            task,
        } => WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            task,
        },
        WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev,
            task_id,
        } => WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            task_id,
        },
        WorkspaceActiveSnapshotEvent::TaskDelta {
            workspace_id,
            snapshot_rev,
            delta,
        } => WorkspaceActiveSnapshotEvent::TaskDelta {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            delta,
        },
        WorkspaceActiveSnapshotEvent::SessionSummary {
            workspace_id,
            snapshot_rev,
            summary,
        } => WorkspaceActiveSnapshotEvent::SessionSummary {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            summary,
        },
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id,
            snapshot_rev,
            delta,
        } => WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            delta,
        },
        WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev,
            session_id,
        } => WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            session_id,
        },
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev,
            delta,
        } => WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            delta,
        },
        WorkspaceActiveSnapshotEvent::SessionHeadSeed {
            workspace_id,
            snapshot_rev,
            head,
        } => WorkspaceActiveSnapshotEvent::SessionHeadSeed {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            head,
        },
        WorkspaceActiveSnapshotEvent::SessionGap {
            workspace_id,
            snapshot_rev,
            session_id,
            after_seq,
            reason,
            seed_follows,
        } => WorkspaceActiveSnapshotEvent::SessionGap {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            session_id,
            after_seq,
            reason,
            seed_follows,
        },
        WorkspaceActiveSnapshotEvent::WorktreeBootstrap {
            workspace_id,
            snapshot_rev,
            notice,
        } => WorkspaceActiveSnapshotEvent::WorktreeBootstrap {
            workspace_id,
            snapshot_rev: clamp_snapshot_rev(latest, snapshot_rev),
            notice,
        },
        WorkspaceActiveSnapshotEvent::ArchivedTaskUpsert {
            workspace_id,
            archived_rev,
            task,
        } => WorkspaceActiveSnapshotEvent::ArchivedTaskUpsert {
            workspace_id,
            archived_rev,
            task,
        },
        WorkspaceActiveSnapshotEvent::ArchivedTaskDelete {
            workspace_id,
            archived_rev,
            task_id,
        } => WorkspaceActiveSnapshotEvent::ArchivedTaskDelete {
            workspace_id,
            archived_rev,
            task_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
    use ctx_core::models::{SessionSummaryDelta, WorkspaceActivePage};

    #[test]
    fn snapshot_payload_advances_connection_snapshot_floor() {
        let latest = AtomicI64::new(10);
        let workspace_id = WorkspaceId::new();
        let snapshot = WorkspaceActiveSnapshot {
            workspace_id,
            snapshot_rev: 20,
            archived_rev: 0,
            active: WorkspaceActivePage {
                tasks: Vec::new(),
                total_count: 0,
            },
        };
        let heads = WorkspaceActiveHeadBatch {
            workspace_id,
            snapshot_rev: 18,
            heads: Vec::new(),
        };

        let snapshot = clamp_snapshot_payload_rev(&latest, snapshot, Some(&heads));
        let heads = clamp_head_batch_payload_rev(&latest, heads);

        assert_eq!(snapshot.snapshot_rev, 20);
        assert_eq!(heads.snapshot_rev, 20);
        assert_eq!(latest.load(Ordering::Relaxed), 20);
    }

    #[test]
    fn lower_summary_event_is_clamped_to_connection_snapshot_floor() {
        let latest = AtomicI64::new(100);
        let event = WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id: WorkspaceId::new(),
            snapshot_rev: 90,
            delta: Box::new(SessionSummaryDelta {
                session_id: SessionId::new(),
                task_id: TaskId::new(),
                activity: None,
                last_message_at: None,
                last_message_preview: None,
                last_event_seq: Some(1),
                projection_rev: Some(1),
                state_rev: Some(1),
                emitted_at_ms: None,
            }),
        };

        let clamped = clamp_event_snapshot_rev(&latest, event);

        assert!(matches!(
            clamped,
            WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
                snapshot_rev: 100,
                ..
            }
        ));
        assert_eq!(latest.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn higher_ready_event_advances_connection_snapshot_floor() {
        let latest = AtomicI64::new(100);
        let event = WorkspaceActiveSnapshotEvent::Ready {
            workspace_id: WorkspaceId::new(),
            snapshot_rev: 125,
            archived_rev: 7,
        };

        let clamped = clamp_event_snapshot_rev(&latest, event);

        assert!(matches!(
            clamped,
            WorkspaceActiveSnapshotEvent::Ready {
                snapshot_rev: 125,
                archived_rev: 7,
                ..
            }
        ));
        assert_eq!(latest.load(Ordering::Relaxed), 125);
    }
}
