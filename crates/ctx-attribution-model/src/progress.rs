//! Advisory progress; the native writer lock alone establishes whether work is active.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationPhase {
    SnapshotUnavailable,
    WaitingForWriter,
    #[default]
    Preparing,
    Indexing,
    Publishing,
    Complete,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationProgress {
    pub phase: MaterializationPhase,
    pub core_generation_id: Option<String>,
    pub completed_sources: Option<u32>,
    pub total_sources: Option<u32>,
    /// Accepted additions, replacements and deletions, excluding unchanged records.
    pub applied_changes: Option<u64>,
    pub elapsed_millis: u64,
}
