//! Gemini CLI provider implementation.
//!
//! The capture facade supplies the concrete lifecycle and registration
//! authority; this crate owns Gemini discovery, parsing, projection, and tests.

use std::path::PathBuf;

use ctx_history_jsonl::JsonlFamilyError;
use ctx_history_source_io::SourceIoError;
use thiserror::Error;

pub const GEMINI_CLI_SOURCE_FORMAT: &str = "gemini_cli_chat_recording_jsonl";
pub const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;

#[derive(Debug, Error)]
pub enum GeminiError {
    #[error(transparent)]
    Source(#[from] SourceIoError),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error("system invariant failed: {0}")]
    SystemInvariant(&'static str),
    #[error("provider source changed during bounded capture")]
    SourceChangedDuringCapture,
}

pub type GeminiResult<T> = std::result::Result<T, GeminiError>;

pub(crate) mod io {
    use crate::GeminiError;

    ctx_history_source_io::define_mapped_source_io_compat!(GeminiError);
}

pub trait GeminiRuntime: ctx_history_jsonl::JsonlFamilyRuntime<Error = GeminiError> {}

impl<T> GeminiRuntime for T where T: ctx_history_jsonl::JsonlFamilyRuntime<Error = GeminiError> {}

impl From<std::io::Error> for GeminiError {
    fn from(error: std::io::Error) -> Self {
        SourceIoError::from(error).into()
    }
}

impl From<serde_json::Error> for GeminiError {
    fn from(error: serde_json::Error) -> Self {
        SourceIoError::from(error).into()
    }
}

impl JsonlFamilyError for GeminiError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }

    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }

    fn worker_panicked(worker: &'static str) -> Self {
        Self::SystemInvariant(worker)
    }

    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }

    fn is_not_found(&self) -> bool {
        matches!(self, Self::Source(SourceIoError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::Source(SourceIoError::SystemIo { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
    }

    fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChangedDuringCapture)
            || matches!(self, Self::InvalidProviderTranscriptPath { reason, .. } if *reason == "provider source changed while its authority handle was retained")
            || matches!(self, Self::Source(error) if error.is_source_changed())
    }

    fn is_source_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Source(SourceIoError::SystemIo { operation, source })
                if ctx_history_source_io::is_provider_source_unavailable_io(operation, source)
        )
    }

    fn is_resource_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Source(SourceIoError::Io(_) | SourceIoError::SystemIo { .. })
        ) && !self.is_not_found()
            && !self.is_source_unavailable()
    }

    fn is_internal(&self) -> bool {
        matches!(
            self,
            Self::SystemInvariant(_) | Self::Source(SourceIoError::SystemInvariant(_))
        )
    }

    fn is_ignorable_membership_entry(&self) -> bool {
        matches!(self, Self::Source(error) if ctx_history_source_io::is_symlink_source_rejection(error) || ctx_history_source_io::is_non_regular_source_rejection(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_source_io_change_classification_preserves_invalid_path_taxonomy() {
        let changed = GeminiError::Source(SourceIoError::InvalidProviderTranscriptPath {
            path: PathBuf::from("retained-root"),
            reason: "provider source changed while its authority handle was retained",
        });
        assert!(changed.is_source_changed());

        for logical in [
            GeminiError::Source(SourceIoError::InvalidProviderTranscriptPath {
                path: PathBuf::from("linked-transcript"),
                reason: "linked Gemini transcript path components are rejected",
            }),
            GeminiError::InvalidProviderTranscriptPath {
                path: PathBuf::from("malformed-layout"),
                reason: "Gemini transcript layout is invalid",
            },
        ] {
            assert!(!logical.is_source_changed());
        }
    }
}

pub mod nativepath;
