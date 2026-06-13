use serde::Serialize;

use crate::WorkspaceActiveSnapshotHub;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceActiveSnapshotStats {
    pub workspace_count: usize,
    pub active_task_count: usize,
    pub active_head_count: usize,
    pub session_replay_sessions: usize,
    pub session_replay_events: usize,
    pub session_replay_event_bytes: usize,
    pub session_replay_event_max_bytes: usize,
    pub workspace_stream_buffer_total: usize,
    pub workspace_stream_buffer_max: usize,
    pub workspace_stream_receivers_total: usize,
    pub workspace_stream_receivers_max: usize,
    pub session_heads_count: usize,
    pub session_heads_bytes: usize,
    pub session_heads_max_bytes: usize,
    pub active_head_index_count: usize,
    pub active_head_bytes: usize,
    pub active_head_max_bytes: usize,
}

impl WorkspaceActiveSnapshotHub {
    pub async fn stats(&self) -> WorkspaceActiveSnapshotStats {
        let inner = self.inner.lock().await;
        let mut active_task_count = 0;
        let mut active_head_count = 0;
        let mut session_replay_sessions = 0;
        let mut session_replay_events = 0;
        let mut session_replay_event_bytes = 0;
        let mut session_replay_event_max_bytes = 0;
        let mut workspace_stream_buffer_total = 0;
        let mut workspace_stream_buffer_max = 0;
        let mut workspace_stream_receivers_total = 0;
        let mut workspace_stream_receivers_max = 0;
        let mut active_head_bytes = 0;
        let mut active_head_max_bytes = 0;
        for entry in inner.values() {
            active_task_count += entry.active_tasks.len();
            active_head_count += entry.active_heads.len();
            session_replay_sessions += entry.session_replay.len();
            session_replay_events += entry
                .session_replay
                .values()
                .map(|state| state.event_count())
                .sum::<usize>();
            for replay in entry.session_replay.values() {
                for delta in replay.events() {
                    let bytes = serde_json::to_vec(delta).map(|buf| buf.len()).unwrap_or(0);
                    session_replay_event_bytes += bytes;
                    if bytes > session_replay_event_max_bytes {
                        session_replay_event_max_bytes = bytes;
                    }
                }
            }
            let buffer_len = entry.tx.len();
            workspace_stream_buffer_total += buffer_len;
            if buffer_len > workspace_stream_buffer_max {
                workspace_stream_buffer_max = buffer_len;
            }
            let receivers = entry.tx.receiver_count();
            workspace_stream_receivers_total += receivers;
            if receivers > workspace_stream_receivers_max {
                workspace_stream_receivers_max = receivers;
            }
            for head in entry.active_heads.values() {
                let bytes = serde_json::to_vec(head).map(|buf| buf.len()).unwrap_or(0);
                active_head_bytes += bytes;
                if bytes > active_head_max_bytes {
                    active_head_max_bytes = bytes;
                }
            }
        }
        let workspace_count = inner.len();
        drop(inner);

        let session_heads = self.session_heads.lock().await;
        let session_heads_count = session_heads.len();
        let mut session_heads_bytes = 0;
        let mut session_heads_max_bytes = 0;
        for cached in session_heads.values() {
            let bytes = serde_json::to_vec(&cached.head)
                .map(|buf| buf.len())
                .unwrap_or(0);
            session_heads_bytes += bytes;
            if bytes > session_heads_max_bytes {
                session_heads_max_bytes = bytes;
            }
        }
        drop(session_heads);
        let active_head_index_count = self.active_head_index.lock().await.len();

        WorkspaceActiveSnapshotStats {
            workspace_count,
            active_task_count,
            active_head_count,
            session_replay_sessions,
            session_replay_events,
            session_replay_event_bytes,
            session_replay_event_max_bytes,
            workspace_stream_buffer_total,
            workspace_stream_buffer_max,
            workspace_stream_receivers_total,
            workspace_stream_receivers_max,
            session_heads_count,
            session_heads_bytes,
            session_heads_max_bytes,
            active_head_index_count,
            active_head_bytes,
            active_head_max_bytes,
        }
    }
}
