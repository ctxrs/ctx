use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use super::buffer::vcs_stream_message_is_snapshot;
use super::*;

#[derive(Default)]
pub(super) struct VcsStreamMetrics {
    pub(super) snapshot_queued_count: AtomicU64,
    pub(super) snapshot_coalesced_count: AtomicU64,
    message_sent_count: AtomicU64,
    snapshot_sent_count: AtomicU64,
    snapshot_queued_recorded_count: AtomicU64,
    snapshot_coalesced_recorded_count: AtomicU64,
    message_sent_recorded_count: AtomicU64,
    snapshot_sent_recorded_count: AtomicU64,
}

impl VcsStreamMetrics {
    pub(super) fn snapshot_queued(&self, coalesced: bool) {
        self.snapshot_queued_count
            .fetch_add(1, AtomicOrdering::Relaxed);
        if coalesced {
            self.snapshot_coalesced_count
                .fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    pub(super) fn message_sent(&self, message: &WorktreeVcsStreamMessage) {
        self.message_sent_count
            .fetch_add(1, AtomicOrdering::Relaxed);
        if vcs_stream_message_is_snapshot(message) {
            self.snapshot_sent_count
                .fetch_add(1, AtomicOrdering::Relaxed);
        }
    }
}

fn counter_delta(counter: &AtomicU64, recorded: &AtomicU64) -> u64 {
    let total = counter.load(AtomicOrdering::Relaxed);
    let previous = recorded.swap(total, AtomicOrdering::Relaxed);
    total.saturating_sub(previous)
}

pub(super) async fn record_workspace_vcs_stream_metrics(
    state: &WorkspaceVcsStreamHandle,
    metrics: &VcsStreamMetrics,
) {
    let counters = [
        (
            "workspace.vcs_stream.server_snapshot_queued_count",
            counter_delta(
                &metrics.snapshot_queued_count,
                &metrics.snapshot_queued_recorded_count,
            ),
        ),
        (
            "workspace.vcs_stream.server_snapshot_coalesced_count",
            counter_delta(
                &metrics.snapshot_coalesced_count,
                &metrics.snapshot_coalesced_recorded_count,
            ),
        ),
        (
            "workspace.vcs_stream.server_message_sent_count",
            counter_delta(
                &metrics.message_sent_count,
                &metrics.message_sent_recorded_count,
            ),
        ),
        (
            "workspace.vcs_stream.server_snapshot_sent_count",
            counter_delta(
                &metrics.snapshot_sent_count,
                &metrics.snapshot_sent_recorded_count,
            ),
        ),
    ];
    for (name, value) in counters {
        if value == 0 {
            continue;
        }
        state.record_workspace_vcs_stream_metric(name, value).await;
    }
}
