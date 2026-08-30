use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderJsonlInventoryLimit {
    Directories,
    Depth,
    EligiblePaths,
    MetadataEntries,
}

impl std::fmt::Display for ProviderJsonlInventoryLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Directories => "directories",
            Self::Depth => "depth",
            Self::EligiblePaths => "eligible_jsonl_paths",
            Self::MetadataEntries => "metadata_entries",
        })
    }
}

#[derive(Debug, Error)]
pub enum SourceIoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ProviderJsonlInventoryLimit,
        maximum: usize,
        observed: usize,
    },
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
}

pub type Result<T> = std::result::Result<T, SourceIoError>;

pub const PROVIDER_SOURCE_IO_OPERATION_PREFIX: &str = "provider source ";

pub fn is_provider_source_io_operation(operation: &str) -> bool {
    operation.starts_with(PROVIDER_SOURCE_IO_OPERATION_PREFIX)
}

pub fn is_provider_source_unavailable_io(operation: &str, source: &std::io::Error) -> bool {
    is_provider_source_io_operation(operation)
        && source.kind() == std::io::ErrorKind::PermissionDenied
}
