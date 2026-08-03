use std::time::SystemTime;

use chrono::{DateTime, Utc};
use thiserror::Error;

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

mod core_record;
pub mod dtos;
pub mod history_jsonl;
pub mod paths;
pub mod platform_security;
pub mod projection;
pub mod provider;
mod result_compaction;
pub mod source;

pub use core_record::{
    core_record_accumulator_leaf_digest, core_record_contract_fingerprint, core_record_leaf_digest,
    core_record_leaf_sha256, CoreContent, CoreContentPolicyStatus, CoreRecord,
    CoreRecordAnnotation, CoreRecordError, CoreRecordResult, GitObjectFormat, GitObjectId,
    RepositoryAbstention, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryBinding, RepositoryCandidate, RepositoryCandidateEvidence, RepositoryCandidateKind,
    RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryFileInvocationEvidence, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryLocalRootAuthorization, RepositoryObjectReplacement, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation,
    RepositoryPullRequestAssociationObservation, RepositoryPullRequestIdentity,
    RepositoryVcsObservation, RepositoryVcsObservationKind, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_CONTENT_POLICY_REVISION, CORE_MISSING_ACTIVITY_TIME_UNIX_MS, CORE_NORMALIZATION_REVISION,
    CORE_RECORD_ACCUMULATOR_IDENTITY, CORE_RECORD_LEAF_DOMAIN, CORE_RECORD_VERSION,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION, CORE_REPOSITORY_CONTRACT_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
pub use dtos::{
    AgentType, ArtifactKind, Confidence, EventRole, EventType, Fidelity, FileChangeKind,
    SessionEdgeType, SessionStatus,
};
pub use history_jsonl::{
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlManifestRecord, CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord,
    CtxHistoryJsonlSourceRecord, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
pub use paths::{
    config_path, default_data_root, device_path, history_dir, logs_dir, managed_data_root,
};
pub use projection::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput, NativeItemKey,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey,
    SourceObservation, StableEntityId, StableEntityKind, SubrecordSelector, TypedKey,
    IDENTITY_VERSION,
};
pub use provider::{
    provider_support_matrix_schema_version, ProviderArtifactDescriptor, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderFidelityClaims, ProviderId, ProviderPathKind, ProviderSourceTrust,
    ProviderSupportEntry, ProviderSupportMatrixDocument, ProviderSupportPath,
    ProviderSupportStatus, PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION,
};
pub use result_compaction::compact_result_payload;
pub use source::CaptureProvider;
pub(crate) fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests;
