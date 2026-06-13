use super::bounded::StreamQueue;
use crate::api::ws::queue::buffers::{
    HeadBatchBuffer, HeadBatchLane, NextWorkspaceStreamItem, SummaryBatchBuffer,
    BACKGROUND_HEAD_BATCH_CHUNK_LIMIT,
};
use ctx_core::models::WorkspaceActiveSnapshotStreamMessage;

pub(crate) async fn take_next_workspace_stream_item(
    priority_control: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    control: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    foreground_head_buffer: &HeadBatchBuffer,
    background_head_buffer: &HeadBatchBuffer,
    summary_buffer: &SummaryBatchBuffer,
    hydrating: bool,
) -> Option<NextWorkspaceStreamItem> {
    if hydrating {
        if let Some(entry) = control.pop().await {
            return Some(NextWorkspaceStreamItem::Control(entry));
        }
        return None;
    }
    if let Some(entry) = priority_control.pop().await {
        return Some(NextWorkspaceStreamItem::Control(entry));
    }
    let foreground = foreground_head_buffer.take_with_meta().await;
    if !foreground.deltas.is_empty() {
        return Some(NextWorkspaceStreamItem::HeadsBatch {
            lane: HeadBatchLane::Foreground,
            snapshot_rev: foreground.snapshot_rev,
            deltas: foreground.deltas,
            oldest_queued_ms: foreground.oldest_queued_ms,
            stream_source: foreground.stream_source,
        });
    }
    if let Some(entry) = control.pop().await {
        return Some(NextWorkspaceStreamItem::Control(entry));
    }
    let background = background_head_buffer
        .take_chunk_with_meta(BACKGROUND_HEAD_BATCH_CHUNK_LIMIT)
        .await;
    if !background.deltas.is_empty() {
        return Some(NextWorkspaceStreamItem::HeadsBatch {
            lane: HeadBatchLane::Background,
            snapshot_rev: background.snapshot_rev,
            deltas: background.deltas,
            oldest_queued_ms: background.oldest_queued_ms,
            stream_source: background.stream_source,
        });
    }
    let events = summary_buffer.take().await;
    if !events.is_empty() {
        return Some(NextWorkspaceStreamItem::SummaryBatch { events });
    }
    None
}

pub(crate) async fn workspace_stream_is_idle(
    priority_control: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    control: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    foreground_head_buffer: &HeadBatchBuffer,
    background_head_buffer: &HeadBatchBuffer,
    summary_buffer: &SummaryBatchBuffer,
) -> bool {
    priority_control.is_empty().await
        && control.is_empty().await
        && foreground_head_buffer.is_empty().await
        && background_head_buffer.is_empty().await
        && summary_buffer.is_empty().await
}
