use std::collections::{HashMap, VecDeque};

use tokio::sync::{Mutex, Notify};

use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct VcsSnapshotKey {
    pub(super) worktree_id: WorktreeId,
    pub(super) tier: WorktreeVcsStreamTier,
}

#[derive(Default)]
struct VcsPendingState {
    controls: VecDeque<WorktreeVcsStreamMessage>,
    snapshots: HashMap<VcsSnapshotKey, WorktreeVcsStreamMessage>,
}

pub(super) struct VcsPendingBuffer {
    state: Mutex<VcsPendingState>,
    notify: Notify,
}

impl VcsPendingBuffer {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(VcsPendingState::default()),
            notify: Notify::new(),
        }
    }

    pub(super) async fn push_control(&self, message: WorktreeVcsStreamMessage) {
        let mut state = self.state.lock().await;
        state.controls.push_back(message);
        self.notify.notify_one();
    }

    pub(super) async fn push_snapshot(
        &self,
        key: VcsSnapshotKey,
        message: WorktreeVcsStreamMessage,
    ) -> bool {
        let mut state = self.state.lock().await;
        let coalesced = state.snapshots.insert(key, message).is_some();
        self.notify.notify_one();
        coalesced
    }

    pub(super) async fn pop(&self) -> Option<WorktreeVcsStreamMessage> {
        let mut state = self.state.lock().await;
        if let Some(message) = state.controls.pop_front() {
            return Some(message);
        }
        let key = state.snapshots.keys().next().copied()?;
        state.snapshots.remove(&key)
    }

    pub(super) async fn wait_for_message(&self) {
        self.notify.notified().await;
    }

    #[cfg(test)]
    pub(super) async fn is_empty(&self) -> bool {
        let state = self.state.lock().await;
        state.controls.is_empty() && state.snapshots.is_empty()
    }
}

pub(super) fn vcs_stream_message_is_snapshot(message: &WorktreeVcsStreamMessage) -> bool {
    matches!(
        message,
        WorktreeVcsStreamMessage::SummarySnapshot { .. }
            | WorktreeVcsStreamMessage::DetailsSnapshot { .. }
            | WorktreeVcsStreamMessage::UnavailableSnapshot { .. }
    )
}
