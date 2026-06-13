use ctx_core::ids::WorkspaceId;
use ctx_core::models::{SessionHeadSnapshot, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot};

#[derive(Clone, Debug)]
pub struct WorkspaceActiveSnapshotCacheEntry {
    pub snapshot: WorkspaceActiveSnapshot,
}

#[derive(Clone, Debug)]
pub struct WorkspaceActiveHeadCacheEntry {
    pub batch: WorkspaceActiveHeadBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionHeadCompleteness {
    Hydrated,
    DeltaOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionHeadCapability {
    ReplayCapable,
    CompactOnly,
}

#[derive(Debug, Clone)]
pub(super) struct CachedSessionHead {
    pub workspace_id: WorkspaceId,
    pub head: SessionHeadSnapshot,
    pub completeness: SessionHeadCompleteness,
    pub capability: SessionHeadCapability,
    pub last_touched_at_ms: i64,
}

impl CachedSessionHead {
    pub(super) fn touch(&mut self, at_ms: i64) {
        self.last_touched_at_ms = at_ms;
    }
}
