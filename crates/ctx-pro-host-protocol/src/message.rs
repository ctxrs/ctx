use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{
    ApplyCoreSourceDeltaPageRequest, AuthorizationRequest, AuthorizationResult,
    BeginCoreMaterializationRequest, BlameRequest, BlameResult, ConfirmGraphKeyDeletionRequest,
    CoreMaterializationBegan, CoreMaterializationFinished, CoreMaterializationReceipt,
    CoreRecordPageMaterialized, CoreSourceDeltaPageApplied, ErrorClass,
    FinishCoreMaterializationRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    MaterializeCoreRecordPageRequest, PrepareGraphKeyDeletionRequest, ProtocolError,
    PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostEnvelope {
    pub sequence: u64,
    pub request_id: Uuid,
    pub message: HostMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HelperEnvelope {
    pub sequence: u64,
    pub request_id: Uuid,
    pub message: HelperMessage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeWire<M> {
    sequence: u64,
    request_id: Uuid,
    message: M,
}

fn decode_envelope<'de, D, M>(deserializer: D) -> Result<EnvelopeWire<M>, D::Error>
where
    D: Deserializer<'de>,
    M: Deserialize<'de>,
{
    let wire = EnvelopeWire::deserialize(deserializer)?;
    if wire.request_id.is_nil() {
        return Err(serde::de::Error::custom(
            "request_id must be a non-nil UUID",
        ));
    }
    Ok(wire)
}

impl<'de> Deserialize<'de> for HostEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = decode_envelope(deserializer)?;
        Ok(Self {
            sequence: wire.sequence,
            request_id: wire.request_id,
            message: wire.message,
        })
    }
}

impl<'de> Deserialize<'de> for HelperEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = decode_envelope(deserializer)?;
        Ok(Self {
            sequence: wire.sequence,
            request_id: wire.request_id,
            message: wire.message,
        })
    }
}

// Core record pages intentionally carry complete records and can dominate this
// enum's stack size. Boxing changes only the Rust representation, never wire V1.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HostMessage {
    Hello(HelloRequest),
    Authorize(AuthorizationRequest),
    PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest),
    ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest),
    Status(StatusRequest),
    BeginCoreMaterialization(BeginCoreMaterializationRequest),
    ApplyCoreSourceDeltaPage(ApplyCoreSourceDeltaPageRequest),
    MaterializeCoreRecordPage(MaterializeCoreRecordPageRequest),
    FinishCoreMaterialization(FinishCoreMaterializationRequest),
    Blame(BlameRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HelperMessage {
    Hello(HelloResult),
    Authorized(AuthorizationResult),
    GraphKeyDeletionPrepared(GraphKeyDeletionPrepared),
    GraphKeyDeleted(GraphKeyDeleted),
    Status(StatusResult),
    CoreMaterializationBegan(CoreMaterializationBegan),
    CoreSourceDeltaPageApplied(CoreSourceDeltaPageApplied),
    CoreRecordPageMaterialized(CoreRecordPageMaterialized),
    CoreMaterializationFinished(CoreMaterializationFinished),
    Blame(BlameResult),
    Error(ProtocolError),
}

/// Independently selectable helper behavior that exists in Protocol V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    EntitlementAuthorization,
    GraphKeyDeletion,
    Status,
    CoreMaterialization,
    Query,
    GitRead,
}

impl Capability {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::EntitlementAuthorization => "entitlement_authorization",
            Self::GraphKeyDeletion => "graph_key_deletion",
            Self::Status => "status",
            Self::CoreMaterialization => "core_materialization",
            Self::Query => "query",
            Self::GitRead => "git_read",
        }
    }
}

/// Exact Protocol V1 handshake. There is no compatibility range negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloRequest {
    pub protocol_version: u16,
    pub protocol_fingerprint: String,
    pub host_version: String,
    pub capabilities: BTreeSet<Capability>,
}

impl HelloRequest {
    #[must_use]
    pub fn current(host_version: impl Into<String>, capabilities: BTreeSet<Capability>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            host_version: host_version.into(),
            capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloResult {
    pub protocol_version: u16,
    pub protocol_fingerprint: String,
    pub helper_version: String,
    pub capabilities: BTreeSet<Capability>,
    pub authorization_challenge_base64url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusRequest {
    pub requested_core_generation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreProjectionCurrentness {
    NotMaterialized,
    Partial,
    Stale,
    NeedsRebuild,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializedCoverage {
    NotMaterialized,
    Partial,
    Complete,
    Empty,
    Abstained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProAccessState {
    Available,
    Locked,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProAccessStatus {
    pub entitlement: ProAccessState,
    pub graph_key: ProAccessState,
    pub local_repository: ProAccessState,
}

impl ProAccessStatus {
    fn all_available(&self) -> bool {
        self.entitlement == ProAccessState::Available
            && self.graph_key == ProAccessState::Available
            && self.local_repository == ProAccessState::Available
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProOperation {
    FileBlame,
    CommitBlame,
    PullRequestBlame,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusResult {
    pub currentness: CoreProjectionCurrentness,
    pub requested_core_generation_id: Option<String>,
    pub core_receipt: Option<CoreMaterializationReceipt>,
    pub coverage: MaterializedCoverage,
    pub access: ProAccessStatus,
    pub supported_operations: BTreeSet<ProOperation>,
    pub available_operations: BTreeSet<ProOperation>,
}

impl StatusResult {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(receipt) = &self.core_receipt {
            receipt.validate()?;
        }
        if let Some(generation) = &self.requested_core_generation_id {
            validate_lower_sha256(generation, "requested Core generation")?;
        }
        match self.currentness {
            CoreProjectionCurrentness::NotMaterialized => {
                if self.core_receipt.is_some()
                    || self.coverage != MaterializedCoverage::NotMaterialized
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "unmaterialized Core status cannot carry a receipt or coverage",
                    ));
                }
            }
            CoreProjectionCurrentness::Current => {
                let receipt = self.core_receipt.as_ref().ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Sequence,
                        "current Core status requires a completed receipt",
                    )
                })?;
                if self
                    .requested_core_generation_id
                    .as_deref()
                    .is_some_and(|requested| requested != receipt.core_generation_id)
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "current Core status receipt does not match the requested generation",
                    ));
                }
            }
            CoreProjectionCurrentness::Stale => {
                let receipt = self.core_receipt.as_ref().ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Sequence,
                        "stale Core status requires the last completed receipt",
                    )
                })?;
                if self
                    .requested_core_generation_id
                    .as_deref()
                    .is_none_or(|requested| requested == receipt.core_generation_id)
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "stale Core status requires distinct requested and receipt generations",
                    ));
                }
            }
            CoreProjectionCurrentness::Partial | CoreProjectionCurrentness::NeedsRebuild => {}
        }
        let terminal_coverage = matches!(
            self.coverage,
            MaterializedCoverage::Complete
                | MaterializedCoverage::Empty
                | MaterializedCoverage::Abstained
        );
        if terminal_coverage != (self.currentness == CoreProjectionCurrentness::Current) {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "terminal materialized coverage requires a current Core projection",
            ));
        }
        if !self
            .available_operations
            .is_subset(&self.supported_operations)
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "available Pro operations must be a subset of supported operations",
            ));
        }
        let blame_ready = self.currentness == CoreProjectionCurrentness::Current
            && self.coverage == MaterializedCoverage::Complete
            && self.access.all_available();
        if !blame_ready && !self.available_operations.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "unready Core coverage or access cannot advertise available blame operations",
            ));
        }
        Ok(())
    }
}

fn validate_lower_sha256(value: &str, label: &'static str) -> Result<(), ProtocolError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    Err(ProtocolError::new(
        ErrorClass::InvalidRequest,
        format!("{label} must be lowercase SHA-256"),
    ))
}
