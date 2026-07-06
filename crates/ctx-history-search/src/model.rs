use std::collections::BTreeMap;

use chrono::Utc;
use ctx_history_core::{
    Artifact, ContextCitation, Event, FileTouched, HistoryRecord, Run, Session, Summary, VcsChange,
};
use uuid::Uuid;

pub(crate) struct Candidate {
    pub(crate) record: HistoryRecord,
    pub(crate) context: RecordContext,
    pub(crate) score: f32,
    pub(crate) why_matched: Vec<String>,
    pub(crate) citations: Vec<ContextCitation>,
    pub(crate) primary_hit: Option<HitMetadata>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RecordContext {
    pub(crate) sessions: Vec<Session>,
    pub(crate) runs: Vec<Run>,
    pub(crate) events: Vec<Event>,
    pub(crate) artifacts: Vec<Artifact>,
    pub(crate) files_touched: Vec<FileTouched>,
    pub(crate) vcs_changes: Vec<VcsChange>,
    pub(crate) summaries: Vec<Summary>,
    pub(crate) sources: BTreeMap<Uuid, ctx_history_core::CaptureSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchSection {
    pub(crate) reason: &'static str,
    pub(crate) weight: f32,
    pub(crate) text: String,
    pub(crate) citation: ContextCitation,
    pub(crate) hit: HitMetadata,
}

#[derive(Debug, Clone)]
pub(crate) struct HitMetadata {
    pub(crate) time: chrono::DateTime<Utc>,
    pub(crate) provider: Option<ctx_history_core::CaptureProvider>,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) history_source: Option<String>,
    pub(crate) history_source_plugin: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) parent_session_id: Option<Uuid>,
    pub(crate) root_session_id: Option<Uuid>,
    pub(crate) event_id: Option<Uuid>,
    pub(crate) event_seq: Option<u64>,
    pub(crate) cwd: Option<String>,
    pub(crate) raw_source_path: Option<String>,
    pub(crate) raw_source_exists: Option<bool>,
    pub(crate) cursor: Option<String>,
}

pub(crate) struct CandidateSearch {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) scan_budget_exhausted: bool,
}
