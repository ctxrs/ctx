use ctx_history_core::{
    core_record_contract_fingerprint, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecordError, ProjectionContractError, SourceKey,
    CORE_RECORD_VERSION, IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    policy::{
        current_source_generation_policy_hash, LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION,
    },
    sha256_hex, source_sort_key,
};

pub const GENERATION_MANIFEST_VERSION: u32 = 4;
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
    #[error("generation manifest retains and removes source {0}")]
    ManifestSourceRemovalOverlap(String),
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
    #[error("generation {generation_id} committed but failed {stage} verification: {detail}")]
    CommittedGenerationNeedsRecovery {
        generation_id: String,
        stage: &'static str,
        detail: String,
    },
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
    pub removals: Vec<GenerationRemoval>,
}

impl GenerationManifest {
    #[cfg(test)]
    pub(crate) fn from_sources(sources: Vec<CertifiedSource>) -> Result<Self> {
        Self::from_parts(sources, Vec::new())
    }

    pub(crate) fn from_parts(
        mut sources: Vec<CertifiedSource>,
        mut removals: Vec<GenerationRemoval>,
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
            removals,
        };
        manifest.validate_contract()?;
        Ok(manifest)
    }

    pub fn generation_id(&self) -> Result<String> {
        Ok(sha256_hex(&serde_json::to_vec(self)?))
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
        for source in &self.sources {
            source.validate_contract()?;
            expected_documents = expected_documents
                .checked_add(source.counts().indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            expected_bytes = expected_bytes
                .checked_add(source.counts().certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
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
