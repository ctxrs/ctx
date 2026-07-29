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

pub fn compute_payload_hash(
    payload: &serde_json::Value,
) -> std::result::Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("fnv1a64:{:016x}", fnv1a64(&bytes)))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
mod content_ref;
pub mod dtos;
pub mod history_jsonl;
pub mod paths;
pub mod platform_security;
pub mod projection;
pub mod provider;
mod result_compaction;
pub mod source;
pub mod source_resolver;
pub mod sync;

pub use archive::SessionHistoryArchive;
pub use content_ref::ContentRef;
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
    blob_dir, config_path, database_path, default_data_root, device_path, history_dir, logs_dir,
    object_dir,
};
pub use projection::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput, NativeItemKey,
    NativeLocator, NativeSessionKey, PositionStability, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, StableEntityId, StableEntityKind,
    SubrecordSelector, TypedKey, IDENTITY_VERSION,
};
pub use provider::{
    provider_support_matrix_schema_version, ProviderArtifactDescriptor, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderFidelityClaims, ProviderId, ProviderPathKind, ProviderSourceTrust,
    ProviderSupportEntry, ProviderSupportMatrixDocument, ProviderSupportPath,
    ProviderSupportStatus, PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION,
};
pub use result_compaction::compact_result_payload;
pub use source::{CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind};
pub use source_resolver::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeRecordCoordinate, SessionHydrationRequest, SourceRecordLocator,
    SourceResolverContractError, MAX_BATCH_HYDRATION_EVENTS, NATIVE_LOCATOR_VERSION,
};
pub use sync::{
    AuditActorKind, AuditLogEntry, EntityTimestamps, Fidelity, RedactionState, SyncAlias,
    SyncBatch, SyncBatchStatus, SyncCursor, SyncDirection, SyncMetadata, SyncOutboxItem,
    SyncOutboxOperation, SyncState, Visibility,
};

pub(crate) use sync::default_metadata;

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

#[cfg(test)]
mod tests;
