use std::sync::Arc;

use ctx_core::models::WorkspaceActiveSnapshotStreamMessage;

use super::super::super::queue::{
    take_next_workspace_stream_item, workspace_stream_is_idle, HeadBatchBuffer,
    NextWorkspaceStreamItem, StreamQueue, SummaryBatchBuffer,
};
use super::super::super::workspace_stream::WorkspaceStreamRuntime;
use super::super::{StreamSendControl, HEAD_BATCH_FLUSH_INTERVAL};

pub(in crate::api::ws) struct WorkspaceStreamSendRuntime {
    priority_control: Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    control: Arc<StreamQueue<WorkspaceActiveSnapshotStreamMessage>>,
    foreground_head_buffer: Arc<HeadBatchBuffer>,
    background_head_buffer: Arc<HeadBatchBuffer>,
    summary_buffer: Arc<SummaryBatchBuffer>,
    send_control: Arc<StreamSendControl>,
    latest_snapshot_rev: Arc<std::sync::atomic::AtomicI64>,
}

impl WorkspaceStreamSendRuntime {
    pub(in crate::api::ws) fn new(runtime: &WorkspaceStreamRuntime) -> Self {
        Self {
            priority_control: runtime.priority_control.clone(),
            control: runtime.control.clone(),
            foreground_head_buffer: runtime.foreground_head_buffer.clone(),
            background_head_buffer: runtime.background_head_buffer.clone(),
            summary_buffer: runtime.summary_buffer.clone(),
            send_control: runtime.send_control.clone(),
            latest_snapshot_rev: runtime.latest_snapshot_rev.clone(),
        }
    }

    pub(in crate::api::ws) fn flush_tick() -> tokio::time::Interval {
        let mut tick = tokio::time::interval(HEAD_BATCH_FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick
    }

    pub(in crate::api::ws) async fn take_next(&self) -> Option<NextWorkspaceStreamItem> {
        take_next_workspace_stream_item(
            &self.priority_control,
            &self.control,
            &self.foreground_head_buffer,
            &self.background_head_buffer,
            &self.summary_buffer,
            self.send_control.is_hydrating(),
        )
        .await
    }

    async fn priority_work_available(&self) -> bool {
        !self.priority_control.is_empty().await
            || !self.foreground_head_buffer.is_empty().await
            || !self.control.is_empty().await
    }

    pub(in crate::api::ws) fn clear_hydrating(&self) {
        self.send_control.clear_hydrating();
    }

    pub(in crate::api::ws) async fn should_disconnect_after_flush(&self) -> bool {
        self.send_control.should_disconnect_after_flush()
            && workspace_stream_is_idle(
                &self.priority_control,
                &self.control,
                &self.foreground_head_buffer,
                &self.background_head_buffer,
                &self.summary_buffer,
            )
            .await
    }

    pub(in crate::api::ws) fn disconnect_requested(&self) -> bool {
        self.send_control.should_disconnect_after_flush()
    }

    pub(in crate::api::ws) async fn wait_for_next_signal(&self, tick: &mut tokio::time::Interval) {
        tokio::select! {
            _ = self.priority_control.notify().notified() => {},
            _ = self.control.notify().notified() => {},
            _ = self.foreground_head_buffer.notify().notified() => {},
            _ = self.background_head_buffer.notify().notified() => {},
            _ = self.summary_buffer.notify().notified() => {},
            _ = tick.tick() => {},
        }
    }

    pub(in crate::api::ws) async fn wait_after_background_batch(&self) {
        if self.priority_work_available().await {
            return;
        }
        let delay = tokio::time::sleep(HEAD_BATCH_FLUSH_INTERVAL);
        tokio::pin!(delay);
        tokio::select! {
            _ = self.priority_control.notify().notified() => {},
            _ = self.control.notify().notified() => {},
            _ = self.foreground_head_buffer.notify().notified() => {},
            _ = &mut delay => {},
        }
    }

    pub(super) fn latest_snapshot_rev(&self) -> &std::sync::atomic::AtomicI64 {
        &self.latest_snapshot_rev
    }
}
