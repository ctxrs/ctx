pub use ctx_history_capture_model::ProviderRootConnectorBinding;
use ctx_history_capture_model::{
    provider_source_config_digest, SourceRouteIdentity, SourceRouteIdentityError,
    MAX_CONFIGURED_PROVIDER_ROOTS,
};
use ctx_history_core::{
    CertifiedSource, CoreRecordError, ProjectionContractError, SourceKey, CORE_RECORD_VERSION,
    IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    current_core_record_contract_fingerprint, hex, is_generation_id,
    policy::{
        current_source_generation_policy_hash, LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION,
    },
    sha256_hex, source_sort_key, source_token,
};

mod digest;
mod provider_root;
use digest::{decode_sha256_hex, is_sha256_hex};
pub use provider_root::{
    AppliedProviderRoot, AppliedProviderRootSourceMembership, DetachedReleasedProviderRootAuthority,
};

pub const GENERATION_MANIFEST_VERSION: u32 = 10;
pub const LEXICAL_SCHEMA_VERSION: u32 = LEXICAL_SCHEMA_REVISION;
pub const LEXICAL_ANALYZER_VERSION: u32 = LEXICAL_TOKENIZER_REVISION;
pub const MAX_PUBLICATION_METADATA_BYTES: usize = 48 * 1024;
pub const MAX_DETACHED_RELEASED_PROVIDER_ROOTS: usize = MAX_CONFIGURED_PROVIDER_ROOTS;

pub const COMMIT_PAYLOAD_VERSION: u32 = 2;
pub const INDEX_MEMORY_MIN_PER_THREAD: usize = 15_000_000;
pub const MAX_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;

/// Comparable lexical segments are coalesced after this many accumulate.
///
/// A merge therefore retires at least `LEXICAL_SEGMENT_MERGE_FAN_IN - 1`
/// active segments, bounding merge publications amortized over tiny appends
/// while avoiding a full-index rewrite for each append. Delete-heavy segments
/// use the independent reclamation threshold in the lexical merge policy.
pub const LEXICAL_SEGMENT_MERGE_FAN_IN: usize = 16;

/// Published active segments may contain at most 1/4 deleted documents.
///
/// The merge policy compares this ratio with integer arithmetic and expunges
/// any segment above it independently of Tantivy's append-merge size ceiling.
/// Exact no-ops never construct a writer, so they intentionally do not perform
/// storage maintenance.
pub const LEXICAL_DELETED_DOCUMENT_RECLAIM_NUMERATOR: u64 = 1;
pub const LEXICAL_DELETED_DOCUMENT_RECLAIM_DENOMINATOR: u64 = 4;

pub type Result<T> = std::result::Result<T, IndexError>;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ProjectionContract(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("the lexical index has no ctx generation payload")]
    MissingCommitPayload,
    #[error("the lexical index has no active generation pointer")]
    MissingActiveGenerationPointer,
    #[error("unsupported active generation pointer version {0}")]
    UnsupportedActiveGenerationPointer(u32),
    #[error("the active generation pointer is malformed or non-canonical")]
    InvalidActiveGenerationPointer,
    #[error("the durable generation-retention lease is malformed, non-canonical, or not owner-private; resolve the unfinished lease owner before publishing Core")]
    InvalidGenerationRetentionLease,
    #[error("unsupported durable generation-retention lease version {0}")]
    UnsupportedGenerationRetentionLease(u32),
    #[error("generation-retention lease owner kind or identity is invalid")]
    InvalidGenerationRetentionLeaseOwner,
    #[error(
        "generation {requested_generation_id} cannot be leased because it is not the active or previous retained generation"
    )]
    GenerationRetentionLeaseTargetNotRetained { requested_generation_id: String },
    #[error(
        "generation {retained_generation_id} is already retained by unfinished owner kind {owner_kind}; only one durable generation-retention lease is allowed"
    )]
    GenerationRetentionLeaseConflict {
        retained_generation_id: String,
        owner_kind: String,
    },
    #[error("generation-retention lease changed ownership before release")]
    GenerationRetentionLeaseOwnerMismatch,
    #[error("unsupported commit payload version {0}")]
    UnsupportedCommitPayload(u32),
    #[error("the lexical commit payload is not in canonical ctx JSON encoding")]
    NonCanonicalCommitPayload,
    #[error("lexical commit payload is too large: {actual} bytes, maximum {maximum}")]
    CommitPayloadTooLarge { actual: usize, maximum: usize },
    #[error("opaque publication metadata is not canonical unpadded base64")]
    InvalidPublicationMetadataEncoding,
    #[error("opaque publication metadata is too large: {actual} bytes, maximum {maximum}")]
    PublicationMetadataTooLarge { actual: usize, maximum: usize },
    #[error("opaque publication metadata construction failed: {0}")]
    PublicationMetadata(String),
    #[error("unsupported generation manifest version {0}")]
    UnsupportedManifest(u32),
    #[error(
        "generation contract mismatch: identity {identity}, schema {schema}, analyzer {analyzer}, Core record {core_record}"
    )]
    GenerationContractMismatch {
        identity: u16,
        schema: u32,
        analyzer: u32,
        core_record: u32,
    },
    #[error(
        "Core record contract fingerprint mismatch: expected {expected}, generation carries {actual}"
    )]
    CoreRecordContractMismatch { expected: String, actual: String },
    #[error("current publication republish source topology is unsupported: {0}")]
    CurrentRepublishSourceTopology(&'static str),
    #[error("current publication republish exceeds the file limit: {actual}/{maximum}")]
    CurrentRepublishFileLimit { actual: usize, maximum: usize },
    #[error("current publication republish exceeds the byte limit: {actual}/{maximum}")]
    CurrentRepublishByteLimit { actual: u64, maximum: u64 },
    #[error(
        "current publication republish needs {required} bytes of headroom, but only {available} are available"
    )]
    CurrentRepublishInsufficientHeadroom { required: u64, available: u64 },
    #[error(
        "Core record revisions do not match the active generation policy: normalization {normalization}/{expected_normalization}, content {content}/{expected_content}"
    )]
    CoreRecordPolicyRevisionMismatch {
        normalization: u32,
        expected_normalization: u32,
        content: u32,
        expected_content: u32,
    },
    #[error(
        "source generation policy mismatch: expected {expected}, generation carries {actual}; \
         rebuild the disposable generation"
    )]
    GenerationPolicyMismatch { expected: String, actual: String },
    #[error("lexical index schema does not match ctx schema version {0}")]
    SchemaMismatch(u32),
    #[error("lexical index settings do not match ctx schema version {0}")]
    IndexSettingsMismatch(u32),
    #[error("a nonempty lexical index has no ctx generation payload")]
    UnboundIndexState,
    #[error("the lexical generation changed while a verified reader was opening")]
    ConcurrentGenerationChange,
    #[error(
        "requested lexical generation {expected_generation_id} is not retained: \
         active generation is {active_generation_id}, previous generation is {previous_generation_id:?}, \
         and no durable generation-retention lease matches"
    )]
    PinnedGenerationNotRetained {
        expected_generation_id: String,
        active_generation_id: String,
        previous_generation_id: Option<String>,
    },
    #[error(
        "requested lexical generation {expected_generation_id} resolved to publication payload/manifest generation {actual_generation_id}"
    )]
    PinnedGenerationMismatch {
        expected_generation_id: String,
        actual_generation_id: String,
    },
    #[error("generation manifest {0} is missing")]
    MissingManifest(String),
    #[error("generation manifest digest mismatch: expected {expected}, actual {actual}")]
    ManifestDigestMismatch { expected: String, actual: String },
    #[error("generation ID is not exactly 64 lowercase hexadecimal characters")]
    InvalidGenerationId,
    #[error("generation manifest is not in canonical ctx JSON encoding")]
    NonCanonicalManifest,
    #[error("generation manifest sources are not strictly sorted and unique")]
    NonCanonicalManifestSources,
    #[error("source route identity is not exactly 64 lowercase hexadecimal characters")]
    InvalidSourceRouteIdentity,
    #[error("generation source routes are not strictly sorted and unique")]
    NonCanonicalSourceRoutes,
    #[error("source route {0} members are not strictly sorted and unique")]
    NonCanonicalSourceRouteMembers(String),
    #[error("source route {0} has invalid active missing state")]
    InvalidSourceRouteMissingState(String),
    #[error("source route {0} is missing but has no retained members")]
    EmptyMissingSourceRoute(String),
    #[error("source route {route_id} contains source {source_id} that is not retained")]
    SourceRouteMemberNotRetained { route_id: String, source_id: String },
    #[error("retained source {0} is not owned by a source route")]
    SourceNotOwnedByRoute(String),
    #[error("retained source {0} is owned by more than one source route")]
    SourceOwnedByMultipleRoutes(String),
    #[error("generation provider-root configuration digest is invalid")]
    InvalidProviderRootConfigDigest,
    #[error("generation provider roots are invalid or non-canonical: {0}")]
    InvalidProviderRoots(String),
    #[error("provider root {root_id} references unknown source route {route_id}")]
    ProviderRootRouteNotRetained { root_id: String, route_id: String },
    #[error("source route {route_id} belongs to more than one provider root")]
    SourceRouteOwnedByMultipleProviderRoots { route_id: String },
    #[error("unknown provider root selector in the pinned generation")]
    UnknownProviderRootSelector(String),
    #[error("unknown provider root group in the pinned generation")]
    UnknownProviderRootGroup(String),
    #[error(
        "generation manifest totals do not match its source certificates: \
         documents {documents}/{expected_documents}, bytes {bytes}/{expected_bytes}"
    )]
    InvalidManifestTotals {
        documents: u64,
        expected_documents: u64,
        bytes: u64,
        expected_bytes: u64,
    },
    #[error("lexical schema is missing required field {0}")]
    MissingSchemaField(&'static str),
    #[error("index memory {actual} is below the {minimum} byte minimum")]
    IndexMemoryTooSmall { actual: usize, minimum: usize },
    #[error(
        "changed-session registry requires {required_bytes} charged bytes for {attempted_entries} entries, exceeding the {maximum_bytes} byte writer memory budget ({maximum_entries} entries maximum)"
    )]
    ChangedSessionRegistryMemoryLimitExceeded {
        attempted_entries: usize,
        required_bytes: usize,
        maximum_bytes: usize,
        maximum_entries: usize,
    },
    #[error(
        "logical verification scratch requires {required_bytes} bytes, exceeding the {maximum_bytes} byte ceiling"
    )]
    VerificationScratchLimitExceeded {
        required_bytes: u64,
        maximum_bytes: u64,
    },
    #[error("source replacement has already started for {0}")]
    DuplicateSource(String),
    #[error("certified deletion for source {0} does not match its complete inventory")]
    InvalidCertifiedSourceDeletion(String),
    #[error("source route {0} was observed missing more than once in one refresh")]
    DuplicateSourceRouteMissingObservation(String),
    #[error("source route plan is incomplete or internally inconsistent: {0}")]
    InvalidSourceRoutePlan(String),
    #[error("source route staging is already active for {0}")]
    SourceRouteStagingAlreadyActive(String),
    #[error("source route staging is not active for {0}")]
    SourceRouteStagingNotActive(String),
    #[error("carried source route {route_id} cannot mutate retained source {source_id}")]
    CarriedSourceRouteMutation { route_id: String, source_id: String },
    #[error("source route {active_route_id} cannot mutate source {source_id} owned by route {owner_route_id}")]
    SourceRouteOwnershipMutation {
        active_route_id: String,
        owner_route_id: String,
        source_id: String,
    },
    #[error("source route {0} cannot enter deletion grace because it is not retained")]
    SourceRouteMissingObservationNotRetained(String),
    #[error(
        "automatic source route deletion grace must require at least two certified observations"
    )]
    InvalidSourceRouteDeletionGraceThreshold,
    #[error("source replacement has not started for {0}")]
    SourceNotStarted(String),
    #[error("source {0} has no certified append frontier in the committed generation")]
    SourceNotAppendable(String),
    #[error("append proof does not match the writer's committed base generation")]
    AppendBaseMismatch,
    #[error("source replacement was not certified for {0}")]
    SourceNotCertified(String),
    #[error("source certificate does not match the staged source")]
    SourceCertificateMismatch,
    #[error("prepared Core record belongs to a different base-generation context")]
    PreparedCoreRecordContextMismatch,
    #[error("source {source_id} certified {certified} documents but staged {staged}")]
    SourceDocumentCountMismatch {
        source_id: String,
        certified: u64,
        staged: u64,
    },
    #[error("source {0} changed during final precommit revalidation")]
    SourceInvalidated(String),
    #[error("document field {field} is empty")]
    EmptyDocumentField { field: &'static str },
    #[error("document field {field} is too large: {actual} bytes, maximum {maximum}")]
    DocumentFieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("stored lexical document field {0} is missing, malformed, or inconsistent")]
    InvalidStoredDocumentField(&'static str),
    #[error("one session has conflicting provider-native lineage claims: {0}")]
    ConflictingProviderNativeSessionClaim(&'static str),
    #[error(
        "session grouping batch has too many exact coordinates: requested {requested}, maximum {maximum}"
    )]
    InvalidSessionGroupingCoordinateCount { requested: usize, maximum: usize },
    #[error("session grouping batch repeats exact coordinate {0}")]
    DuplicateSessionGroupingCoordinate(String),
    #[error("session grouping authority is missing exact coordinate {0}")]
    MissingSessionGroupingCoordinate(String),
    #[error("session grouping authority work limit exceeded for {operation}: maximum {maximum}")]
    SessionGroupingAuthorityWorkLimitExceeded {
        operation: &'static str,
        maximum: usize,
    },
    #[error("lexical index checksum verification failed for one or more active files")]
    ChecksumMismatch,
    #[error("ID prefix must contain 1 to 32 hexadecimal digits, with optional hyphens")]
    InvalidIdPrefix,
    #[error("query filter {field} is empty")]
    EmptyQueryFilter { field: &'static str },
    #[error("query filter {field} is too large: {actual} bytes, maximum {maximum}")]
    QueryFilterTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("lexical query text is too large: {actual} aggregate bytes, maximum {maximum}")]
    LexicalQueryBytesTooLarge { actual: usize, maximum: usize },
    #[error("lexical query has too many alternatives: observed {observed}, maximum {maximum}")]
    LexicalQueryAlternativesTooMany { observed: usize, maximum: usize },
    #[error(
        "lexical query has too many unique analyzed tokens: observed {observed}, maximum {maximum}"
    )]
    LexicalQueryTokensTooMany { observed: usize, maximum: usize },
    #[error("lexical result limit must not exceed {maximum} items, requested {requested}")]
    InvalidLexicalResultLimit { requested: usize, maximum: usize },
    #[error(
        "copied-event lineage occurrence limit must be between 1 and {maximum} items, requested {requested}"
    )]
    InvalidCopiedEventLineageOccurrenceLimit { requested: usize, maximum: usize },
    #[error(
        "copied-event lineage posting-visit limit must be between 1 and {maximum}, requested {requested}"
    )]
    InvalidCopiedEventLineagePostingVisitLimit { requested: usize, maximum: usize },
    #[error(
        "copied-event lineage exact event-and-session identity lookup exceeded {maximum} posting visits"
    )]
    CopiedEventLineageEventAndSessionIdentityPostingVisitLimitExceeded { maximum: usize },
    #[error("content scope {scope} cannot be combined with an exact event_type filter")]
    ContentScopeEventTypeConflict { scope: &'static str },
    #[error(
        "semantic event page size must be between 1 and {maximum} items, requested {requested}"
    )]
    InvalidSemanticEventPageSize { requested: usize, maximum: usize },
    #[error("source event page size must be between 1 and {maximum} items, requested {requested}")]
    InvalidSourceEventPageSize { requested: usize, maximum: usize },
    #[error(
        "session event page size must be between 1 and {maximum} items, requested {requested}"
    )]
    InvalidSessionEventPageSize { requested: usize, maximum: usize },
    #[error(
        "session event coordinate selection must be between 1 and {maximum} items, requested {requested}"
    )]
    InvalidSessionEventCoordinateLimit { requested: usize, maximum: usize },
    #[error(
        "Core event page {field} byte limit must be between 1 and {maximum}, requested {requested}"
    )]
    InvalidCoreEventPageByteLimit {
        field: &'static str,
        requested: usize,
        maximum: usize,
    },
    #[error("source {0} is not retained by the pinned generation")]
    SourceEventSourceNotRetained(String),
    #[error("source {0} has a different descriptor in the pinned generation")]
    SourceEventSourceDescriptorMismatch(String),
    #[error(
        "source event cursor belongs to generation {cursor_generation}, \
         not pinned generation {pinned_generation}"
    )]
    SourceEventCursorGenerationMismatch {
        cursor_generation: String,
        pinned_generation: String,
    },
    #[error("source event cursor belongs to a different exact source")]
    SourceEventCursorSourceMismatch,
    #[error("source event cursor does not contain a valid event identity for its exact source")]
    InvalidSourceEventCursorIdentity,
    #[error("session {0} is not present in the pinned generation")]
    SessionEventSessionNotFound(Uuid),
    #[error(
        "session event cursor belongs to generation {cursor_generation}, \
         not pinned generation {pinned_generation}"
    )]
    SessionEventCursorGenerationMismatch {
        cursor_generation: String,
        pinned_generation: String,
    },
    #[error("session event cursor belongs to a different full session identity")]
    SessionEventCursorSessionMismatch,
    #[error("session event cursor does not contain a valid full session identity")]
    InvalidSessionEventCursorSessionIdentity,
    #[error("session event cursor does not name a valid deterministic session coordinate")]
    InvalidSessionEventCursorCoordinate,
    #[error(
        "semantic event cursor belongs to generation {cursor_generation}, \
         not pinned generation {pinned_generation}"
    )]
    SemanticEventCursorGenerationMismatch {
        cursor_generation: String,
        pinned_generation: String,
    },
    #[error("semantic event cursor uses a different eligibility contract")]
    SemanticEventCursorEligibilityMismatch,
    #[error("semantic event cursor does not contain a valid event identity")]
    InvalidSemanticEventCursorIdentity,
    #[error("lexical analyzer {0} is unavailable")]
    MissingAnalyzer(&'static str),
    #[error("document source does not have an active replacement")]
    DocumentSourceNotActive,
    #[error("duplicate event identity {0} in one candidate generation")]
    DuplicateEventIdentity(String),
    #[error("session identity {0} is already owned by another source")]
    DuplicateSessionIdentity(String),
    #[error(
        "{kind} UUID collision at {uuid}: existing digest {existing_digest}, new digest {new_digest}"
    )]
    CompactIdentityCollision {
        kind: &'static str,
        uuid: Uuid,
        existing_digest: String,
        new_digest: String,
    },
    #[error("document count mismatch: manifest {manifest}, index {index}")]
    DocumentCountMismatch { manifest: u64, index: u64 },
    #[error(
        "candidate lexical segment retains {deleted_documents} deleted documents out of \
         {max_documents}, exceeding the 25% publication bound"
    )]
    CandidateDeletionDensityExceeded {
        deleted_documents: u64,
        max_documents: u64,
    },
    #[error("source {source_id} count mismatch: manifest {manifest}, index {index}")]
    SourceCountMismatch {
        source_id: String,
        manifest: u64,
        index: u64,
    },
    #[error("generation count overflow")]
    CountOverflow,
    #[error(
        "exact replay inventory coverage is incomplete: prior source {source_id} was neither \
         replayed nor terminally removed"
    )]
    IncompleteExactReplayCoverage { source_id: String },
    #[error(
        "exact replay inventory for {provider} observed {observed} sources but matched {matched} \
         retained source lineages"
    )]
    ExactReplayInventoryCountMismatch {
        provider: String,
        observed: usize,
        matched: usize,
    },
    #[error(
        "complete inventory authority {provider}/{authority_namespace} was certified more than once"
    )]
    DuplicateCompleteInventoryAuthority {
        provider: String,
        authority_namespace: String,
    },
    #[error(
        "complete inventory authority {provider}/{authority_namespace} changed during final \
         precommit revalidation"
    )]
    CompleteInventoryInvalidated {
        provider: String,
        authority_namespace: String,
    },
    #[error("generation writer invariant violated: {0}")]
    WriterInvariant(&'static str),
    #[error("the active-generation rebuild marker is malformed")]
    InvalidActiveGenerationRebuildMarker,
    #[error(
        "active lexical generation {generation_id} failed physical integrity validation and requires a source-authoritative rebuild: {detail}"
    )]
    ActiveGenerationNeedsRebuild {
        generation_id: String,
        detail: String,
    },
    #[error("generation {generation_id} committed but failed {stage} verification: {detail}")]
    CommittedGenerationNeedsRecovery {
        generation_id: String,
        stage: &'static str,
        detail: String,
    },
    #[error("source {source_id} Core-record aggregate count mismatch: manifest {manifest}, index {index}")]
    CoreRecordAggregateCountMismatch {
        source_id: String,
        manifest: u64,
        index: u64,
    },
    #[error("manifest Core-record aggregate is invalid for source {0}")]
    CoreRecordAggregateMismatch(String),
}

impl From<ctx_history_index_generation::GenerationError> for IndexError {
    fn from(error: ctx_history_index_generation::GenerationError) -> Self {
        use ctx_history_index_generation::GenerationError;

        match error {
            GenerationError::Io(error) => Self::Io(error),
            GenerationError::Json(error) => Self::Json(error),
            GenerationError::Tantivy(error) => Self::Tantivy(error),
            GenerationError::MissingActiveGenerationPointer => Self::MissingActiveGenerationPointer,
            GenerationError::UnsupportedActiveGenerationPointer(version) => {
                Self::UnsupportedActiveGenerationPointer(version)
            }
            GenerationError::InvalidActiveGenerationPointer => Self::InvalidActiveGenerationPointer,
            GenerationError::InvalidGenerationRetentionLease => {
                Self::InvalidGenerationRetentionLease
            }
            GenerationError::UnsupportedGenerationRetentionLease(version) => {
                Self::UnsupportedGenerationRetentionLease(version)
            }
            GenerationError::InvalidGenerationRetentionLeaseOwner => {
                Self::InvalidGenerationRetentionLeaseOwner
            }
            GenerationError::GenerationRetentionLeaseTargetNotRetained {
                requested_generation_id,
            } => Self::GenerationRetentionLeaseTargetNotRetained {
                requested_generation_id,
            },
            GenerationError::GenerationRetentionLeaseConflict {
                retained_generation_id,
                owner_kind,
            } => Self::GenerationRetentionLeaseConflict {
                retained_generation_id,
                owner_kind,
            },
            GenerationError::GenerationRetentionLeaseOwnerMismatch => {
                Self::GenerationRetentionLeaseOwnerMismatch
            }
            GenerationError::InvalidGenerationId => Self::InvalidGenerationId,
            GenerationError::MissingManifest(generation_id) => Self::MissingManifest(generation_id),
            GenerationError::ManifestDigestMismatch { expected, actual } => {
                Self::ManifestDigestMismatch { expected, actual }
            }
            GenerationError::IndexSettingsMismatch => {
                Self::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION)
            }
            GenerationError::ConcurrentGenerationChange => Self::ConcurrentGenerationChange,
            GenerationError::ChecksumMismatch => Self::ChecksumMismatch,
            GenerationError::CurrentRepublishSourceTopology(detail) => {
                Self::CurrentRepublishSourceTopology(detail)
            }
            GenerationError::CurrentRepublishFileLimit { actual, maximum } => {
                Self::CurrentRepublishFileLimit { actual, maximum }
            }
            GenerationError::CurrentRepublishByteLimit { actual, maximum } => {
                Self::CurrentRepublishByteLimit { actual, maximum }
            }
            GenerationError::CurrentRepublishInsufficientHeadroom {
                required,
                available,
            } => Self::CurrentRepublishInsufficientHeadroom {
                required,
                available,
            },
            GenerationError::CountOverflow => Self::CountOverflow,
        }
    }
}

impl From<SourceRouteIdentityError> for IndexError {
    fn from(_: SourceRouteIdentityError) -> Self {
        Self::InvalidSourceRouteIdentity
    }
}

/// A predecessor migration whose atomic pointer replacement became visible,
/// but whose durability or subsequent pointer reconciliation was uncertain.
///
/// This compatibility result remains public for downstream callers even though
/// current writer opens no longer construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedPredecessorMigrationRecovery {
    generation_id: String,
    detail: String,
}

impl CommittedPredecessorMigrationRecovery {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Non-zero number of consecutive certified route observations that found a
/// whole automatic source route absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsecutiveSourceMissingCount(u32);

impl ConsecutiveSourceMissingCount {
    fn first() -> Self {
        Self(1)
    }

    fn incremented(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(IndexError::CountOverflow)
    }

    pub fn get(self) -> u32 {
        self.0
    }

    fn validate(self) -> Result<()> {
        if self.0 == 0 {
            return Err(IndexError::InvalidSourceRouteMissingState(
                "zero-count".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One committed refresh point at which a complete inventory omitted a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMissingObservationPoint {
    generation_id: String,
    observed_at_unix_ms: u64,
}

impl SourceMissingObservationPoint {
    pub fn new(generation_id: String, observed_at_unix_ms: u64) -> Result<Self> {
        let point = Self {
            generation_id,
            observed_at_unix_ms,
        };
        point.validate_contract()?;
        Ok(point)
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }

    fn validate_contract(&self) -> Result<()> {
        if !is_generation_id(&self.generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        Ok(())
    }
}

/// Durable cross-refresh grace for one whole automatic route that is
/// conclusively absent. It exists only while that route still owns retained
/// current sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRouteMissingState {
    consecutive_missing: ConsecutiveSourceMissingCount,
    first_observation: SourceMissingObservationPoint,
    last_observation: SourceMissingObservationPoint,
}

impl SourceRouteMissingState {
    pub fn first(observation: SourceMissingObservationPoint) -> Self {
        Self {
            consecutive_missing: ConsecutiveSourceMissingCount::first(),
            first_observation: observation.clone(),
            last_observation: observation,
        }
    }

    pub fn advance(&self, observation: SourceMissingObservationPoint) -> Result<Self> {
        Ok(Self {
            consecutive_missing: self.consecutive_missing.incremented()?,
            first_observation: self.first_observation.clone(),
            last_observation: observation,
        })
    }

    pub fn consecutive_missing(&self) -> ConsecutiveSourceMissingCount {
        self.consecutive_missing
    }

    pub fn first_observation(&self) -> &SourceMissingObservationPoint {
        &self.first_observation
    }

    pub fn last_observation(&self) -> &SourceMissingObservationPoint {
        &self.last_observation
    }

    fn validate_contract(&self, route_id: &str) -> Result<()> {
        self.consecutive_missing
            .validate()
            .map_err(|_| IndexError::InvalidSourceRouteMissingState(route_id.to_owned()))?;
        self.first_observation
            .validate_contract()
            .map_err(|_| IndexError::InvalidSourceRouteMissingState(route_id.to_owned()))?;
        self.last_observation
            .validate_contract()
            .map_err(|_| IndexError::InvalidSourceRouteMissingState(route_id.to_owned()))?;
        if self.consecutive_missing.get() == 1 && self.first_observation != self.last_observation {
            return Err(IndexError::InvalidSourceRouteMissingState(
                route_id.to_owned(),
            ));
        }
        Ok(())
    }
}

/// Generation-authoritative membership of one route. `missing` is present
/// only during active whole-route absence grace; lifetime removed routes are
/// deliberately absent from the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRouteSnapshot {
    route_identity: SourceRouteIdentity,
    sources: std::sync::Arc<[SourceKey]>,
    missing: Option<SourceRouteMissingState>,
}

impl SourceRouteSnapshot {
    pub fn present(route_identity: SourceRouteIdentity, sources: Vec<SourceKey>) -> Result<Self> {
        Self::new(route_identity, sources, None)
    }

    pub fn missing(
        route_identity: SourceRouteIdentity,
        sources: Vec<SourceKey>,
        missing: SourceRouteMissingState,
    ) -> Result<Self> {
        Self::new(route_identity, sources, Some(missing))
    }

    fn new(
        route_identity: SourceRouteIdentity,
        mut sources: Vec<SourceKey>,
        missing: Option<SourceRouteMissingState>,
    ) -> Result<Self> {
        sources.sort_by_key(source_sort_key);
        let snapshot = Self {
            route_identity,
            sources: sources.into(),
            missing,
        };
        snapshot.validate_contract()?;
        Ok(snapshot)
    }

    pub fn route_identity(&self) -> &SourceRouteIdentity {
        &self.route_identity
    }

    pub fn sources(&self) -> &[SourceKey] {
        &self.sources
    }

    pub fn exact_snapshot_eq(&self, other: &Self) -> bool {
        self.route_identity == other.route_identity
            && self.missing == other.missing
            && (std::sync::Arc::ptr_eq(&self.sources, &other.sources)
                || self.sources == other.sources)
    }

    pub fn missing_state(&self) -> Option<&SourceRouteMissingState> {
        self.missing.as_ref()
    }

    fn validate_contract(&self) -> Result<()> {
        self.route_identity.validate().map_err(IndexError::from)?;
        if self
            .sources
            .windows(2)
            .any(|pair| source_sort_key(&pair[0]) >= source_sort_key(&pair[1]))
        {
            return Err(IndexError::NonCanonicalSourceRouteMembers(
                self.route_identity.as_str().to_owned(),
            ));
        }
        for source in self.sources.iter() {
            source.validate_contract()?;
        }
        if let Some(missing) = &self.missing {
            if self.sources.is_empty() {
                return Err(IndexError::EmptyMissingSourceRoute(
                    self.route_identity.as_str().to_owned(),
                ));
            }
            missing.validate_contract(self.route_identity.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub manifest_version: u32,
    pub identity_version: u16,
    pub core_record_version: u32,
    pub core_record_contract_fingerprint: String,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
    pub indexed_documents: u64,
    pub certified_source_bytes: u64,
    pub sources: Vec<CertifiedSource>,
    pub core_record_aggregates: Vec<SourceCoreRecordAggregate>,
    source_routes: Vec<SourceRouteSnapshot>,
    automatic_provider_discovery: bool,
    provider_root_config_digest: String,
    provider_roots: Vec<AppliedProviderRoot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    detached_released_provider_roots: Vec<DetachedReleasedProviderRootAuthority>,
}

/// Incrementally composable commitment to one source's exact stored Core
/// records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCoreRecordAggregate {
    source_identity_digest: String,
    indexed_documents: u64,
    core_record_accumulator: String,
}

impl SourceCoreRecordAggregate {
    pub fn new(
        source_identity_digest: String,
        indexed_documents: u64,
        core_record_accumulator: String,
    ) -> Result<Self> {
        let aggregate = Self {
            source_identity_digest,
            indexed_documents,
            core_record_accumulator,
        };
        aggregate.validate_contract()?;
        Ok(aggregate)
    }

    pub fn source_identity_digest(&self) -> &str {
        &self.source_identity_digest
    }

    pub fn indexed_documents(&self) -> u64 {
        self.indexed_documents
    }

    pub fn core_record_accumulator(&self) -> &str {
        &self.core_record_accumulator
    }

    pub fn accumulator_bytes(&self) -> Result<[u8; 32]> {
        decode_sha256_hex(&self.core_record_accumulator)
    }

    fn validate_contract(&self) -> Result<()> {
        if !is_sha256_hex(&self.source_identity_digest)
            || !is_sha256_hex(&self.core_record_accumulator)
        {
            return Err(IndexError::InvalidGenerationId);
        }
        Ok(())
    }
}

mod generation_manifest;
#[cfg(any(test, feature = "test-support"))]
fn test_aggregates(sources: &[CertifiedSource]) -> Result<Vec<SourceCoreRecordAggregate>> {
    sources
        .iter()
        .map(|source| {
            SourceCoreRecordAggregate::new(
                crate::source_token(source.observation().source()),
                source.counts().indexed_documents,
                "00".repeat(32),
            )
        })
        .collect()
}

pub fn implicit_source_routes(sources: &[CertifiedSource]) -> Result<Vec<SourceRouteSnapshot>> {
    sources
        .iter()
        .map(|source| {
            let source_key = source.observation().source().clone();
            let route_identity = SourceRouteIdentity::from_sha256(sha256_hex(
                format!(
                    "ctx-implicit-source-route-v1\0{}",
                    crate::source_token(&source_key)
                )
                .as_bytes(),
            ))?;
            SourceRouteSnapshot::present(route_identity, vec![source_key])
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize)]
pub struct CommitPayload {
    pub version: u32,
    pub generation_id: String,
    pub publication_metadata: Option<String>,
}
