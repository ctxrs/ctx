use ctx_history_core::{
    core_record_contract_fingerprint, CertifiedSource, CertifiedSourceDeletion, CoreRecordError,
    ProjectionContractError, SourceKey, CORE_RECORD_VERSION, IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hex,
    identity::is_generation_id,
    policy::{
        current_source_generation_policy_hash, LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION,
    },
    sha256_hex, source_sort_key,
};

mod digest;

use digest::{decode_sha256_hex, is_sha256_hex};

pub const GENERATION_MANIFEST_VERSION: u32 = 7;
pub const LEXICAL_SCHEMA_VERSION: u32 = LEXICAL_SCHEMA_REVISION;
pub const LEXICAL_ANALYZER_VERSION: u32 = LEXICAL_TOKENIZER_REVISION;

pub(crate) const MANIFEST_DIRECTORY: &str = "ctx-generations";
pub(crate) const COMMIT_PAYLOAD_VERSION: u32 = 1;
pub(crate) const INDEX_MEMORY_MIN_PER_THREAD: usize = 15_000_000;
pub(crate) const MAX_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;

/// Comparable lexical segments are coalesced after this many accumulate.
///
/// A merge therefore retires at least `LEXICAL_SEGMENT_MERGE_FAN_IN - 1`
/// active segments, bounding merge publications amortized over tiny appends
/// while avoiding a full-index rewrite for each append. Delete-heavy segments
/// use the independent reclamation threshold in the lexical merge policy.
pub const LEXICAL_SEGMENT_MERGE_FAN_IN: usize = 8;

/// Published active segments may contain at most 1/4 deleted documents.
///
/// The merge policy compares this ratio with integer arithmetic and expunges
/// any segment above it independently of Tantivy's append-merge size ceiling.
/// Exact no-ops never construct a writer, so they intentionally do not perform
/// storage maintenance.
pub(crate) const LEXICAL_DELETED_DOCUMENT_RECLAIM_NUMERATOR: u64 = 1;
pub(crate) const LEXICAL_DELETED_DOCUMENT_RECLAIM_DENOMINATOR: u64 = 4;

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
    #[error("unsupported commit payload version {0}")]
    UnsupportedCommitPayload(u32),
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
    #[error("source replacement has already started for {0}")]
    DuplicateSource(String),
    #[error("certified deletion for source {0} does not match its complete inventory")]
    InvalidCertifiedSourceDeletion(String),
    #[error("source route {0} was observed missing more than once in one refresh")]
    DuplicateSourceRouteMissingObservation(String),
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
        "semantic-eligible document count {eligible} exceeds indexed document count {indexed}"
    )]
    InvalidSemanticEligibleDocumentCount { eligible: u64, indexed: u64 },
    #[error(
        "semantic-eligible document count mismatch: manifest {manifest}, source aggregates {aggregates}"
    )]
    SemanticEligibleDocumentCountMismatch { manifest: u64, aggregates: u64 },
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
        "physical integrity receipt for generation {generation_id} is missing or invalid: {detail}"
    )]
    GenerationPhysicalIntegrityMismatch {
        generation_id: String,
        detail: String,
    },
    #[error(
        "active lexical generation {generation_id} failed its physical integrity check and requires a source-authoritative rebuild: {detail}"
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

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub indexer_threads: usize,
    pub memory_bytes: usize,
}

impl Default for WriterOptions {
    fn default() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self {
            indexer_threads: parallelism.clamp(1, 8),
            memory_bytes: 512 * 1024 * 1024,
        }
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
    pub(crate) fn new(generation_id: String, observed_at_unix_ms: u64) -> Result<Self> {
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
    pub(crate) fn first(observation: SourceMissingObservationPoint) -> Self {
        Self {
            consecutive_missing: ConsecutiveSourceMissingCount::first(),
            first_observation: observation.clone(),
            last_observation: observation,
        }
    }

    pub(crate) fn advance(&self, observation: SourceMissingObservationPoint) -> Result<Self> {
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

/// Exact identity of one selected ingestion route. The digest is derived by
/// discovery from the provider, format, selection authority, and exact local
/// route locator; paths themselves do not enter Core or Pro records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRouteIdentity(String);

impl SourceRouteIdentity {
    pub fn from_sha256(value: String) -> Result<Self> {
        if !is_sha256_hex(&value) {
            return Err(IndexError::InvalidSourceRouteIdentity);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate_contract(&self) -> Result<()> {
        if !is_sha256_hex(&self.0) {
            return Err(IndexError::InvalidSourceRouteIdentity);
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
    sources: Vec<SourceKey>,
    missing: Option<SourceRouteMissingState>,
}

impl SourceRouteSnapshot {
    pub fn present(route_identity: SourceRouteIdentity, sources: Vec<SourceKey>) -> Result<Self> {
        Self::new(route_identity, sources, None)
    }

    pub(crate) fn missing(
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
        sources.sort_by(|left, right| source_sort_key(left).cmp(&source_sort_key(right)));
        let snapshot = Self {
            route_identity,
            sources,
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

    pub fn missing_state(&self) -> Option<&SourceRouteMissingState> {
        self.missing.as_ref()
    }

    fn validate_contract(&self) -> Result<()> {
        self.route_identity.validate_contract()?;
        if self
            .sources
            .windows(2)
            .any(|pair| source_sort_key(&pair[0]) >= source_sort_key(&pair[1]))
        {
            return Err(IndexError::NonCanonicalSourceRouteMembers(
                self.route_identity.0.clone(),
            ));
        }
        for source in &self.sources {
            source.validate_contract()?;
        }
        if let Some(missing) = &self.missing {
            if self.sources.is_empty() {
                return Err(IndexError::EmptyMissingSourceRoute(
                    self.route_identity.0.clone(),
                ));
            }
            missing.validate_contract(&self.route_identity.0)?;
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
    pub semantic_eligible_documents: u64,
    pub certified_source_bytes: u64,
    pub sources: Vec<CertifiedSource>,
    pub core_record_aggregates: Vec<SourceCoreRecordAggregate>,
    source_routes: Vec<SourceRouteSnapshot>,
}

/// Incrementally composable commitment to one source's exact stored Core
/// records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCoreRecordAggregate {
    source_identity_digest: String,
    indexed_documents: u64,
    semantic_eligible_documents: u64,
    core_record_accumulator: String,
}

impl SourceCoreRecordAggregate {
    pub(crate) fn new(
        source_identity_digest: String,
        indexed_documents: u64,
        semantic_eligible_documents: u64,
        core_record_accumulator: String,
    ) -> Result<Self> {
        let aggregate = Self {
            source_identity_digest,
            indexed_documents,
            semantic_eligible_documents,
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

    pub fn semantic_eligible_documents(&self) -> u64 {
        self.semantic_eligible_documents
    }

    pub fn core_record_accumulator(&self) -> &str {
        &self.core_record_accumulator
    }

    pub(crate) fn accumulator_bytes(&self) -> Result<[u8; 32]> {
        decode_sha256_hex(&self.core_record_accumulator)
    }

    fn validate_contract(&self) -> Result<()> {
        if !is_sha256_hex(&self.source_identity_digest)
            || !is_sha256_hex(&self.core_record_accumulator)
        {
            return Err(IndexError::InvalidGenerationId);
        }
        if self.semantic_eligible_documents > self.indexed_documents {
            return Err(IndexError::InvalidSemanticEligibleDocumentCount {
                eligible: self.semantic_eligible_documents,
                indexed: self.indexed_documents,
            });
        }
        Ok(())
    }
}

impl GenerationManifest {
    #[cfg(test)]
    pub(crate) fn from_sources(sources: Vec<CertifiedSource>) -> Result<Self> {
        let aggregates = test_aggregates(&sources)?;
        let source_routes = implicit_source_routes(&sources)?;
        Self::from_parts_with_record_aggregates(sources, aggregates, source_routes)
    }

    #[cfg(test)]
    pub(crate) fn from_parts(
        sources: Vec<CertifiedSource>,
        source_routes: Vec<SourceRouteSnapshot>,
    ) -> Result<Self> {
        let aggregates = test_aggregates(&sources)?;
        Self::from_parts_with_record_aggregates(sources, aggregates, source_routes)
    }

    pub(crate) fn from_parts_with_record_aggregates(
        mut sources: Vec<CertifiedSource>,
        mut core_record_aggregates: Vec<SourceCoreRecordAggregate>,
        mut source_routes: Vec<SourceRouteSnapshot>,
    ) -> Result<Self> {
        sources.sort_by(|left, right| {
            source_sort_key(left.observation().source())
                .cmp(&source_sort_key(right.observation().source()))
        });
        if sources.windows(2).any(|pair| {
            source_sort_key(pair[0].observation().source())
                >= source_sort_key(pair[1].observation().source())
        }) {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        source_routes.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        core_record_aggregates.sort_by(|left, right| {
            left.source_identity_digest
                .cmp(&right.source_identity_digest)
        });
        let mut indexed_documents = 0_u64;
        let mut semantic_eligible_documents = 0_u64;
        let mut certified_source_bytes = 0_u64;
        for (source, aggregate) in sources.iter().zip(&core_record_aggregates) {
            indexed_documents = indexed_documents
                .checked_add(source.counts().indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            semantic_eligible_documents = semantic_eligible_documents
                .checked_add(aggregate.semantic_eligible_documents)
                .ok_or(IndexError::CountOverflow)?;
            certified_source_bytes = certified_source_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
        }
        let manifest = Self {
            manifest_version: GENERATION_MANIFEST_VERSION,
            identity_version: IDENTITY_VERSION,
            core_record_version: CORE_RECORD_VERSION,
            core_record_contract_fingerprint: core_record_contract_fingerprint(),
            lexical_schema_version: LEXICAL_SCHEMA_VERSION,
            lexical_analyzer_version: LEXICAL_ANALYZER_VERSION,
            policy_schema_hash: current_source_generation_policy_hash()?,
            indexed_documents,
            semantic_eligible_documents,
            certified_source_bytes,
            sources,
            core_record_aggregates,
            source_routes,
        };
        manifest.validate_contract()?;
        Ok(manifest)
    }

    pub fn generation_id(&self) -> Result<String> {
        Ok(sha256_hex(&serde_json::to_vec(self)?))
    }

    pub fn source_routes(&self) -> &[SourceRouteSnapshot] {
        &self.source_routes
    }

    pub fn source_route(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> Option<&SourceRouteSnapshot> {
        self.source_routes
            .binary_search_by(|candidate| candidate.route_identity().cmp(route_identity))
            .ok()
            .and_then(|index| self.source_routes.get(index))
    }

    pub(crate) fn validate_contract(&self) -> Result<()> {
        if self.sources.windows(2).any(|pair| {
            source_sort_key(pair[0].observation().source())
                >= source_sort_key(pair[1].observation().source())
        }) {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        if self
            .source_routes
            .windows(2)
            .any(|pair| pair[0].route_identity() >= pair[1].route_identity())
        {
            return Err(IndexError::NonCanonicalSourceRoutes);
        }
        if self
            .core_record_aggregates
            .windows(2)
            .any(|pair| pair[0].source_identity_digest >= pair[1].source_identity_digest)
        {
            return Err(IndexError::CoreRecordAggregateMismatch(
                "non-canonical aggregate ordering".to_owned(),
            ));
        }
        let mut owned_sources = Vec::new();
        for route in &self.source_routes {
            route.validate_contract()?;
            for route_source in route.sources() {
                let retained = self.sources.binary_search_by(|candidate| {
                    source_sort_key(candidate.observation().source())
                        .cmp(&source_sort_key(route_source))
                });
                let is_exactly_retained = retained
                    .ok()
                    .and_then(|index| self.sources.get(index))
                    .is_some_and(|source| {
                        source
                            .observation()
                            .source()
                            .exact_descriptor_eq(route_source)
                    });
                if !is_exactly_retained {
                    return Err(IndexError::SourceRouteMemberNotRetained {
                        route_id: route.route_identity().as_str().to_owned(),
                        source_id: route_source.identity().to_string(),
                    });
                }
                owned_sources.push(source_sort_key(route_source));
            }
        }
        owned_sources.sort();
        if let Some(duplicate) = owned_sources.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(IndexError::SourceOwnedByMultipleRoutes(hex(&duplicate[0])));
        }
        for source in &self.sources {
            let key = source_sort_key(source.observation().source());
            if owned_sources.binary_search(&key).is_err() {
                return Err(IndexError::SourceNotOwnedByRoute(
                    source.observation().source().identity().to_string(),
                ));
            }
        }
        let mut expected_documents = 0_u64;
        let mut expected_semantic_eligible_documents = 0_u64;
        let mut expected_bytes = 0_u64;
        for (source_index, source) in self.sources.iter().enumerate() {
            source.validate_contract()?;
            let source_id = crate::source_token(source.observation().source());
            let aggregate = self
                .core_record_aggregates
                .get(source_index)
                .ok_or_else(|| IndexError::CoreRecordAggregateMismatch(source_id.clone()))?;
            aggregate.validate_contract()?;
            if aggregate.source_identity_digest != source_id {
                return Err(IndexError::CoreRecordAggregateMismatch(source_id));
            }
            if aggregate.indexed_documents != source.counts().indexed_documents {
                return Err(IndexError::CoreRecordAggregateCountMismatch {
                    source_id: aggregate.source_identity_digest.clone(),
                    manifest: source.counts().indexed_documents,
                    index: aggregate.indexed_documents,
                });
            }
            expected_documents = expected_documents
                .checked_add(source.counts().indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            expected_semantic_eligible_documents = expected_semantic_eligible_documents
                .checked_add(aggregate.semantic_eligible_documents)
                .ok_or(IndexError::CountOverflow)?;
            expected_bytes = expected_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
        }
        if self.core_record_aggregates.len() != self.sources.len() {
            return Err(IndexError::CoreRecordAggregateMismatch(
                "manifest aggregate cardinality".to_owned(),
            ));
        }
        if self.semantic_eligible_documents != expected_semantic_eligible_documents {
            return Err(IndexError::SemanticEligibleDocumentCountMismatch {
                manifest: self.semantic_eligible_documents,
                aggregates: expected_semantic_eligible_documents,
            });
        }
        if self.indexed_documents != expected_documents
            || self.certified_source_bytes != expected_bytes
        {
            return Err(IndexError::InvalidManifestTotals {
                documents: self.indexed_documents,
                expected_documents,
                bytes: self.certified_source_bytes,
                expected_bytes,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
fn test_aggregates(sources: &[CertifiedSource]) -> Result<Vec<SourceCoreRecordAggregate>> {
    sources
        .iter()
        .map(|source| {
            SourceCoreRecordAggregate::new(
                crate::source_token(source.observation().source()),
                source.counts().indexed_documents,
                0,
                "00".repeat(32),
            )
        })
        .collect()
}

pub(crate) fn implicit_source_routes(
    sources: &[CertifiedSource],
) -> Result<Vec<SourceRouteSnapshot>> {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CommitPayload {
    pub(crate) version: u32,
    pub(crate) generation_id: String,
}

#[derive(Debug, Clone)]
pub struct CommitReceipt {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub semantic_eligible_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
    manifest: GenerationManifest,
}

impl CommitReceipt {
    pub(crate) fn from_manifest(opstamp: u64, manifest: GenerationManifest) -> Result<Self> {
        Ok(Self {
            generation_id: manifest.generation_id()?,
            opstamp,
            indexed_documents: manifest.indexed_documents,
            semantic_eligible_documents: manifest.semantic_eligible_documents,
            certified_sources: manifest.sources.len(),
            certified_source_bytes: manifest.certified_source_bytes,
            manifest,
        })
    }

    /// Returns the exact immutable manifest snapshot published by this commit.
    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}
