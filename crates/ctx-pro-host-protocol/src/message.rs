use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{
    ApplyCoreEventDeltaPageRequest, ApplyCoreEventDeltaPagesRequest,
    ApplyCoreSourceDeltaPageRequest, AuthorizationRequest, AuthorizationResult,
    BeginCoreMaterializationRequest, BlameRequest, BlameResult, ConfirmGraphKeyDeletionRequest,
    CoreEventDeltaPageApplied, CoreEventDeltaPagesApplied, CoreEventStatePage,
    CoreEventStatePageRequest, CoreMaterializationBegan, CoreMaterializationFinished,
    CoreMaterializationReceipt, CoreSourceDeltaPageApplied, ErrorClass,
    FinishCoreMaterializationRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, ProtocolError, FRAME_HEADER_BYTES, PROTOCOL_FINGERPRINT,
    PROTOCOL_VERSION,
};

const ENVELOPE_SEQUENCE_PREFIX_BYTES: usize = "{\"sequence\":".len();
const ENVELOPE_REQUEST_ID_PREFIX_BYTES: usize = ",\"request_id\":\"".len();
const UUID_HYPHENATED_BYTES: usize = 36;
const APPLY_CORE_SOURCE_DELTA_PAGE_MESSAGE_PREFIX_BYTES: usize =
    "\",\"message\":{\"kind\":\"apply_core_source_delta_page\",\"body\":".len();
const CORE_SOURCE_APPLIED_MESSAGE_PREFIX_BYTES: usize =
    "\",\"message\":{\"kind\":\"core_source_delta_page_applied\",\"body\":".len();
const CORE_MATERIALIZATION_FRAME_SUFFIX_BYTES: usize = "}}".len();

const fn u64_decimal_bytes(mut value: u64) -> usize {
    let mut bytes = 1;
    while value >= 10 {
        value /= 10;
        bytes += 1;
    }
    bytes
}

fn core_materialization_message_frame_wire_bytes(
    sequence: u64,
    message_prefix_bytes: usize,
    encoded_body_bytes: usize,
    overflow_message: &'static str,
) -> Result<usize, ProtocolError> {
    FRAME_HEADER_BYTES
        .checked_add(ENVELOPE_SEQUENCE_PREFIX_BYTES)
        .and_then(|bytes| bytes.checked_add(u64_decimal_bytes(sequence)))
        .and_then(|bytes| bytes.checked_add(ENVELOPE_REQUEST_ID_PREFIX_BYTES))
        .and_then(|bytes| bytes.checked_add(UUID_HYPHENATED_BYTES))
        .and_then(|bytes| bytes.checked_add(message_prefix_bytes))
        .and_then(|bytes| bytes.checked_add(encoded_body_bytes))
        .and_then(|bytes| bytes.checked_add(CORE_MATERIALIZATION_FRAME_SUFFIX_BYTES))
        .ok_or_else(|| ProtocolError::new(ErrorClass::Bounds, overflow_message))
}

pub(crate) fn apply_core_source_delta_page_request_frame_wire_bytes_from_request_bytes(
    sequence: u64,
    encoded_request_bytes: usize,
) -> Result<usize, ProtocolError> {
    core_materialization_message_frame_wire_bytes(
        sequence,
        APPLY_CORE_SOURCE_DELTA_PAGE_MESSAGE_PREFIX_BYTES,
        encoded_request_bytes,
        "Core source delta request frame byte count overflowed",
    )
}

/// Returns the exact byte length of a complete framed source-page request.
///
/// UUIDs have a fixed 36-byte JSON representation. Callers that must admit a
/// request before its transport sequence is available use `u64::MAX`; every
/// actual Protocol V2 sequence is then no larger than the admitted frame.
pub fn apply_core_source_delta_page_request_frame_wire_bytes(
    sequence: u64,
    request: &ApplyCoreSourceDeltaPageRequest,
) -> Result<usize, ProtocolError> {
    apply_core_source_delta_page_request_frame_wire_bytes_from_request_bytes(
        sequence,
        crate::core_materialization::encoded_len(request)?,
    )
}

pub(crate) fn core_source_delta_page_applied_frame_wire_bytes_from_response_bytes(
    sequence: u64,
    encoded_response_bytes: usize,
) -> Result<usize, ProtocolError> {
    core_materialization_message_frame_wire_bytes(
        sequence,
        CORE_SOURCE_APPLIED_MESSAGE_PREFIX_BYTES,
        encoded_response_bytes,
        "Core source delta acknowledgement frame byte count overflowed",
    )
}

/// Returns the exact byte length of a complete framed source acknowledgement.
///
/// UUIDs have a fixed 36-byte JSON representation. Callers that must admit a
/// response before its transport sequence is available use `u64::MAX`; every
/// actual Protocol V2 sequence is then no larger than the admitted frame.
pub fn core_source_delta_page_applied_frame_wire_bytes(
    sequence: u64,
    response: &CoreSourceDeltaPageApplied,
) -> Result<usize, ProtocolError> {
    core_source_delta_page_applied_frame_wire_bytes_from_response_bytes(
        sequence,
        crate::core_materialization::encoded_len(response)?,
    )
}

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
    CoreEventStatePage(CoreEventStatePageRequest),
    ApplyCoreEventDeltaPage(ApplyCoreEventDeltaPageRequest),
    FinishCoreMaterialization(FinishCoreMaterializationRequest),
    Blame(BlameRequest),
    ApplyCoreEventDeltaPages(ApplyCoreEventDeltaPagesRequest),
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
    CoreEventStatePage(CoreEventStatePage),
    CoreEventDeltaPageApplied(CoreEventDeltaPageApplied),
    CoreMaterializationFinished(CoreMaterializationFinished),
    Blame(Box<BlameResult>),
    Error(ProtocolError),
    CoreEventDeltaPagesApplied(CoreEventDeltaPagesApplied),
}

/// Independently selectable helper behavior that exists in Protocol V2.
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

/// Exact Protocol V2 handshake. There is no compatibility range negotiation.
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
    fn global_prerequisites_available(&self) -> bool {
        self.entitlement == ProAccessState::Available && self.graph_key == ProAccessState::Available
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProOperation {
    FileBlame,
    CommitBlame,
    PullRequestBlame,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCoverage {
    pub repository_candidate_events: u64,
    pub logical_binding_events: u64,
    pub certified_live_root_access_events: u64,
    pub file_evidence_events: u64,
    pub exact_commit_evidence_events: u64,
    pub exact_pull_request_evidence_events: u64,
}

impl RepositoryCoverage {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate_for_receipt(
        &self,
        receipt: Option<&CoreMaterializationReceipt>,
    ) -> Result<(), ProtocolError> {
        let Some(receipt) = receipt else {
            if self.is_empty() {
                return Ok(());
            }
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "repository coverage requires a completed Core receipt",
            ));
        };
        for (label, count) in [
            (
                "repository candidate events",
                self.repository_candidate_events,
            ),
            ("logical binding events", self.logical_binding_events),
            (
                "certified live-root access events",
                self.certified_live_root_access_events,
            ),
            ("file evidence events", self.file_evidence_events),
            (
                "exact commit evidence events",
                self.exact_commit_evidence_events,
            ),
            (
                "exact pull-request evidence events",
                self.exact_pull_request_evidence_events,
            ),
        ] {
            if count > receipt.event_count {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    format!("repository coverage {label} exceeds Core receipt event count"),
                ));
            }
        }
        if self.logical_binding_events > self.repository_candidate_events {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "repository coverage logical binding events exceed candidate coverage",
            ));
        }
        for (label, count) in [
            (
                "certified live-root access events",
                self.certified_live_root_access_events,
            ),
            ("file evidence events", self.file_evidence_events),
            (
                "exact commit evidence events",
                self.exact_commit_evidence_events,
            ),
            (
                "exact pull-request evidence events",
                self.exact_pull_request_evidence_events,
            ),
        ] {
            if count > self.logical_binding_events {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    format!("repository coverage {label} exceeds logical binding coverage"),
                ));
            }
        }
        Ok(())
    }
}

pub const MAX_JOURNAL_FINISH_WORKERS: u16 = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalFinishActivity {
    pub worker_limit: u16,
    pub peak_workers: u16,
    pub started_after_preparation: bool,
}

impl JournalFinishActivity {
    pub fn validate(&self, journal_packs_written: u64) -> Result<(), ProtocolError> {
        if self.worker_limit > MAX_JOURNAL_FINISH_WORKERS
            || self.peak_workers > self.worker_limit
            || u64::from(self.peak_workers) > journal_packs_written
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Journal finish activity exceeds its worker or pack bound",
            ));
        }
        if (self.worker_limit == 0 && self.peak_workers != 0)
            || (self.worker_limit != 0 && journal_packs_written != 0 && self.peak_workers == 0)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Journal finish activity has an invalid zero or positive worker vector",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProStorageEvidence {
    pub graph_manifest_schema: u16,
    pub flat_format_version: u16,
    pub materializer_checkpoint_version: u16,
    pub journal_pack_format_version: u16,
    pub legacy_journals_written: u64,
    pub journal_pages_written: u64,
    pub journal_packs_written: u64,
    pub journal_finish_activity: JournalFinishActivity,
}

impl ProStorageEvidence {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.graph_manifest_schema != 3
            || self.flat_format_version != 2
            || self.materializer_checkpoint_version != 4
            || self.journal_pack_format_version != 3
            || self.legacy_journals_written != 0
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Pro storage evidence has an unsupported format identity",
            ));
        }
        if self.journal_pages_written == 0
            || self.journal_packs_written == 0
            || self.journal_packs_written > self.journal_pages_written
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Pro storage evidence has invalid journal publication counts",
            ));
        }
        self.journal_finish_activity
            .validate(self.journal_packs_written)?;
        Ok(())
    }
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusResult {
    pub currentness: CoreProjectionCurrentness,
    pub requested_core_generation_id: Option<String>,
    pub core_receipt: Option<CoreMaterializationReceipt>,
    pub coverage: MaterializedCoverage,
    pub repository_coverage: RepositoryCoverage,
    pub core_preparation_peak_workers: u16,
    pub access: ProAccessStatus,
    pub supported_operations: BTreeSet<ProOperation>,
    pub available_operations: BTreeSet<ProOperation>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub storage_evidence: Option<ProStorageEvidence>,
}

impl StatusResult {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.core_preparation_peak_workers > 16 {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core preparation peak workers cannot exceed 16",
            ));
        }
        if let Some(receipt) = &self.core_receipt {
            receipt.validate()?;
        }
        self.repository_coverage
            .validate_for_receipt(self.core_receipt.as_ref())?;
        if let Some(evidence) = &self.storage_evidence {
            evidence.validate()?;
            if self.core_receipt.is_none() {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Pro storage evidence requires a completed Core receipt",
                ));
            }
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
        if let Some(receipt) = &self.core_receipt {
            let valid_terminal_mapping = match self.coverage {
                MaterializedCoverage::Empty => {
                    receipt.event_count == 0 && self.repository_coverage.is_empty()
                }
                MaterializedCoverage::Abstained => {
                    receipt.event_count > 0 && self.repository_coverage.logical_binding_events == 0
                }
                MaterializedCoverage::Complete => {
                    receipt.event_count > 0 && self.repository_coverage.logical_binding_events > 0
                }
                MaterializedCoverage::NotMaterialized | MaterializedCoverage::Partial => true,
            };
            if !valid_terminal_mapping {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "materialized coverage does not match the completed Core receipt and repository coverage",
                ));
            }
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
        let globally_ready = self.currentness == CoreProjectionCurrentness::Current
            && self.coverage == MaterializedCoverage::Complete
            && self.access.global_prerequisites_available();
        if !globally_ready && !self.available_operations.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "unready Core coverage, entitlement, or graph key cannot advertise available blame operations",
            ));
        }
        for operation in &self.available_operations {
            let prerequisites_available = match operation {
                ProOperation::FileBlame => {
                    self.access.local_repository == ProAccessState::Available
                        && self.repository_coverage.certified_live_root_access_events > 0
                        && self.repository_coverage.file_evidence_events > 0
                        && self.repository_coverage.exact_commit_evidence_events > 0
                }
                ProOperation::CommitBlame => {
                    self.repository_coverage.exact_commit_evidence_events > 0
                }
                ProOperation::PullRequestBlame => {
                    self.repository_coverage.exact_pull_request_evidence_events > 0
                }
            };
            if !prerequisites_available {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    match operation {
                        ProOperation::FileBlame => {
                            "file blame requires local-repository access, certified live-root access, file evidence, and exact commit evidence"
                        }
                        ProOperation::CommitBlame => {
                            "commit blame requires exact commit evidence"
                        }
                        ProOperation::PullRequestBlame => {
                            "pull-request blame requires exact pull-request evidence"
                        }
                    },
                ));
            }
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
