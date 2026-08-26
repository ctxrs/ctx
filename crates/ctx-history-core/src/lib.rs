use std::time::SystemTime;

use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
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
pub mod projection;
pub mod provider;
pub mod source;

pub use core_record::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    core_record_accumulator_leaf_digest, core_record_contract_fingerprint, core_record_leaf_digest,
    core_record_leaf_sha256, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CoreActivity, CoreContent, CoreContentPolicyStatus,
    CoreDiscoveryExclusion, CoreRecord, CoreRecordAnnotation, CoreRecordError, CoreRecordResult,
    LiteralFactKind, ProviderDeclaredFact, ProviderNativeCopyProof, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship, CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION,
    CORE_NORMALIZATION_REVISION, CORE_RECORD_ACCUMULATOR_IDENTITY, CORE_RECORD_LEAF_DOMAIN,
    CORE_RECORD_VERSION, CORE_RELATIONSHIP_CONTRACT_REVISION, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES, MAX_PROVIDER_DECLARED_FACTS,
};
pub use dtos::{ArtifactKind, EventRole, EventType, Fidelity, SessionStatus};
pub use history_jsonl::{
    CtxHistoryJsonlCopiedFromSelector, CtxHistoryJsonlCopyProofKind, CtxHistoryJsonlEdgeRecord,
    CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileReferenceRecord, CtxHistoryJsonlLineageContract,
    CtxHistoryJsonlManifestRecord, CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord,
    CtxHistoryJsonlSourceRecord, CTX_HISTORY_JSONL_SCHEMA_VERSION,
};
pub use projection::{
    derive_event_id, derive_native_session_id, derive_session_id, CertifiedSource,
    CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput,
    NativeItemKey, NativeSessionKey, PositionStability, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, StableEntityId, StableEntityKind,
    SubrecordSelector, TypedKey, IDENTITY_VERSION,
};
pub use provider::{
    provider_support_matrix_schema_version, ProviderArtifactDescriptor, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderFidelityClaims, ProviderId, ProviderPathKind, ProviderSourceTrust,
    ProviderSupportEntry, ProviderSupportMatrixDocument, ProviderSupportPath,
    ProviderSupportStatus, PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION,
};
pub use source::CaptureProvider;
pub(crate) fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests;
