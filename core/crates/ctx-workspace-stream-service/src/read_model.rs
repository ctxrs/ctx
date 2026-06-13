use ctx_core::models::{WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot};

#[derive(Clone, Debug)]
pub struct WorkspaceStreamSnapshotReadModel {
    pub active_snapshot: WorkspaceActiveSnapshot,
    pub active_heads: WorkspaceActiveHeadBatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceStreamInitialState {
    pub snapshot_rev: i64,
    pub archived_rev: i64,
}
