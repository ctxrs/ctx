use serde::{Deserialize, Serialize};

/// Persisted per-event Core coverage carried by CTXEVI06.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentCoreCoverage {
    pub repository_candidate_events: u64,
    pub logical_binding_events: u64,
    pub certified_live_root_access_events: u64,
    pub file_evidence_events: u64,
    pub exact_commit_evidence_events: u64,
    pub exact_pull_request_evidence_events: u64,
}

/// Existing bounded Core corpus ceiling encoded by the event-index format.
pub const MAX_SEGMENT_CORE_EVENTS: usize = 4_194_304;
