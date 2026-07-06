use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    dtos::{
        Artifact, Event, FileTouched, HistoryRecord, HistoryRecordLink, Run, Session, Summary,
        VcsChange, VcsWorkspace,
    },
    source::{CaptureSource, CaptureSourceDescriptor},
    sync::{default_metadata, Fidelity},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHistoryArchive {
    #[serde(default = "legacy_archive_schema_version")]
    pub schema_version: u32,
    #[serde(default = "legacy_archive_schema_version")]
    pub version: u32,
    pub records: Vec<HistoryRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_sources: Vec<CaptureSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<Run>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_records: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcs_workspaces: Vec<VcsWorkspace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcs_changes: Vec<VcsChange>,
    #[serde(
        default,
        rename = "history_record_links",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub history_record_links: Vec<HistoryRecordLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<Summary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<FileTouched>,
}

impl Default for SessionHistoryArchive {
    fn default() -> Self {
        Self {
            schema_version: archive_schema_version(),
            version: archive_schema_version(),
            records: Vec::new(),
            capture_sources: Vec::new(),
            sessions: Vec::new(),
            runs: Vec::new(),
            events: Vec::new(),
            artifact_records: Vec::new(),
            vcs_workspaces: Vec::new(),
            vcs_changes: Vec::new(),
            history_record_links: Vec::new(),
            summaries: Vec::new(),
            files_touched: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEnvelope {
    pub schema_version: u32,
    pub capture_event_id: Uuid,
    pub dedupe_key: String,
    pub source: CaptureSourceDescriptor,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_metadata")]
    pub env_session_hints: serde_json::Value,
    #[serde(default = "default_metadata")]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default)]
    pub fidelity: Fidelity,
}

fn archive_schema_version() -> u32 {
    2
}

fn legacy_archive_schema_version() -> u32 {
    1
}
