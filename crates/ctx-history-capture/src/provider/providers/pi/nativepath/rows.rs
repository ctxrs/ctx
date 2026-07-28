use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{Confidence, EventRole, EventType, FileChangeKind};
use serde::Serialize;
use serde_json::Value;

pub(crate) const PI_NATIVE_PAGE_MAX_UNITS: usize = 64;
pub(crate) const PI_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const PI_NATIVE_REJECTION_MAX_CHARS: usize = 1_024;
pub(super) const PI_NATIVE_PAGE_ENCODING_RESERVE: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PiNativeSessionRow {
    pub(crate) provider_session_id: String,
    pub(crate) version: Option<u64>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_session: Option<String>,
    pub(crate) source_metadata: Value,
    pub(crate) session_metadata: Value,
    pub(crate) source_idempotency_key: String,
    pub(crate) session_idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PiNativePhysicalLocator {
    pub(crate) path: PathBuf,
    pub(crate) source_record_ordinal: u64,
    pub(crate) line_number: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) record_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PiNativeEventRow {
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_identity_index: u64,
    pub(crate) cursor: Option<String>,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) idempotency_key: String,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
    pub(crate) locator: PiNativePhysicalLocator,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PiNativeFileTouchRow {
    pub(crate) provider_session_id: String,
    pub(crate) provider_touch_index: u64,
    pub(crate) provider_event_index: Option<u64>,
    pub(crate) raw_source_path: Option<String>,
    pub(crate) source_root: Option<String>,
    pub(crate) path: String,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) old_path: Option<String>,
    pub(crate) line_count_delta: Option<i64>,
    pub(crate) confidence: Confidence,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) source_format: String,
    pub(crate) metadata: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum PiNativeRejectionKind {
    MalformedJson,
    InvalidHeader,
    InvalidRecord,
    BeforeHeader,
    OversizedRecord,
    OversizedCoreUnit,
    TooManyCoreUnits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PiNativeRejection {
    pub(crate) kind: PiNativeRejectionKind,
    pub(crate) source_record_ordinal: u64,
    pub(crate) line_number: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) diagnostic: String,
}

impl PiNativeRejection {
    pub(super) fn new(
        kind: PiNativeRejectionKind,
        source_record_ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        diagnostic: impl AsRef<str>,
    ) -> Self {
        Self {
            kind,
            source_record_ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            diagnostic: bounded_diagnostic(diagnostic.as_ref()),
        }
    }
}

fn bounded_diagnostic(value: &str) -> String {
    value.chars().take(PI_NATIVE_REJECTION_MAX_CHARS).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) enum PiNativeCoreUnit {
    Session(PiNativeSessionRow),
    Event(PiNativeEventRow),
    FileTouch(PiNativeFileTouchRow),
    Rejection(PiNativeRejection),
}

#[derive(Debug)]
pub(crate) struct PiNativeCorePage {
    pub(crate) units: Vec<PiNativeCoreUnit>,
    pub(crate) encoded_bytes: usize,
}

#[derive(Default)]
pub(super) struct PiCorePageBuilder {
    pub(super) units: Vec<PiNativeCoreUnit>,
    pub(super) encoded_bytes: usize,
}

impl PiCorePageBuilder {
    pub(super) fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub(super) fn can_push(&self, units: &[PiNativeCoreUnit], encoded_bytes: usize) -> bool {
        self.units.len().saturating_add(units.len()) <= PI_NATIVE_PAGE_MAX_UNITS
            && PI_NATIVE_PAGE_ENCODING_RESERVE
                .saturating_add(self.encoded_bytes)
                .saturating_add(encoded_bytes)
                <= PI_NATIVE_PAGE_MAX_BYTES
    }

    pub(super) fn push(&mut self, mut units: Vec<PiNativeCoreUnit>, encoded_bytes: usize) {
        self.encoded_bytes = self.encoded_bytes.saturating_add(encoded_bytes);
        self.units.append(&mut units);
    }

    pub(super) fn take(&mut self) -> PiNativeCorePage {
        PiNativeCorePage {
            units: std::mem::take(&mut self.units),
            encoded_bytes: std::mem::take(&mut self.encoded_bytes),
        }
    }
}

pub(super) fn core_units_encoded_bytes(
    units: &[PiNativeCoreUnit],
) -> Result<usize, serde_json::Error> {
    serde_json::to_vec(units).map(|bytes| bytes.len())
}
