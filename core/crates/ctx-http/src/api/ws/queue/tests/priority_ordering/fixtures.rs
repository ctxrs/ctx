use super::*;

pub(super) struct PriorityOrderingFixture {
    pub(super) priority_control: StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    pub(super) control: StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    pub(super) foreground_head_buffer: HeadBatchBuffer,
    pub(super) background_head_buffer: HeadBatchBuffer,
    summary_buffer: SummaryBatchBuffer,
}

impl PriorityOrderingFixture {
    pub(super) fn new() -> Self {
        Self {
            priority_control: StreamQueue::new(8, Duration::from_secs(1)),
            control: StreamQueue::new(8, Duration::from_secs(1)),
            foreground_head_buffer: HeadBatchBuffer::new(),
            background_head_buffer: HeadBatchBuffer::new(),
            summary_buffer: SummaryBatchBuffer::new(8),
        }
    }

    pub(super) async fn next(&self) -> Option<NextWorkspaceStreamItem> {
        take_next_workspace_stream_item(
            &self.priority_control,
            &self.control,
            &self.foreground_head_buffer,
            &self.background_head_buffer,
            &self.summary_buffer,
            false,
        )
        .await
    }
}

pub(super) async fn push_control_gap(
    queue: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    source: &'static str,
    snapshot_rev: i64,
    after_seq: i64,
    reason: &'static str,
) -> Result<(), ()> {
    push_stream_message(
        queue,
        workspace_id,
        Some(session_id),
        source,
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev: 0,
            event: Box::new(WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id,
                snapshot_rev,
                session_id,
                after_seq,
                reason: Some(reason.to_string()),
                seed_follows: false,
            }),
            stream_source: None,
        },
    )
    .await
}

pub(super) fn assert_control_session_gap(
    item: Option<NextWorkspaceStreamItem>,
    expected_session_id: SessionId,
    context: &'static str,
) {
    let Some(NextWorkspaceStreamItem::Control(entry)) = item else {
        panic!("{context}");
    };
    let (_, message) = entry.into_parts();
    assert!(
        matches!(
            message,
            WorkspaceActiveSnapshotStreamMessage::Event { event, .. }
                if matches!(
                    event.as_ref(),
                    WorkspaceActiveSnapshotEvent::SessionGap { session_id, .. }
                        if *session_id == expected_session_id
                )
        ),
        "{context}"
    );
}

pub(super) fn expect_heads_batch(
    item: Option<NextWorkspaceStreamItem>,
    expected_lane: HeadBatchLane,
    context: &'static str,
) -> Vec<SessionHeadDelta> {
    let Some(NextWorkspaceStreamItem::HeadsBatch { lane, deltas, .. }) = item else {
        panic!("{context}");
    };
    assert_eq!(lane, expected_lane, "{context}");
    deltas
}
