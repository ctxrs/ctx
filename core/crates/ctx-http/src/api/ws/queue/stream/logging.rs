use super::bounded::{StreamQueue, StreamQueuePushError};
use crate::api::ws::queue::buffers::{HeadBatchPushError, SummaryBatchPushError};
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::WorkspaceActiveSnapshotStreamMessage;

fn log_stream_queue_push_error(
    context: &'static str,
    workspace_id: WorkspaceId,
    session_id: Option<SessionId>,
    err: &StreamQueuePushError,
) {
    match err {
        StreamQueuePushError::QueueFull { len, limit } => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                session_id = ?session_id,
                queue_len = *len,
                queue_limit = *limit,
                "workspace stream queue full ({context})",
            );
        }
        StreamQueuePushError::QueueStale { age_ms, max_age_ms } => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                session_id = ?session_id,
                oldest_age_ms = *age_ms,
                max_age_ms = *max_age_ms,
                "workspace stream queue stale ({context})",
            );
        }
    }
}

pub(crate) fn log_head_batch_push_error(
    context: &'static str,
    workspace_id: WorkspaceId,
    err: &HeadBatchPushError,
) {
    match err {
        HeadBatchPushError::SessionLimit { session_id, limit } => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                session_id = %session_id.0,
                limit = *limit,
                "workspace head batch session limit exceeded ({context})",
            );
        }
        HeadBatchPushError::TotalLimit { limit } => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                limit = *limit,
                "workspace head batch total limit exceeded ({context})",
            );
        }
    }
}

pub(crate) fn log_summary_batch_push_error(
    context: &'static str,
    workspace_id: WorkspaceId,
    err: &SummaryBatchPushError,
) {
    match err {
        SummaryBatchPushError::TotalLimit { limit } => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                limit = *limit,
                "workspace summary batch total limit exceeded ({context})",
            );
        }
    }
}

pub(crate) async fn push_stream_message(
    pending: &StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    workspace_id: WorkspaceId,
    session_id: Option<SessionId>,
    context: &'static str,
    message: WorkspaceActiveSnapshotStreamMessage,
) -> Result<(), ()> {
    match pending.push(message).await {
        Ok(()) => Ok(()),
        Err(err) => {
            log_stream_queue_push_error(context, workspace_id, session_id, &err);
            Err(())
        }
    }
}
