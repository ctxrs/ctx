use ctx_history_core::{
    core_record_contract_fingerprint, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecordError, ProjectionContractError, SourceKey,
    CORE_RECORD_VERSION, IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    identity::is_generation_id,
    policy::{
        current_source_generation_policy_hash, LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION,
    },
    sha256_hex, source_sort_key,
};

pub const GENERATION_MANIFEST_VERSION: u32 = 5;
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
/// while avoiding a full-index rewrite for each append.
pub const LEXICAL_SEGMENT_MERGE_FAN_IN: usize = 8;

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
    #[error("generation manifest removals are not strictly sorted and unique")]
    NonCanonicalManifestRemovals,
    #[error("generation source catalog missing states are not strictly sorted and unique")]
    NonCanonicalSourceCatalogMissingStates,
    #[error("generation manifest retains and removes source {0}")]
    ManifestSourceRemovalOverlap(String),
    #[error("generation source catalog has invalid missing state for source {0}")]
    InvalidSourceCatalogMissingState(String),
    #[error("generation source catalog marks unretained source {0} as missing")]
    SourceCatalogMissingSourceNotRetained(String),
    #[error("certified removal for source {0} does not match its complete inventory")]
    InvalidGenerationRemoval(String),
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
    #[error("source {0} was observed missing more than once in one refresh")]
    DuplicateSourceMissingObservation(String),
    #[error("source {0} cannot enter deletion grace because it is not retained")]
    SourceMissingObservationNotRetained(String),
    #[error("automatic source deletion grace must require at least two complete inventories")]
    InvalidSourceDeletionGraceThreshold,
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

/// Metadata-only proof that one source lineage was authoritatively absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRemoval {
    deletion: CertifiedSourceDeletion,
    inventory: CertifiedSourceInventory,
}

/// Non-zero number of consecutive complete inventories that omitted a source.
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
            return Err(IndexError::InvalidSourceCatalogMissingState(
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

/// Durable cross-refresh state for one retained source absent from a complete inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCatalogMissingState {
    latest_deletion_candidate: CertifiedSourceDeletion,
    consecutive_missing: ConsecutiveSourceMissingCount,
    first_observation: SourceMissingObservationPoint,
    last_observation: SourceMissingObservationPoint,
}

impl SourceCatalogMissingState {
    pub(crate) fn first(
        latest_deletion_candidate: CertifiedSourceDeletion,
        observation: SourceMissingObservationPoint,
    ) -> Self {
        Self {
            latest_deletion_candidate,
            consecutive_missing: ConsecutiveSourceMissingCount::first(),
            first_observation: observation.clone(),
            last_observation: observation,
        }
    }

    pub(crate) fn advance(
        &self,
        latest_deletion_candidate: CertifiedSourceDeletion,
        observation: SourceMissingObservationPoint,
    ) -> Result<Self> {
        if !self
            .source()
            .exact_descriptor_eq(latest_deletion_candidate.source())
            || !same_inventory_authority(
                self.latest_deletion_candidate.inventory(),
                latest_deletion_candidate.inventory(),
            )
        {
            return Ok(Self::first(latest_deletion_candidate, observation));
        }
        Ok(Self {
            latest_deletion_candidate,
            consecutive_missing: self.consecutive_missing.incremented()?,
            first_observation: self.first_observation.clone(),
            last_observation: observation,
        })
    }

    pub fn source(&self) -> &SourceKey {
        self.latest_deletion_candidate.source()
    }

    pub fn latest_deletion_candidate(&self) -> &CertifiedSourceDeletion {
        &self.latest_deletion_candidate
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

    fn validate_contract(&self) -> Result<()> {
        let source_id = self.source().identity().to_string();
        self.latest_deletion_candidate
            .validate_contract()
            .map_err(|_| IndexError::InvalidSourceCatalogMissingState(source_id.clone()))?;
        self.consecutive_missing
            .validate()
            .map_err(|_| IndexError::InvalidSourceCatalogMissingState(source_id.clone()))?;
        self.first_observation
            .validate_contract()
            .map_err(|_| IndexError::InvalidSourceCatalogMissingState(source_id.clone()))?;
        self.last_observation
            .validate_contract()
            .map_err(|_| IndexError::InvalidSourceCatalogMissingState(source_id.clone()))?;
        if self.consecutive_missing.get() == 1 && self.first_observation != self.last_observation {
            return Err(IndexError::InvalidSourceCatalogMissingState(source_id));
        }
        Ok(())
    }
}

fn same_inventory_authority(
    left: &ctx_history_core::SourceInventoryObservation,
    right: &ctx_history_core::SourceInventoryObservation,
) -> bool {
    left.provider() == right.provider()
        && left.authority_namespace() == right.authority_namespace()
        && left.authority_key() == right.authority_key()
}

/// Generation-bound ctx source catalog state that is not provider content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCatalogCheckpoint {
    missing_sources: Vec<SourceCatalogMissingState>,
}

impl SourceCatalogCheckpoint {
    pub(crate) fn from_missing_sources(
        mut missing_sources: Vec<SourceCatalogMissingState>,
    ) -> Result<Self> {
        missing_sources.sort_by(|left, right| {
            source_sort_key(left.source()).cmp(&source_sort_key(right.source()))
        });
        let checkpoint = Self { missing_sources };
        checkpoint.validate_contract()?;
        Ok(checkpoint)
    }

    pub fn missing_sources(&self) -> &[SourceCatalogMissingState] {
        &self.missing_sources
    }

    pub fn missing_source(&self, source: &SourceKey) -> Option<&SourceCatalogMissingState> {
        self.missing_sources
            .binary_search_by(|candidate| {
                source_sort_key(candidate.source()).cmp(&source_sort_key(source))
            })
            .ok()
            .and_then(|index| self.missing_sources.get(index))
            .filter(|candidate| candidate.source().exact_descriptor_eq(source))
    }

    pub fn is_empty(&self) -> bool {
        self.missing_sources.is_empty()
    }

    fn validate_contract(&self) -> Result<()> {
        if self
            .missing_sources
            .windows(2)
            .any(|pair| source_sort_key(pair[0].source()) >= source_sort_key(pair[1].source()))
        {
            return Err(IndexError::NonCanonicalSourceCatalogMissingStates);
        }
        for state in &self.missing_sources {
            state.validate_contract()?;
        }
        Ok(())
    }
}

impl GenerationRemoval {
    pub fn new(
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<Self> {
        let removal = Self {
            deletion,
            inventory,
        };
        removal.validate_contract()?;
        Ok(removal)
    }

    pub fn deletion(&self) -> &CertifiedSourceDeletion {
        &self.deletion
    }

    pub fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }

    pub fn source(&self) -> &SourceKey {
        self.deletion.source()
    }

    pub(crate) fn validate_contract(&self) -> Result<()> {
        self.deletion.validate_contract()?;
        self.inventory.validate_contract()?;
        if !self.deletion.verifies(&self.inventory) {
            return Err(IndexError::InvalidGenerationRemoval(
                self.deletion.source().identity().to_string(),
            ));
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
    pub removals: Vec<GenerationRemoval>,
    source_catalog: SourceCatalogCheckpoint,
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
    pub(crate) fn new(
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

    pub(crate) fn accumulator_bytes(&self) -> Result<[u8; 32]> {
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

impl GenerationManifest {
    #[cfg(test)]
    pub(crate) fn from_sources(sources: Vec<CertifiedSource>) -> Result<Self> {
        Self::from_parts(sources, Vec::new())
    }

    #[cfg(test)]
    pub(crate) fn from_parts(
        sources: Vec<CertifiedSource>,
        removals: Vec<GenerationRemoval>,
    ) -> Result<Self> {
        let aggregates = sources
            .iter()
            .map(|source| {
                SourceCoreRecordAggregate::new(
                    crate::source_token(source.observation().source()),
                    source.counts().indexed_documents,
                    "00".repeat(32),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_catalog_parts_with_record_aggregates(
            sources,
            aggregates,
            removals,
            SourceCatalogCheckpoint::default(),
        )
    }

    pub(crate) fn from_catalog_parts_with_record_aggregates(
        mut sources: Vec<CertifiedSource>,
        mut core_record_aggregates: Vec<SourceCoreRecordAggregate>,
        mut removals: Vec<GenerationRemoval>,
        source_catalog: SourceCatalogCheckpoint,
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
        removals.sort_by(|left, right| {
            source_sort_key(left.source()).cmp(&source_sort_key(right.source()))
        });
        if removals
            .windows(2)
            .any(|pair| source_sort_key(pair[0].source()) >= source_sort_key(pair[1].source()))
        {
            return Err(IndexError::NonCanonicalManifestRemovals);
        }
        core_record_aggregates.sort_by(|left, right| {
            left.source_identity_digest
                .cmp(&right.source_identity_digest)
        });
        let mut indexed_documents = 0_u64;
        let mut certified_source_bytes = 0_u64;
        for source in &sources {
            indexed_documents = indexed_documents
                .checked_add(source.counts().indexed_documents)
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
            certified_source_bytes,
            sources,
            core_record_aggregates,
            removals,
            source_catalog,
        };
        manifest.validate_contract()?;
        Ok(manifest)
    }

    pub fn generation_id(&self) -> Result<String> {
        Ok(sha256_hex(&serde_json::to_vec(self)?))
    }

    pub fn source_catalog(&self) -> &SourceCatalogCheckpoint {
        &self.source_catalog
    }

    pub(crate) fn validate_contract(&self) -> Result<()> {
        if self.sources.windows(2).any(|pair| {
            source_sort_key(pair[0].observation().source())
                >= source_sort_key(pair[1].observation().source())
        }) {
            return Err(IndexError::NonCanonicalManifestSources);
        }
        if self
            .removals
            .windows(2)
            .any(|pair| source_sort_key(pair[0].source()) >= source_sort_key(pair[1].source()))
        {
            return Err(IndexError::NonCanonicalManifestRemovals);
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
        self.source_catalog.validate_contract()?;
        let mut source_index = 0;
        for removal in &self.removals {
            removal.validate_contract()?;
            let removal_key = source_sort_key(removal.source());
            while self
                .sources
                .get(source_index)
                .is_some_and(|source| source_sort_key(source.observation().source()) < removal_key)
            {
                source_index += 1;
            }
            if self
                .sources
                .get(source_index)
                .is_some_and(|source| source_sort_key(source.observation().source()) == removal_key)
            {
                return Err(IndexError::ManifestSourceRemovalOverlap(
                    removal.source().identity().to_string(),
                ));
            }
        }
        let mut expected_documents = 0_u64;
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
            expected_bytes = expected_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
        }
        if self.core_record_aggregates.len() != self.sources.len() {
            return Err(IndexError::CoreRecordAggregateMismatch(
                "manifest aggregate cardinality".to_owned(),
            ));
        }
        for missing in self.source_catalog.missing_sources() {
            let retained = self.sources.binary_search_by(|candidate| {
                source_sort_key(candidate.observation().source())
                    .cmp(&source_sort_key(missing.source()))
            });
            let is_exactly_retained = retained
                .ok()
                .and_then(|index| self.sources.get(index))
                .is_some_and(|source| {
                    source
                        .observation()
                        .source()
                        .exact_descriptor_eq(missing.source())
                });
            if !is_exactly_retained {
                return Err(IndexError::SourceCatalogMissingSourceNotRetained(
                    missing.source().identity().to_string(),
                ));
            }
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

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_sha256_hex(value: &str) -> Result<[u8; 32]> {
    if !is_sha256_hex(value) {
        return Err(IndexError::InvalidGenerationId);
    }
    let mut decoded = [0_u8; 32];
    for (output, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = hex_nibble(pair[0]).ok_or(IndexError::InvalidGenerationId)?;
        let low = hex_nibble(pair[1]).ok_or(IndexError::InvalidGenerationId)?;
        *output = (high << 4) | low;
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
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
