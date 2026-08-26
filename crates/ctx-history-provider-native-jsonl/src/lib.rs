//! Native JSONL provider dialects, source traversal, and Core projections.
//!
//! Capture composes this provider package with its lifecycle and publication
//! runtime. This crate owns no capture registry or publication authority.

use std::path::Path;

use ctx_history_jsonl::{JsonlFamilyError, JsonlFamilyRuntime};
use ctx_history_source_io::{
    visit_bounded_tree_files, SourceIoError, NON_REGULAR_PROVIDER_SOURCE_REASON,
    REPARSE_PROVIDER_SOURCE_REASON, SYMLINK_PROVIDER_SOURCE_REASON,
};
use thiserror::Error;

pub const ANTIGRAVITY_CLI_SOURCE_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";
pub const TABNINE_CLI_SOURCE_FORMAT: &str = "tabnine_cli_chat_recording_jsonl";
pub const QODER_SOURCE_FORMAT: &str = "qoder_transcript_jsonl";
pub const FACTORY_DROID_SOURCE_FORMAT: &str = "factory_ai_droid_sessions_jsonl";
pub const COPILOT_CLI_SOURCE_FORMAT: &str = "copilot_cli_session_events_jsonl";
pub const QWEN_CODE_SOURCE_FORMAT: &str = "qwen_code_chat_jsonl";
pub use ctx_history_capture_model::PROVIDER_MAX_PREVIEW_CHARS;
pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

pub use ctx_history_source_io::ProviderJsonlInventoryLimit;

#[derive(Debug, Error)]
pub enum NativeJsonlError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath {
        path: std::path::PathBuf,
        reason: &'static str,
    },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ProviderJsonlInventoryLimit,
        maximum: usize,
        observed: usize,
    },
    #[error("{0} worker thread panicked")]
    WorkerPanicked(&'static str),
    #[error("system I/O error during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system invariant failed: {0}")]
    SystemInvariant(&'static str),
    #[error("provider source changed during bounded capture")]
    SourceChangedDuringCapture,
    #[error("{provider} source {path:?} failed ({kind}): {detail}")]
    ProviderSource {
        provider: &'static str,
        path: std::path::PathBuf,
        kind: ctx_history_capture_model::ProviderSourceFailureKind,
        detail: String,
    },
}

impl From<SourceIoError> for NativeJsonlError {
    fn from(error: SourceIoError) -> Self {
        match error {
            SourceIoError::Io(error) => Self::Io(error),
            SourceIoError::Json(error) => Self::Json(error),
            SourceIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SourceIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => Self::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            },
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
        }
    }
}

impl JsonlFamilyError for NativeJsonlError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }
    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }
    fn worker_panicked(worker: &'static str) -> Self {
        Self::WorkerPanicked(worker)
    }
    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }
    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::SystemIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
    fn is_source_changed(&self) -> bool {
        matches!(
            self,
            Self::SourceChangedDuringCapture
                | Self::InvalidProviderTranscriptPath {
                    reason: "provider source changed while its authority handle was retained",
                    ..
                }
        )
    }
    fn is_resource_unavailable(&self) -> bool {
        matches!(self, Self::Io(_) | Self::SystemIo { .. }) && !self.is_not_found()
    }
    fn is_internal(&self) -> bool {
        matches!(self, Self::SystemInvariant(_) | Self::WorkerPanicked(_))
    }
    fn is_ignorable_membership_entry(&self) -> bool {
        matches!(
            self,
            Self::InvalidProviderTranscriptPath { reason, .. }
                if *reason == SYMLINK_PROVIDER_SOURCE_REASON
                    || *reason == REPARSE_PROVIDER_SOURCE_REASON
                    || *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
        )
    }
}

pub type Result<T> = std::result::Result<T, NativeJsonlError>;
pub use NativeJsonlError as CaptureError;

pub fn compute_payload_hash(value: &serde_json::Value) -> Result<String> {
    Ok(ctx_history_core::compute_payload_hash(value)?)
}
pub trait NativeJsonlRuntime: JsonlFamilyRuntime<Error = NativeJsonlError> {
    /// Preserves the established Tabnine unavailable-source classification
    /// without coupling provider parsing to capture's concrete error type.
    fn tabnine_unavailable_source(_path: &Path, error: Self::Error) -> Self::Error {
        error
    }

    fn is_tabnine_unavailable_source(_error: &Self::Error) -> bool {
        false
    }
}

mod dialect;
pub mod native_path;
mod normalization;
pub mod result_content;
#[cfg(test)]
mod test_support;

pub use native_path::DirectJsonlFamilyAdapter;
pub fn native_jsonl_timestamp(value: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    normalization::native_jsonl_timestamp(value)
}

pub fn visit_native_jsonl_files_with<E>(
    root: &Path,
    provider: ctx_history_core::CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> std::result::Result<(), E>,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
{
    visit_bounded_tree_files(
        root,
        &mut |candidate| {
            dialect::native_jsonl_file_candidate_is_selected(provider, root, candidate)
        },
        &mut |source_file| visit(source_file.path()),
    )
}

#[cfg(test)]
mod test_support_paths {
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("ctx-native-jsonl-")
            .tempdir()
    }
}

#[cfg(test)]
mod tests;
