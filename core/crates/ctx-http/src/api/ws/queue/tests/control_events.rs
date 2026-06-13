use super::*;

#[tokio::test]
async fn hydrating_keeps_snapshot_control_ahead_of_priority_lane() {
    let priority_control = StreamQueue::new(8, Duration::from_secs(1));
    let control = StreamQueue::new(8, Duration::from_secs(1));
    let foreground_head_buffer = HeadBatchBuffer::new();
    let background_head_buffer = HeadBatchBuffer::new();
    let summary_buffer = SummaryBatchBuffer::new(8);
    let workspace_id = WorkspaceId::new();
    let foreground_session_id = SessionId::new();

    push_stream_message(
        &control,
        workspace_id,
        None,
        "test_snapshot",
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev: 0,
            active_snapshot: WorkspaceActiveSnapshot {
                workspace_id,
                snapshot_rev: 1,
                archived_rev: 0,
                active: WorkspaceActivePage {
                    tasks: Vec::new(),
                    total_count: 0,
                },
            },
            active_heads: None,
        },
    )
    .await
    .expect("snapshot should enqueue");
    push_stream_message(
        &priority_control,
        workspace_id,
        Some(foreground_session_id),
        "test_priority",
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev: 0,
            event: Box::new(WorkspaceActiveSnapshotEvent::SessionGap {
                workspace_id,
                snapshot_rev: 2,
                session_id: foreground_session_id,
                after_seq: 1,
                reason: Some("foreground".to_string()),
                seed_follows: false,
            }),
            stream_source: None,
        },
    )
    .await
    .expect("priority event should enqueue");

    let Some(NextWorkspaceStreamItem::Control(entry)) = take_next_workspace_stream_item(
        &priority_control,
        &control,
        &foreground_head_buffer,
        &background_head_buffer,
        &summary_buffer,
        true,
    )
    .await
    else {
        panic!("expected snapshot control while hydrating");
    };
    let (_, message) = entry.into_parts();
    assert!(matches!(
        message,
        WorkspaceActiveSnapshotStreamMessage::Snapshot { .. }
    ));

    let Some(NextWorkspaceStreamItem::Control(entry)) = take_next_workspace_stream_item(
        &priority_control,
        &control,
        &foreground_head_buffer,
        &background_head_buffer,
        &summary_buffer,
        false,
    )
    .await
    else {
        panic!("expected priority event after hydration");
    };
    let (_, message) = entry.into_parts();
    assert!(matches!(
        message,
        WorkspaceActiveSnapshotStreamMessage::Event { event, .. }
            if matches!(
                event.as_ref(),
                WorkspaceActiveSnapshotEvent::SessionGap { session_id, .. }
                    if *session_id == foreground_session_id
            )
    ));
}
