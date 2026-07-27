use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    utc_now, AgentType, CaptureProvider, Confidence, EventRole, EventType, Fidelity,
    FileChangeKind, ProviderCaptureEnvelope, SessionStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{default_machine_id, ProviderImportSummary, Result};

use crate::common::json::default_metadata;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFixtureLine {
    pub provider: CaptureProvider,
    pub session: ProviderSessionDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<ProviderEventDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSessionDto {
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEventDto {
    pub provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "default_metadata")]
    pub payload: Value,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub source_root: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
}

impl ProviderAdapterContext {
    pub(crate) fn source_root_display(&self) -> Option<String> {
        self.source_root
            .as_ref()
            .or(self.source_path.as_ref())
            .map(|path| path.display().to_string())
    }
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            source_root: None,
            imported_at: utc_now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NormalizedProviderImportOptions {
    pub history_record_id: Option<Uuid>,
    pub persist_cursors: bool,
    pub wrap_transaction: bool,
    pub fast_event_inserts: bool,
    pub capture_work_limit: CaptureWorkLimit,
    pub inventory_observation_token: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureWorkLimit {
    #[default]
    Drain,
    OneSafeGroup,
}

impl Default for NormalizedProviderImportOptions {
    fn default() -> Self {
        Self {
            history_record_id: None,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
            capture_work_limit: CaptureWorkLimit::Drain,
            inventory_observation_token: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderNormalizationResult {
    pub summary: ProviderImportSummary,
    pub captures: Vec<(usize, ProviderCaptureEnvelope)>,
    pub files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFileTouchedEnvelope {
    pub provider: CaptureProvider,
    pub provider_session_id: String,
    pub provider_touch_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count_delta: Option<i64>,
    #[serde(default)]
    pub confidence: Confidence,
    pub occurred_at: DateTime<Utc>,
    pub source_format: String,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

pub trait ProviderCaptureAdapter {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &str;
    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult>;
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureJsonlAdapter {
    pub expected_provider: Option<CaptureProvider>,
    pub source_format: String,
    pub fidelity: Fidelity,
}
