use std::time::SystemTime;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("could not determine a home directory for the default ctx data root")]
    MissingHome,
    #[error("invalid {enum_name} value: {value}")]
    InvalidEnumValue {
        enum_name: &'static str,
        value: String,
    },
}

pub type Result<T> = std::result::Result<T, CoreError>;

pub fn utc_now() -> DateTime<Utc> {
    DateTime::<Utc>::from(SystemTime::now())
}

macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
        default $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn variants() -> &'static [&'static str] {
                &[$($value),+]
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(CoreError::InvalidEnumValue {
                        enum_name: stringify!($name),
                        value: value.to_owned(),
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

pub mod archive;
pub mod dtos;
pub mod history_jsonl;
pub mod paths;
pub mod provider;
pub mod redaction;
pub mod source;
pub mod sync;

pub use archive::{CaptureEnvelope, SessionHistoryArchive};
pub use dtos::{
    AgentType, Artifact, ArtifactKind, CitationReference, Confidence, ContextCitation,
    ContextCitationType, ContextLinks, ContextPagination, ContextTruncation, Event, EventRole,
    EventType, FileChangeKind, FileTouched, HistoryRecord, HistoryRecordLink,
    HistoryRecordLinkTargetType, HistoryRecordLinkType, HistoryRecordMetadata, HistoryRecordStatus,
    HistoryRecordTag, RecordEdge, RecordEdgeType, Run, RunStatus, RunType, Session, SessionEdge,
    SessionEdgeType, SessionStatus, Summary, SummaryKind, Tag, TagKind, VcsChange, VcsChangeKind,
    VcsHost, VcsKind, VcsWorkspace,
};
pub use history_jsonl::{
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlManifestRecord, CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord,
    CtxHistoryJsonlSourceRecord, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
pub use paths::{
    blob_dir, config_path, database_path, default_data_root, device_path, history_dir, inbox_dir,
    logs_dir, object_dir, spool_dir,
};
pub use provider::{
    provider_capture_envelope_schema_version, provider_support_matrix_schema_version,
    ProviderArtifactDescriptor, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderEventEnvelope, ProviderFidelityClaims, ProviderId,
    ProviderMatrixPriority, ProviderPathKind, ProviderRawRetention, ProviderRedactionBoundary,
    ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust, ProviderSupportEntry,
    ProviderSupportMatrixDocument, ProviderSupportPath, ProviderSupportStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION, PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION,
};
pub use redaction::{
    redact_preview, redact_secret_markers, redact_share_safe_markers, redact_share_safe_preview,
    RedactionState,
};
pub use source::{CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind};
pub use sync::{
    AuditActorKind, AuditLogEntry, EntityTimestamps, Fidelity, SyncAlias, SyncBatch,
    SyncBatchStatus, SyncCursor, SyncDirection, SyncMetadata, SyncOutboxItem, SyncOutboxOperation,
    SyncState, Visibility,
};

pub(crate) use sync::default_metadata;

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

#[cfg(test)]
mod tests;
