use anyhow::Result;
use ctx_history_relational::{RelationalProjectionMetadata, RelationalProjectionReceipt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compact_json;

use super::{status_name, SourceBackedRelationalCatchUpError};

const SOURCE_BACKED_RELATIONAL_STATUS_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceBackedRelationalCatchUpState {
    Pending,
    Error,
    Completed,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct SourceBackedRelationalCatchUpStatus {
    schema_version: u16,
    owner: String,
    kind: String,
    status: SourceBackedRelationalCatchUpState,
    pending: bool,
    retryable: bool,
    pub(super) core_generation_id: String,
    active_core_generation_id: Option<String>,
    receipt_core_generation_id: Option<String>,
    projection_status: Option<String>,
    build_generation: Option<u64>,
    pub(super) attempts: u64,
    last_attempt_at_ms: i64,
    #[serde(default)]
    last_attempt_duration_us: u64,
    error_code: Option<String>,
    last_error: Option<String>,
}

impl SourceBackedRelationalCatchUpStatus {
    pub(super) fn pending(
        core_generation_id: &str,
        attempts: u64,
        frontier: Option<&RelationalProjectionMetadata>,
    ) -> Self {
        Self {
            schema_version: SOURCE_BACKED_RELATIONAL_STATUS_SCHEMA_VERSION,
            owner: "daemon".to_owned(),
            kind: "source_backed_relational_catch_up".to_owned(),
            status: SourceBackedRelationalCatchUpState::Pending,
            pending: true,
            retryable: true,
            core_generation_id: core_generation_id.to_owned(),
            active_core_generation_id: frontier
                .and_then(|metadata| metadata.active_core_generation_id.clone()),
            receipt_core_generation_id: None,
            projection_status: frontier.map(|metadata| status_name(metadata.status).to_owned()),
            build_generation: frontier.map(|metadata| metadata.build_generation),
            attempts,
            last_attempt_at_ms: ctx_history_core::utc_now().timestamp_millis(),
            last_attempt_duration_us: 0,
            error_code: None,
            last_error: None,
        }
    }

    pub(super) fn error(
        mut self,
        error: SourceBackedRelationalCatchUpError,
        frontier: Option<&RelationalProjectionMetadata>,
    ) -> Self {
        self.status = SourceBackedRelationalCatchUpState::Error;
        self.pending = true;
        self.retryable = true;
        self.active_core_generation_id = frontier
            .and_then(|metadata| metadata.active_core_generation_id.clone())
            .or(self.active_core_generation_id);
        self.projection_status = frontier.map(|metadata| status_name(metadata.status).to_owned());
        self.build_generation = frontier.map(|metadata| metadata.build_generation);
        self.error_code = Some(error.code().to_owned());
        self.last_error = Some(error.to_string());
        self
    }

    pub(super) fn completed(mut self, receipt: &RelationalProjectionReceipt) -> Self {
        self.status = SourceBackedRelationalCatchUpState::Completed;
        self.pending = false;
        self.retryable = false;
        self.active_core_generation_id = Some(receipt.core_generation_id.clone());
        self.receipt_core_generation_id = Some(receipt.core_generation_id.clone());
        self.projection_status = Some("ready".to_owned());
        self.build_generation = Some(receipt.build_generation);
        self.error_code = None;
        self.last_error = None;
        self
    }

    pub(super) fn with_duration(mut self, duration_us: u64) -> Self {
        self.last_attempt_duration_us = duration_us;
        self
    }

    pub(super) fn is_completed_for(&self, core_generation_id: &str) -> bool {
        self.status == SourceBackedRelationalCatchUpState::Completed
            && self.core_generation_id == core_generation_id
            && self.active_core_generation_id.as_deref() == Some(core_generation_id)
            && self.receipt_core_generation_id.as_deref() == Some(core_generation_id)
            && self.projection_status.as_deref() == Some("ready")
    }

    pub(super) fn to_json(&self) -> Result<Value> {
        Ok(compact_json(serde_json::to_value(self)?))
    }
}
