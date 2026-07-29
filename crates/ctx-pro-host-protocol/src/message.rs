use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{
    AuthorizationRequest, AuthorizationResult, BeginOutputInventoryRequest,
    BeginSourceManifestRequest, BlameRequest, BlameResult, ConfirmGraphKeyDeletionRequest,
    DeleteSourceRequest, ErrorClass, FinishOutputInventoryRequest, FinishSourceManifestRequest,
    GraphKeyDeleted, GraphKeyDeletionPrepared, JournalCheckpoint, JournalSyncRequest,
    JournalSyncResult, MaterializeSourcePageRequest, ObserveOutputSourceRequest,
    OutputInventoryBegan, OutputInventoryFinished, OutputPageMaterialized, OutputProgressRequest,
    OutputProgressResult, OutputSourceObserved, PrepareGraphKeyDeletionRequest,
    PrepareSourceRequest, ProOutputMaterializationPage, ProtocolError, SourceDeleted,
    SourceManifestBegan, SourceManifestFinished, SourceManifestReceipt, SourcePageMaterialized,
    SourcePrepared, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
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
    SyncJournal(JournalSyncRequest),
    BeginOutputInventory(BeginOutputInventoryRequest),
    ObserveOutputSource(ObserveOutputSourceRequest),
    MaterializeOutputPage(ProOutputMaterializationPage),
    FinishOutputInventory(FinishOutputInventoryRequest),
    GetOutputProgress(OutputProgressRequest),
    BeginSourceManifest(BeginSourceManifestRequest),
    PrepareSource(PrepareSourceRequest),
    MaterializeSourcePage(MaterializeSourcePageRequest),
    DeleteSource(DeleteSourceRequest),
    FinishSourceManifest(FinishSourceManifestRequest),
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
    JournalSynced(JournalSyncResult),
    OutputInventoryBegan(OutputInventoryBegan),
    OutputSourceObserved(OutputSourceObserved),
    OutputPageMaterialized(OutputPageMaterialized),
    OutputInventoryFinished(OutputInventoryFinished),
    OutputProgress(OutputProgressResult),
    SourceManifestBegan(SourceManifestBegan),
    SourcePrepared(SourcePrepared),
    SourcePageMaterialized(SourcePageMaterialized),
    SourceDeleted(SourceDeleted),
    SourceManifestFinished(SourceManifestFinished),
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
    JournalSync,
    OutputMaterialization,
    SourceMaterialization,
    Query,
    GitRead,
}

impl Capability {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::EntitlementAuthorization => "entitlement_authorization",
            Self::GraphKeyDeletion => "graph_key_deletion",
            Self::Status => "status",
            Self::JournalSync => "journal_sync",
            Self::OutputMaterialization => "output_materialization",
            Self::SourceMaterialization => "source_materialization",
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
pub struct StatusRequest {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphState {
    NotMaterialized,
    NeedsRebuild,
    Partial,
    NeedsResume,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationAuthority {
    Journal,
    Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusResult {
    pub state: GraphState,
    pub authority: MaterializationAuthority,
    pub checkpoint: Option<JournalCheckpoint>,
    pub source_receipt: Option<SourceManifestReceipt>,
}

impl StatusResult {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.validate()?;
        }
        if let Some(receipt) = &self.source_receipt {
            receipt.validate()?;
        }
        match self.authority {
            MaterializationAuthority::Journal => {
                if self.source_receipt.is_some() {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "journal status cannot report source authority",
                    ));
                }
                if self.state == GraphState::Ready && self.checkpoint.is_none() {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "ready journal status requires a completed checkpoint",
                    ));
                }
            }
            MaterializationAuthority::Source => {
                if self.checkpoint.is_some() {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "source status cannot report journal authority",
                    ));
                }
                if self.state == GraphState::Ready && self.source_receipt.is_none() {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "ready source status requires a completed receipt",
                    ));
                }
                if self.state != GraphState::Ready && self.source_receipt.is_some() {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "incomplete source status cannot report a completed receipt",
                    ));
                }
            }
        }
        Ok(())
    }
}
