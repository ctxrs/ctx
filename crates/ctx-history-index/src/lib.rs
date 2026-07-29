//! Atomic source-backed lexical generations.
//!
//! A Tantivy commit payload names an immutable manifest containing the exact
//! provider source revisions represented by that commit. The manifest is
//! durably written before Tantivy publishes `meta.json`, so readers observe
//! either the previous complete generation or the new complete generation.

mod durable_directory;
pub mod policy;
mod query;

pub use policy::{
    current_source_generation_policy, current_source_generation_policy_hash,
    EmbeddingGenerationPolicy, LexicalBodySelection, LexicalGenerationPolicy,
    LexicalIndexedBodyLimit, SemanticGenerationPolicy, SemanticHydratedContentFilter,
    SourceEventClass, SourceEventRole, SourceGenerationPolicy, StoredSourceContent,
    LEXICAL_INDEXED_BODY_LIMIT, LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION,
    SEMANTIC_CHUNK_OVERLAP_CHARS, SEMANTIC_CHUNK_TARGET_CHARS,
    SEMANTIC_EMBEDDING_CONTRACT_REVISION, SEMANTIC_EMBEDDING_DIMENSIONS, SEMANTIC_EMBEDDING_MODEL,
    SEMANTIC_EMBEDDING_MODEL_REVISION, SEMANTIC_EMBEDDING_NORMALIZATION, SEMANTIC_SOURCE_MAX_CHARS,
};
pub use query::{
    AgentScope, EventRecord, EventSearchCandidate, EventSearchFilters, ExcludedSessionTree,
    SemanticEligibility, SemanticEventCursor, SemanticEventPage, SessionRecord, SourceEventCursor,
    SourceEventPage, MAX_SEMANTIC_EVENT_PAGE_ITEMS, MAX_SOURCE_EVENT_PAGE_ITEMS,
};

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    ProjectionContractError, SourceKey, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, StableEntityKind, IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::{
    collector::Count,
    directory::{Directory, Lock},
    schema::{
        Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED,
        STRING,
    },
    DocAddress, Index, IndexMeta, IndexSettings, IndexWriter, ReloadPolicy, Searcher,
    TantivyDocument, Term,
};
use thiserror::Error;
use uuid::Uuid;

use durable_directory::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};

pub const GENERATION_MANIFEST_VERSION: u32 = 3;
pub const LEXICAL_SCHEMA_VERSION: u32 = LEXICAL_SCHEMA_REVISION;
pub const LEXICAL_ANALYZER_VERSION: u32 = LEXICAL_TOKENIZER_REVISION;

const MANIFEST_DIRECTORY: &str = "ctx-generations";
const COMMIT_PAYLOAD_VERSION: u32 = 1;
const INDEX_MEMORY_MIN_PER_THREAD: usize = 15_000_000;
const MAX_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;
const MAX_TOUCHED_FILES: usize = 4_096;

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
    SourceResolverContract(#[from] SourceResolverContractError),
    #[error(transparent)]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("the lexical index has no ctx generation payload")]
    MissingCommitPayload,
    #[error("unsupported commit payload version {0}")]
    UnsupportedCommitPayload(u32),
    #[error("unsupported generation manifest version {0}")]
    UnsupportedManifest(u32),
    #[error(
        "generation contract mismatch: identity {identity}, schema {schema}, analyzer {analyzer}"
    )]
    GenerationContractMismatch {
        identity: u16,
        schema: u32,
        analyzer: u32,
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
    #[error("document {0} does not carry an event identity")]
    InvalidEventIdentityKind(String),
    #[error("document {0} does not carry a session identity")]
    InvalidSessionIdentityKind(String),
    #[error("document identities do not belong to source {0}")]
    IdentitySourceMismatch(String),
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

#[derive(Debug, Clone)]
pub struct LexicalDocument {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub source: SourceKey,
    pub locator: SourceRecordLocator,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    /// Full policy-selected meaningful text. It is indexed but never stored.
    pub body: String,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

impl LexicalDocument {
    fn validate(&self) -> Result<Vec<u8>> {
        self.locator.validate_contract()?;
        if self.locator.source() != &self.source {
            return Err(IndexError::DocumentSourceNotActive);
        }
        let locator_bytes = serde_json::to_vec(&self.locator)?;
        if locator_bytes.len() > MAX_DOCUMENT_METADATA_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "native_locator",
                actual: locator_bytes.len(),
                maximum: MAX_DOCUMENT_METADATA_BYTES,
            });
        }
        validate_document_text("event_type", &self.event_type, MAX_DOCUMENT_METADATA_BYTES)?;
        if self.body.is_empty() {
            return Err(IndexError::EmptyDocumentField { field: "body" });
        }
        match LEXICAL_INDEXED_BODY_LIMIT {
            LexicalIndexedBodyLimit::ProviderValidatedFullText => {}
        }
        validate_optional_document_text(
            "provider_session_id",
            self.provider_session_id.as_deref(),
            MAX_DOCUMENT_METADATA_BYTES,
        )?;
        validate_optional_document_text(
            "branch",
            self.branch.as_deref(),
            MAX_DOCUMENT_METADATA_BYTES,
        )?;
        validate_optional_document_text(
            "source_path",
            self.source_path.as_deref(),
            MAX_DOCUMENT_METADATA_BYTES,
        )?;
        validate_document_text("agent_type", &self.agent_type, MAX_DOCUMENT_METADATA_BYTES)?;
        validate_optional_document_text("role", self.role.as_deref(), MAX_DOCUMENT_METADATA_BYTES)?;
        validate_optional_document_text(
            "workspace",
            self.workspace.as_deref(),
            MAX_DOCUMENT_METADATA_BYTES,
        )?;
        validate_optional_document_text("cwd", self.cwd.as_deref(), MAX_DOCUMENT_METADATA_BYTES)?;
        if self.touched_files.len() > MAX_TOUCHED_FILES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "touched_files",
                actual: self.touched_files.len(),
                maximum: MAX_TOUCHED_FILES,
            });
        }
        for path in &self.touched_files {
            validate_document_text("touched_file", path, MAX_DOCUMENT_METADATA_BYTES)?;
        }
        Ok(locator_bytes)
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

    fn validate_contract(&self) -> Result<()> {
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
    fn from_sources(sources: Vec<CertifiedSource>) -> Result<Self> {
        Self::from_parts(sources, Vec::new())
    }

    fn from_parts(
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

    fn validate_contract(&self) -> Result<()> {
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
struct CommitPayload {
    version: u32,
    generation_id: String,
}

#[derive(Debug, Clone)]
pub struct CommitReceipt {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum RevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

#[derive(Clone, Copy)]
struct Fields {
    event_id: Field,
    event_identity_digest: Field,
    event_identity: Field,
    event_id_high: Field,
    event_id_low: Field,
    session_id: Field,
    session_identity_digest: Field,
    session_identity: Field,
    parent_session_id: Field,
    parent_session_identity: Field,
    root_session_id: Field,
    root_session_identity: Field,
    source_key: Field,
    native_locator: Field,
    provider: Field,
    source_format: Field,
    provider_session_id: Field,
    branch: Field,
    source_path: Field,
    agent_type: Field,
    is_primary: Field,
    event_sequence: Field,
    occurred_at_unix_ms: Field,
    event_type: Field,
    role: Field,
    body_search: Field,
    workspace: Field,
    workspace_filter: Field,
    cwd: Field,
    touched_file: Field,
    touched_file_filter: Field,
}

struct PendingSource {
    source: SourceKey,
    mode: PendingSourceMode,
    staged_documents: u64,
    certificate: Option<CertifiedSource>,
}

// Append validation consults the base certificate throughout staged ingestion.
// Boxing it would add allocation and indirection without measured product benefit.
#[allow(clippy::large_enum_variant)]
enum PendingSourceMode {
    Replace,
    Append { base: CertifiedSource },
}

pub struct GenerationWriter {
    root: PathBuf,
    index: Index,
    writer: IndexWriter<TantivyDocument>,
    fields: Fields,
    base_manifest: Option<GenerationManifest>,
    base_searcher: Option<Searcher>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, GenerationRemoval>,
    source_identities: HashMap<Uuid, [u8; 32]>,
    checked_base_sessions: HashSet<Uuid>,
    staged_event_identities: HashMap<Uuid, [u8; 32]>,
    staged_session_identities: HashMap<Uuid, [u8; 32]>,
}

impl GenerationWriter {
    pub fn open(root: impl AsRef<Path>, options: WriterOptions) -> Result<Self> {
        let indexer_threads = options.indexer_threads.clamp(1, 8);
        let minimum = INDEX_MEMORY_MIN_PER_THREAD.saturating_mul(indexer_threads);
        if options.memory_bytes < minimum {
            return Err(IndexError::IndexMemoryTooSmall {
                actual: options.memory_bytes,
                minimum,
            });
        }
        let requested_root = root.as_ref().to_path_buf();
        fs::create_dir_all(&requested_root)?;
        let directory =
            DurableMmapDirectory::open(&requested_root).map_err(tantivy::TantivyError::from)?;
        let root = directory.root_path().to_path_buf();
        fs::create_dir_all(root.join(MANIFEST_DIRECTORY))?;
        // Index::create replaces any prior index state. Serialize the initial
        // exists/create decision under a ctx-owned lock so two first-run
        // daemons cannot both decide that `meta.json` is absent.
        let initialization_lock = Lock {
            filepath: PathBuf::from(".ctx-index-initialization.lock"),
            is_blocking: true,
        };
        let initialization_guard =
            directory
                .acquire_lock(&initialization_lock)
                .map_err(|error| {
                    tantivy::TantivyError::LockFailure(
                        error,
                        Some("failed to acquire ctx index initialization lock".to_owned()),
                    )
                })?;
        let index = if Index::exists(&directory).map_err(tantivy::TantivyError::from)? {
            Index::open(directory.clone())?
        } else {
            Index::create(
                directory.clone(),
                lexical_schema(),
                IndexSettings::default(),
            )?
        };
        drop(initialization_guard);
        let fields = fields_from_schema(&index.schema())?;
        validate_schema(&index.schema())?;
        // IndexWriter owns Tantivy's writer lock. Load the base generation only
        // after acquiring it so a concurrent writer cannot advance meta.json
        // between base capture and candidate construction.
        let writer = index
            .writer_with_num_threads::<TantivyDocument>(indexer_threads, options.memory_bytes)?;
        let base_metas = index.load_metas()?;
        let (base_manifest, base_searcher) = if base_metas.payload.is_some() {
            let manifest = load_manifest_for_metas(&root, &base_metas)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            let searcher = reader.searcher();
            if searcher_generation(&searcher) != meta_generation(&base_metas) {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            verify_searcher(&searcher, &manifest)?;
            (Some(manifest), Some(searcher))
        } else if base_metas.segments.is_empty() {
            (None, None)
        } else {
            return Err(IndexError::UnboundIndexState);
        };
        // No other writer can create candidates while this writer owns
        // Tantivy's lock. Reclaim interrupted ctx atomic writes by their exact
        // reserved names, then let Tantivy remove only managed files absent
        // from its active/pinned segment inventory.
        reclaim_abandoned_atomic_writes(&root)?;
        reclaim_abandoned_atomic_writes(&root.join(MANIFEST_DIRECTORY))?;
        let _ = writer.garbage_collect_files().wait()?;
        let mut source_identities = HashMap::new();
        if let Some(manifest) = &base_manifest {
            for source in &manifest.sources {
                register_compact_identity(
                    &mut source_identities,
                    source.observation().source().identity(),
                    "source",
                    false,
                )?;
            }
            for removal in &manifest.removals {
                register_compact_identity(
                    &mut source_identities,
                    removal.source().identity(),
                    "source",
                    false,
                )?;
            }
        }
        Ok(Self {
            root,
            index,
            writer,
            fields,
            base_manifest,
            base_searcher,
            pending: HashMap::new(),
            deletions: HashMap::new(),
            source_identities,
            checked_base_sessions: HashSet::new(),
            staged_event_identities: HashMap::new(),
            staged_session_identities: HashMap::new(),
        })
    }

    /// Returns the base generation captured after this writer acquired
    /// Tantivy's exclusive writer lock.
    pub fn base_manifest(&self) -> Option<&GenerationManifest> {
        self.base_manifest.as_ref()
    }

    /// Starts replacing every lexical document owned by `source`.
    ///
    /// Documents can then be submitted as they are parsed; no whole-source or
    /// whole-batch DTO is retained by this writer.
    pub fn begin_source(&mut self, source: SourceKey) -> Result<()> {
        register_compact_identity(
            &mut self.source_identities,
            source.identity(),
            "source",
            false,
        )?;
        let token = source_token(&source);
        if self.pending.contains_key(&token) {
            return Err(IndexError::DuplicateSource(source.identity().to_string()));
        }
        self.writer
            .delete_term(Term::from_field_text(self.fields.source_key, &token));
        self.deletions.remove(&source);
        self.pending.insert(
            token,
            PendingSource {
                source,
                mode: PendingSourceMode::Replace,
                staged_documents: 0,
                certificate: None,
            },
        );
        Ok(())
    }

    /// Starts an exact append from the frontier in the committed manifest.
    ///
    /// The provider must hash the entire previously certified prefix while it
    /// parses the delta and submit a matching [`CertifiedSourceAppend`].
    pub fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource> {
        register_compact_identity(
            &mut self.source_identities,
            source.identity(),
            "source",
            false,
        )?;
        let token = source_token(&source);
        if self.pending.contains_key(&token) {
            return Err(IndexError::DuplicateSource(source.identity().to_string()));
        }
        let base = self
            .base_manifest
            .as_ref()
            .and_then(|manifest| {
                manifest
                    .sources
                    .iter()
                    .find(|candidate| candidate.observation().source() == &source)
            })
            .cloned()
            .ok_or_else(|| IndexError::SourceNotAppendable(source.identity().to_string()))?;
        if base.frontier().is_none() || !base.observation().source().exact_descriptor_eq(&source) {
            return Err(IndexError::SourceNotAppendable(
                source.identity().to_string(),
            ));
        }
        self.deletions.remove(&source);
        self.pending.insert(
            token.clone(),
            PendingSource {
                source,
                mode: PendingSourceMode::Append { base },
                staged_documents: 0,
                certificate: None,
            },
        );
        let pending = self
            .pending
            .get(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        match &pending.mode {
            PendingSourceMode::Append { base } => Ok(base),
            PendingSourceMode::Replace => Err(IndexError::DocumentSourceNotActive),
        }
    }

    pub fn add_document(&mut self, document: LexicalDocument) -> Result<()> {
        let locator_bytes = document.validate()?;
        let event_identity_bytes = document.event_id.encode_canonical()?;
        let session_identity_bytes = document.session_id.encode_canonical()?;
        let root_session_identity_bytes = document.root_session_id.encode_canonical()?;
        let parent_session_identity_bytes = document
            .parent_session_id
            .map(StableEntityId::encode_canonical)
            .transpose()?;
        if document.event_id.entity_kind() != StableEntityKind::Event {
            return Err(IndexError::InvalidEventIdentityKind(
                document.event_id.to_string(),
            ));
        }
        if document.session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidSessionIdentityKind(
                document.session_id.to_string(),
            ));
        }
        for related_session_id in document
            .parent_session_id
            .into_iter()
            .chain(std::iter::once(document.root_session_id))
        {
            if related_session_id.entity_kind() != StableEntityKind::Session {
                return Err(IndexError::InvalidSessionIdentityKind(
                    related_session_id.to_string(),
                ));
            }
        }
        let source_digest = document.source.identity().digest();
        let source_descriptor_digest = document.source.exact_descriptor_digest();
        if document.event_id.source_digest() != source_digest
            || document.session_id.source_digest() != source_digest
            || document.event_id.source_descriptor_digest() != source_descriptor_digest
            || document.session_id.source_descriptor_digest() != source_descriptor_digest
        {
            return Err(IndexError::IdentitySourceMismatch(
                document.source.identity().to_string(),
            ));
        }
        let token = source_token(&document.source);
        let pending_source = &self
            .pending
            .get(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        if !pending_source.source.exact_descriptor_eq(&document.source) {
            return Err(IndexError::DocumentSourceNotActive);
        }
        let is_append = matches!(&pending_source.mode, PendingSourceMode::Append { .. });
        if let Some(base_searcher) = &self.base_searcher {
            validate_event_identity_against_base(
                base_searcher,
                self.fields,
                document.event_id,
                &token,
                !is_append,
            )?;
            if self
                .checked_base_sessions
                .insert(document.session_id.as_uuid())
            {
                validate_session_identity_against_base(
                    base_searcher,
                    self.fields,
                    document.session_id,
                    &token,
                )?;
            }
            for related_session_id in document
                .parent_session_id
                .into_iter()
                .chain(std::iter::once(document.root_session_id))
            {
                if related_session_id != document.session_id
                    && self
                        .checked_base_sessions
                        .insert(related_session_id.as_uuid())
                {
                    validate_referenced_session_identity_against_base(
                        base_searcher,
                        self.fields,
                        related_session_id,
                    )?;
                }
            }
        } else if is_append {
            return Err(IndexError::AppendBaseMismatch);
        }
        register_session_identity(&mut self.staged_session_identities, document.session_id)?;
        if let Some(parent_session_id) = document.parent_session_id {
            register_session_identity(&mut self.staged_session_identities, parent_session_id)?;
        }
        register_session_identity(
            &mut self.staged_session_identities,
            document.root_session_id,
        )?;
        register_event_identity(&mut self.staged_event_identities, document.event_id)?;
        let mut target = TantivyDocument::default();
        target.add_text(self.fields.event_id, document.event_id.to_string());
        target.add_text(
            self.fields.event_identity_digest,
            hex(&document.event_id.digest()),
        );
        target.add_bytes(self.fields.event_identity, &event_identity_bytes);
        let event_uuid = document.event_id.as_uuid().as_u128();
        target.add_u64(self.fields.event_id_high, (event_uuid >> 64) as u64);
        target.add_u64(self.fields.event_id_low, event_uuid as u64);
        target.add_text(self.fields.session_id, document.session_id.to_string());
        target.add_text(
            self.fields.session_identity_digest,
            hex(&document.session_id.digest()),
        );
        target.add_bytes(self.fields.session_identity, &session_identity_bytes);
        if let (Some(parent_session_id), Some(parent_session_identity_bytes)) = (
            document.parent_session_id,
            parent_session_identity_bytes.as_ref(),
        ) {
            target.add_text(self.fields.parent_session_id, parent_session_id.to_string());
            target.add_bytes(
                self.fields.parent_session_identity,
                parent_session_identity_bytes,
            );
        }
        target.add_text(
            self.fields.root_session_id,
            document.root_session_id.to_string(),
        );
        target.add_bytes(
            self.fields.root_session_identity,
            &root_session_identity_bytes,
        );
        target.add_text(self.fields.source_key, &token);
        target.add_bytes(self.fields.native_locator, &locator_bytes);
        target.add_text(self.fields.provider, document.source.provider());
        target.add_text(self.fields.source_format, document.source.source_format());
        if let Some(provider_session_id) = document.provider_session_id {
            target.add_text(self.fields.provider_session_id, provider_session_id);
        }
        if let Some(branch) = document.branch {
            target.add_text(self.fields.branch, branch);
        }
        if let Some(source_path) = document.source_path {
            target.add_text(self.fields.workspace_filter, source_path.to_lowercase());
            target.add_text(self.fields.source_path, source_path);
        }
        target.add_text(self.fields.agent_type, document.agent_type);
        target.add_u64(self.fields.is_primary, u64::from(document.is_primary));
        target.add_u64(self.fields.event_sequence, document.event_sequence);
        if let Some(occurred_at_unix_ms) = document.occurred_at_unix_ms {
            target.add_i64(self.fields.occurred_at_unix_ms, occurred_at_unix_ms);
        }
        target.add_text(self.fields.event_type, document.event_type);
        if let Some(role) = document.role {
            target.add_text(self.fields.role, role);
        }
        target.add_text(self.fields.body_search, document.body);
        if let Some(workspace) = document.workspace {
            target.add_text(self.fields.workspace_filter, workspace.to_lowercase());
            target.add_text(self.fields.workspace, workspace);
        }
        if let Some(cwd) = document.cwd {
            target.add_text(self.fields.workspace_filter, cwd.to_lowercase());
            target.add_text(self.fields.cwd, cwd);
        }
        for touched_file in document.touched_files {
            target.add_text(self.fields.touched_file_filter, touched_file.to_lowercase());
            target.add_text(self.fields.touched_file, touched_file);
        }
        self.writer.add_document(target)?;
        let pending = self
            .pending
            .get_mut(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        pending.staged_documents = pending
            .staged_documents
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    pub fn certify_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        let token = source_token(certificate.observation().source());
        let pending = self.pending.get_mut(&token).ok_or_else(|| {
            IndexError::SourceNotStarted(certificate.observation().source().identity().to_string())
        })?;
        if !pending
            .source
            .exact_descriptor_eq(certificate.observation().source())
        {
            return Err(IndexError::SourceCertificateMismatch);
        }
        if !matches!(&pending.mode, PendingSourceMode::Replace) {
            return Err(IndexError::AppendBaseMismatch);
        }
        let certified = certificate.counts().indexed_documents;
        if certified != pending.staged_documents {
            return Err(IndexError::SourceDocumentCountMismatch {
                source_id: pending.source.identity().to_string(),
                certified,
                staged: pending.staged_documents,
            });
        }
        pending.certificate = Some(certificate);
        Ok(())
    }

    pub fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<()> {
        let token = source_token(append.current().observation().source());
        let pending = self.pending.get_mut(&token).ok_or_else(|| {
            IndexError::SourceNotStarted(
                append
                    .current()
                    .observation()
                    .source()
                    .identity()
                    .to_string(),
            )
        })?;
        let PendingSourceMode::Append { base } = &pending.mode else {
            return Err(IndexError::AppendBaseMismatch);
        };
        if base != append.base()
            || !pending
                .source
                .exact_descriptor_eq(append.current().observation().source())
        {
            return Err(IndexError::AppendBaseMismatch);
        }
        let certified_delta = append
            .current()
            .counts()
            .indexed_documents
            .checked_sub(base.counts().indexed_documents)
            .ok_or(IndexError::AppendBaseMismatch)?;
        if certified_delta != pending.staged_documents {
            return Err(IndexError::SourceDocumentCountMismatch {
                source_id: pending.source.identity().to_string(),
                certified: certified_delta,
                staged: pending.staged_documents,
            });
        }
        pending.certificate = Some(append.into_current());
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        proof: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        let removal = GenerationRemoval::new(proof, inventory)?;
        let source = removal.source();
        register_compact_identity(
            &mut self.source_identities,
            source.identity(),
            "source",
            false,
        )?;
        let token = source_token(source);
        if self.pending.contains_key(&token) {
            return Err(IndexError::DuplicateSource(source.identity().to_string()));
        }
        self.writer
            .delete_term(Term::from_field_text(self.fields.source_key, &token));
        self.deletions.insert(source.clone(), removal);
        Ok(())
    }

    /// Publishes one atomic lexical generation.
    ///
    /// `revalidate` runs after Tantivy has flushed all staged indexing workers
    /// and immediately before the immutable manifest and `meta.json` commit.
    pub fn commit<F>(mut self, mut revalidate: F) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
    {
        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }

        let manifest = self.next_manifest()?;
        let previous_generation_id = self
            .base_manifest
            .as_ref()
            .map(GenerationManifest::generation_id)
            .transpose()?;
        let root = self.root.clone();
        let mut prepared = self.writer.prepare_commit()?;
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            if !revalidate(RevalidationTarget::Source(certificate)) {
                let source = pending.source.identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }
        for removal in self.deletions.values() {
            if !revalidate(RevalidationTarget::Deletion(removal.deletion())) {
                let source = removal.source().identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }

        let generation_id = manifest.generation_id()?;
        if let Err(error) = write_manifest(&root, &generation_id, &manifest) {
            let _ = prepared.abort();
            return Err(error);
        }
        let payload = serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: generation_id.clone(),
        })?;
        prepared.set_payload(&payload);
        let commit_result = prepared.commit();
        if let Err(error) = self.writer.wait_merging_threads() {
            return Err(classify_publication_failure(
                &self.index,
                &generation_id,
                previous_generation_id.as_deref(),
                "merge completion",
                error,
            ));
        }
        let opstamp = match commit_result {
            Ok(opstamp) => opstamp,
            Err(error) => reconcile_commit_error(
                &self.index,
                &root,
                &generation_id,
                previous_generation_id.as_deref(),
                error,
            )?,
        };
        if let Err(error) = sync_directory(&root) {
            return Err(IndexError::CommittedGenerationNeedsRecovery {
                generation_id,
                stage: "root durability",
                detail: error.to_string(),
            });
        }
        let verified = VerifiedIndex::open(&root).map_err(|error| {
            IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.clone(),
                stage: "generation verification",
                detail: error.to_string(),
            }
        })?;
        if verified.generation_id() != generation_id {
            return Err(IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.clone(),
                stage: "generation verification",
                detail: format!(
                    "visible generation changed to {} before the commit receipt",
                    verified.generation_id()
                ),
            });
        }

        Ok(CommitReceipt {
            generation_id,
            opstamp,
            indexed_documents: manifest.indexed_documents,
            certified_sources: manifest.sources.len(),
            certified_source_bytes: manifest.certified_source_bytes,
        })
    }

    fn next_manifest(&self) -> Result<GenerationManifest> {
        let mut sources = HashMap::<SourceKey, CertifiedSource>::new();
        let mut removals = HashMap::<SourceKey, GenerationRemoval>::new();
        if let Some(base) = &self.base_manifest {
            for source in &base.sources {
                sources.insert(source.observation().source().clone(), source.clone());
            }
            for removal in &base.removals {
                removals.insert(removal.source().clone(), removal.clone());
            }
        }
        for (source, removal) in &self.deletions {
            sources.remove(source);
            removals.insert(source.clone(), removal.clone());
        }
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            sources.insert(pending.source.clone(), certificate.clone());
            removals.remove(&pending.source);
        }
        GenerationManifest::from_parts(
            sources.into_values().collect(),
            removals.into_values().collect(),
        )
    }
}

/// A verified reader pinned to one immutable lexical generation.
pub struct VerifiedIndex {
    searcher: Searcher,
    manifest: GenerationManifest,
    generation_id: String,
    semantic_eligible_event_count: OnceLock<u64>,
    custom_source_identity_events: OnceLock<Vec<(Uuid, String, String)>>,
}

impl VerifiedIndex {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = directory.root_path().to_path_buf();
        let index = Index::open(directory)?;
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let manifest = load_manifest_for_metas(&root, &metas)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        verify_searcher(&searcher, &manifest)?;
        let generation_id = manifest.generation_id()?;
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligible_event_count: OnceLock::new(),
            custom_source_identity_events: OnceLock::new(),
        })
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    pub fn document_count(&self) -> u64 {
        self.searcher.num_docs()
    }

    pub fn validate_checksums(&self) -> Result<HashSet<PathBuf>> {
        Ok(self.searcher.index().validate_checksum()?)
    }

    #[cfg(test)]
    fn count_term(&self, term_text: &str) -> Result<usize> {
        use tantivy::query::TermQuery;

        let body = required_field(self.searcher.schema(), "body_search")?;
        let query = TermQuery::new(
            Term::from_field_text(body, term_text),
            IndexRecordOption::Basic,
        );
        Ok(self.searcher.search(&query, &Count)?)
    }
}

fn load_manifest_for_metas(root: &Path, metas: &IndexMeta) -> Result<GenerationManifest> {
    let payload = metas
        .payload
        .as_ref()
        .ok_or(IndexError::MissingCommitPayload)?;
    let payload: CommitPayload = serde_json::from_str(payload)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(&payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let path = manifest_path(root, &payload.generation_id);
    let bytes = fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => IndexError::MissingManifest(payload.generation_id.clone()),
        _ => IndexError::Io(error),
    })?;
    let actual = sha256_hex(&bytes);
    if actual != payload.generation_id {
        return Err(IndexError::ManifestDigestMismatch {
            expected: payload.generation_id,
            actual,
        });
    }
    let manifest: GenerationManifest = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&manifest)? != bytes {
        return Err(IndexError::NonCanonicalManifest);
    }
    if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
        return Err(IndexError::UnsupportedManifest(manifest.manifest_version));
    }
    if manifest.identity_version != IDENTITY_VERSION
        || manifest.lexical_schema_version != LEXICAL_SCHEMA_VERSION
        || manifest.lexical_analyzer_version != LEXICAL_ANALYZER_VERSION
    {
        return Err(IndexError::GenerationContractMismatch {
            identity: manifest.identity_version,
            schema: manifest.lexical_schema_version,
            analyzer: manifest.lexical_analyzer_version,
        });
    }
    let expected_policy_hash = current_source_generation_policy_hash()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash,
        });
    }
    manifest.validate_contract()?;
    Ok(manifest)
}

fn reconcile_commit_error(
    index: &Index,
    root: &Path,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    commit_error: tantivy::TantivyError,
) -> Result<u64> {
    let metas = index.load_metas().map_err(|reconcile_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; reopening meta.json failed: {reconcile_error}"),
        }
    })?;
    let visible_generation = payload_generation_id(&metas).map_err(|payload_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; visible payload is invalid: {payload_error}"),
        }
    })?;
    if visible_generation.as_deref() == Some(expected_generation_id) {
        let verification = (|| -> Result<u64> {
            let manifest = load_manifest_for_metas(root, &metas)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            let searcher = reader.searcher();
            if searcher_generation(&searcher) != meta_generation(&metas) {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            verify_searcher(&searcher, &manifest)?;
            Ok(metas.opstamp)
        })();
        return verification.map_err(|verification_error| {
            IndexError::CommittedGenerationNeedsRecovery {
                generation_id: expected_generation_id.to_owned(),
                stage: "commit reconciliation",
                detail: format!(
                    "{commit_error}; new payload is visible but verification failed: \
                     {verification_error}"
                ),
            }
        });
    }
    if visible_generation.as_deref() == previous_generation_id
        || (previous_generation_id.is_none()
            && visible_generation.is_none()
            && metas.segments.is_empty())
    {
        return Err(IndexError::Tantivy(commit_error));
    }
    Err(IndexError::CommittedGenerationNeedsRecovery {
        generation_id: expected_generation_id.to_owned(),
        stage: "commit reconciliation",
        detail: format!(
            "{commit_error}; expected old generation {:?} or new generation, found {:?}",
            previous_generation_id, visible_generation
        ),
    })
}

fn payload_generation_id(metas: &IndexMeta) -> Result<Option<String>> {
    let Some(payload) = metas.payload.as_deref() else {
        return Ok(None);
    };
    let payload: CommitPayload = serde_json::from_str(payload)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(&payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    Ok(Some(payload.generation_id))
}

fn classify_publication_failure(
    index: &Index,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    stage: &'static str,
    error: tantivy::TantivyError,
) -> IndexError {
    let visible_generation = index
        .load_metas()
        .map_err(IndexError::from)
        .and_then(|metas| payload_generation_id(&metas));
    match visible_generation {
        Ok(visible) if visible.as_deref() == previous_generation_id => IndexError::Tantivy(error),
        Ok(None) if previous_generation_id.is_none() => IndexError::Tantivy(error),
        Ok(visible) => IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage,
            detail: format!("{error}; visible generation is {visible:?}"),
        },
        Err(reconcile_error) => IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage,
            detail: format!("{error}; visibility reconciliation failed: {reconcile_error}"),
        },
    }
}

fn write_manifest(root: &Path, generation_id: &str, manifest: &GenerationManifest) -> Result<()> {
    let bytes = serde_json::to_vec(manifest)?;
    let actual = sha256_hex(&bytes);
    if actual != generation_id {
        return Err(IndexError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    let directory = root.join(MANIFEST_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let path = manifest_path(root, generation_id);
    if path.is_file() {
        let existing = fs::read(&path)?;
        if existing == bytes {
            // A prior process may have died after publishing this immutable
            // filename but before synchronizing either its contents or its
            // directory entry. Re-fence both before meta.json can name it.
            File::open(&path)?.sync_all()?;
            sync_directory(&directory)?;
            return Ok(());
        }
        let quarantine = directory.join(format!(
            ".{generation_id}.corrupt-{}",
            Uuid::now_v7().simple()
        ));
        fs::rename(&path, quarantine)?;
        sync_directory(&directory)?;
    }

    // The writer lock serializes manifest publication, so no-clobber hard-link
    // tricks are unnecessary and exclude filesystems without hard-link
    // support. Reuse the same durable atomic replacement primitive as
    // Tantivy's meta publication.
    let durable_directory =
        DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    durable_directory.atomic_write(&relative_path, &bytes)?;
    Ok(())
}

fn verify_searcher(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    use tantivy::query::TermQuery;

    verify_total_document_count(searcher, manifest.indexed_documents)?;
    let source_field = required_field(searcher.schema(), "source_key")?;
    for source in &manifest.sources {
        let source_id = source_token(source.observation().source());
        let query = TermQuery::new(
            Term::from_field_text(source_field, &source_id),
            IndexRecordOption::Basic,
        );
        let actual = searcher.search(&query, &Count)? as u64;
        let expected = source.counts().indexed_documents;
        if actual != expected {
            return Err(IndexError::SourceCountMismatch {
                source_id,
                manifest: expected,
                index: actual,
            });
        }
    }
    verify_generation_identities(searcher)?;
    Ok(())
}

fn verify_generation_identities(searcher: &Searcher) -> Result<()> {
    let fields = fields_from_schema(searcher.schema())?;
    let mut event_identities = HashMap::new();
    let mut session_identities = HashMap::new();
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment.max_doc() {
            if segment.is_deleted(doc_id) {
                continue;
            }
            let event = query::stored_event_record(
                searcher,
                DocAddress::new(segment_ord as u32, doc_id),
                fields,
            )?;
            register_event_identity(&mut event_identities, event.event_id)?;
            let owner = source_token(event.locator.source());
            register_generation_session_identity(
                &mut session_identities,
                event.session_id,
                Some(&owner),
            )?;
            if let Some(parent_session_id) = event.parent_session_id {
                register_generation_session_identity(
                    &mut session_identities,
                    parent_session_id,
                    None,
                )?;
            }
            register_generation_session_identity(
                &mut session_identities,
                event.root_session_id,
                None,
            )?;
        }
    }
    Ok(())
}

fn register_generation_session_identity(
    identities: &mut HashMap<Uuid, ([u8; 32], Option<String>)>,
    identity: StableEntityId,
    owner: Option<&str>,
) -> Result<()> {
    let uuid = identity.as_uuid();
    let digest = identity.digest();
    match identities.entry(uuid) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert((digest, owner.map(str::to_owned)));
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(mut entry) if entry.get().0 == digest => {
            let registered_owner = &mut entry.get_mut().1;
            match (registered_owner.as_deref(), owner) {
                (Some(existing), Some(candidate)) if existing != candidate => {
                    Err(IndexError::DuplicateSessionIdentity(uuid.to_string()))
                }
                (None, Some(candidate)) => {
                    *registered_owner = Some(candidate.to_owned());
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            Err(IndexError::CompactIdentityCollision {
                kind: "session",
                uuid,
                existing_digest: hex(&entry.get().0),
                new_digest: hex(&digest),
            })
        }
    }
}

fn verify_total_document_count(searcher: &Searcher, expected: u64) -> Result<()> {
    let actual = searcher.search(&tantivy::query::AllQuery, &Count)? as u64;
    if actual != expected {
        return Err(IndexError::DocumentCountMismatch {
            manifest: expected,
            index: actual,
        });
    }
    Ok(())
}

fn validate_schema(schema: &Schema) -> Result<()> {
    if serde_json::to_vec(schema)? != serde_json::to_vec(&lexical_schema())? {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

fn meta_generation(metas: &IndexMeta) -> BTreeMap<String, Option<u64>> {
    metas
        .segments
        .iter()
        .map(|segment| (segment.id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

fn searcher_generation(searcher: &Searcher) -> BTreeMap<String, Option<u64>> {
    searcher
        .segment_readers()
        .iter()
        .map(|segment| (segment.segment_id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

fn lexical_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("event_id", STRING | STORED);
    builder.add_text_field("event_identity_digest", STRING | STORED);
    builder.add_bytes_field("event_identity", STORED);
    builder.add_u64_field("event_id_high", FAST);
    builder.add_u64_field("event_id_low", FAST);
    builder.add_text_field("session_id", STRING | STORED);
    builder.add_text_field("session_identity_digest", STRING | STORED);
    builder.add_bytes_field("session_identity", STORED);
    builder.add_text_field("parent_session_id", STRING | STORED);
    builder.add_bytes_field("parent_session_identity", STORED);
    builder.add_text_field("root_session_id", STRING | STORED);
    builder.add_bytes_field("root_session_identity", STORED);
    builder.add_text_field("source_key", STRING | STORED);
    builder.add_bytes_field("native_locator", STORED);
    builder.add_text_field("provider", STRING | STORED);
    builder.add_text_field("source_format", STRING | STORED);
    builder.add_text_field("provider_session_id", STRING | STORED);
    builder.add_text_field("branch", STRING | STORED);
    builder.add_text_field("source_path", STORED);
    builder.add_text_field("agent_type", STRING | STORED);
    builder.add_u64_field("is_primary", STORED | INDEXED);
    builder.add_u64_field("event_sequence", FAST | STORED | INDEXED);
    builder.add_i64_field("occurred_at_unix_ms", FAST | STORED | INDEXED);
    builder.add_text_field("event_type", STRING | STORED);
    builder.add_text_field("role", STRING | STORED);
    let body_indexing = TextFieldIndexing::default()
        .set_tokenizer("default")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    builder.add_text_field(
        "body_search",
        TextOptions::default().set_indexing_options(body_indexing),
    );
    builder.add_text_field("workspace", STRING | STORED);
    builder.add_text_field("workspace_filter", STRING);
    builder.add_text_field("cwd", STRING | STORED);
    builder.add_text_field("touched_file", STRING | STORED);
    builder.add_text_field("touched_file_filter", STRING);
    builder.build()
}

fn fields_from_schema(schema: &Schema) -> Result<Fields> {
    Ok(Fields {
        event_id: required_field(schema, "event_id")?,
        event_identity_digest: required_field(schema, "event_identity_digest")?,
        event_identity: required_field(schema, "event_identity")?,
        event_id_high: required_field(schema, "event_id_high")?,
        event_id_low: required_field(schema, "event_id_low")?,
        session_id: required_field(schema, "session_id")?,
        session_identity_digest: required_field(schema, "session_identity_digest")?,
        session_identity: required_field(schema, "session_identity")?,
        parent_session_id: required_field(schema, "parent_session_id")?,
        parent_session_identity: required_field(schema, "parent_session_identity")?,
        root_session_id: required_field(schema, "root_session_id")?,
        root_session_identity: required_field(schema, "root_session_identity")?,
        source_key: required_field(schema, "source_key")?,
        native_locator: required_field(schema, "native_locator")?,
        provider: required_field(schema, "provider")?,
        source_format: required_field(schema, "source_format")?,
        provider_session_id: required_field(schema, "provider_session_id")?,
        branch: required_field(schema, "branch")?,
        source_path: required_field(schema, "source_path")?,
        agent_type: required_field(schema, "agent_type")?,
        is_primary: required_field(schema, "is_primary")?,
        event_sequence: required_field(schema, "event_sequence")?,
        occurred_at_unix_ms: required_field(schema, "occurred_at_unix_ms")?,
        event_type: required_field(schema, "event_type")?,
        role: required_field(schema, "role")?,
        body_search: required_field(schema, "body_search")?,
        workspace: required_field(schema, "workspace")?,
        workspace_filter: required_field(schema, "workspace_filter")?,
        cwd: required_field(schema, "cwd")?,
        touched_file: required_field(schema, "touched_file")?,
        touched_file_filter: required_field(schema, "touched_file_filter")?,
    })
}

fn required_field(schema: &Schema, name: &'static str) -> Result<Field> {
    schema
        .get_field(name)
        .map_err(|_| IndexError::MissingSchemaField(name))
}

fn source_token(source: &SourceKey) -> String {
    hex(&source.identity().digest())
}

fn source_sort_key(source: &SourceKey) -> [u8; 32] {
    source.identity().digest()
}

fn register_session_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
) -> Result<()> {
    register_compact_identity(identities, identity, "session", false)
}

fn validate_event_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source_token: &str,
    allow_replacement_from_same_source: bool,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.event_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_digest = document
            .get_first(fields.event_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "event_identity_digest",
            })?;
        let existing_source = document
            .get_first(fields.source_key)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "source_key",
            })?;
        if allow_replacement_from_same_source && existing_source == current_source_token {
            continue;
        }
        if existing_digest == new_digest {
            return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
        }
        return Err(IndexError::CompactIdentityCollision {
            kind: "event",
            uuid,
            existing_digest: existing_digest.to_owned(),
            new_digest,
        });
    }
    Ok(())
}

fn validate_session_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source_token: &str,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.session_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_source = document
            .get_first(fields.source_key)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "source_key",
            })?;
        if existing_source == current_source_token {
            continue;
        }
        let existing_digest = document
            .get_first(fields.session_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "session_identity_digest",
            })?;
        if existing_digest == new_digest {
            return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
        }
        return Err(IndexError::CompactIdentityCollision {
            kind: "session",
            uuid,
            existing_digest: existing_digest.to_owned(),
            new_digest,
        });
    }
    Ok(())
}

fn validate_referenced_session_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.session_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_digest = document
            .get_first(fields.session_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "session_identity_digest",
            })?;
        if existing_digest != new_digest {
            return Err(IndexError::CompactIdentityCollision {
                kind: "session",
                uuid,
                existing_digest: existing_digest.to_owned(),
                new_digest,
            });
        }
    }
    Ok(())
}

fn register_event_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
) -> Result<()> {
    register_compact_identity(identities, identity, "event", true)
}

fn register_compact_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
    kind: &'static str,
    duplicate_is_error: bool,
) -> Result<()> {
    let uuid = identity.as_uuid();
    let digest = identity.digest();
    match identities.entry(uuid) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(digest);
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(entry) if *entry.get() == digest => {
            if duplicate_is_error {
                Err(IndexError::DuplicateEventIdentity(uuid.to_string()))
            } else {
                Ok(())
            }
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            Err(IndexError::CompactIdentityCollision {
                kind,
                uuid,
                existing_digest: hex(entry.get()),
                new_digest: hex(&digest),
            })
        }
    }
}

fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn is_generation_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex(value: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn manifest_path(root: &Path, generation_id: &str) -> PathBuf {
    root.join(MANIFEST_DIRECTORY)
        .join(format!("{generation_id}.json"))
}

#[cfg(not(windows))]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn validate_document_text(field: &'static str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() {
        return Err(IndexError::EmptyDocumentField { field });
    }
    if value.len() > maximum {
        return Err(IndexError::DocumentFieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn validate_optional_document_text(
    field: &'static str,
    value: Option<&str>,
    maximum: usize,
) -> Result<()> {
    if let Some(value) = value {
        validate_document_text(field, value, maximum)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CertifiedSourceInventory, EventIdentityInput,
        LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
        SourceInventoryObservation, SourceObservation, TypedKey,
    };
    use tantivy::{
        collector::DocSetCollector, indexer::NoMergePolicy, query::AllQuery,
        schema::Value as TantivyValue,
    };
    use tempfile::tempdir;

    use super::*;

    fn source(name: &str) -> SourceKey {
        source_for_provider("codex", "codex_session_jsonl", name)
    }

    fn source_for_provider(provider: &str, source_format: &str, name: &str) -> SourceKey {
        SourceKey::derive(
            provider,
            source_format,
            "session",
            1,
            SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
        )
        .unwrap()
    }

    fn certificate(source: &SourceKey, revision: u8, documents: u64) -> CertifiedSource {
        let opening =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
        CertifiedSource::certify(
            opening.clone(),
            opening,
            "codex-parser-v1",
            [revision; 32],
            ScannedSourceCounts {
                complete_records: documents,
                retained_records: documents,
                indexed_documents: documents,
                certified_bytes: documents * 10,
                ..ScannedSourceCounts::default()
            },
        )
        .unwrap()
    }

    fn appendable_certificate(
        source: &SourceKey,
        revision: u8,
        documents: u64,
        bytes: u64,
    ) -> CertifiedSource {
        let observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
        CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            "codex-parser-v1",
            [revision; 32],
            ScannedSourceCounts {
                complete_records: documents,
                retained_records: documents,
                indexed_documents: documents,
                certified_bytes: bytes,
                ..ScannedSourceCounts::default()
            },
            Some(
                SourceFrontier::new(
                    "jsonl-byte-offset",
                    TypedKey::U64(bytes),
                    bytes,
                    [revision; 32],
                )
                .unwrap(),
            ),
        )
        .unwrap()
    }

    fn deletion_evidence(
        source: &SourceKey,
        revision: u8,
    ) -> (CertifiedSourceDeletion, CertifiedSourceInventory) {
        let inventory = SourceInventoryObservation::new(
            source.provider(),
            "provider-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![revision],
        )
        .unwrap();
        let inventory =
            CertifiedSourceInventory::certify(inventory.clone(), inventory, "discovery-v1", vec![])
                .unwrap();
        let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
        (deletion, inventory)
    }

    fn document(source: &SourceKey, sequence: u64, body: &str) -> LexicalDocument {
        document_for_session(source, "session", sequence, body)
    }

    fn document_for_session(
        source: &SourceKey,
        native_session_id: &str,
        sequence: u64,
        body: &str,
    ) -> LexicalDocument {
        let native_session_coordinate = TypedKey::utf8(native_session_id).unwrap();
        let session_key =
            NativeSessionKey::native_id("session", native_session_coordinate.clone()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id(
            "message",
            TypedKey::utf8(format!("event-{sequence}")).unwrap(),
        )
        .unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator: SourceRecordLocator::new(
                source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: sequence * 100,
                    byte_length: 100,
                    physical_ordinal: sequence,
                    native_session_key: Some(native_session_coordinate),
                    native_event_key: Some(TypedKey::U64(sequence)),
                },
                LocatorRevisionPolicy::StableRecordEvidence,
                None,
                [sequence as u8; 32],
            )
            .unwrap(),
            provider_session_id: Some(native_session_id.to_owned()),
            branch: Some("main".to_owned()),
            source_path: Some(format!("/history/{native_session_id}.jsonl")),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
            event_type: "message".to_owned(),
            role: Some("user".to_owned()),
            body: body.to_owned(),
            workspace: Some("ctx".to_owned()),
            cwd: Some("/work/ctx".to_owned()),
            touched_files: vec!["src/lib.rs".to_owned()],
        }
    }

    fn filtered_session_ids(index: &VerifiedIndex, filters: EventSearchFilters) -> Vec<Uuid> {
        sorted_uuids(
            index
                .search_event_candidates_with_filters("shared needle", &filters, 10)
                .unwrap()
                .into_iter()
                .map(|candidate| candidate.event.session_id.as_uuid())
                .collect(),
        )
    }

    fn sorted_uuids(mut ids: Vec<Uuid>) -> Vec<Uuid> {
        ids.sort();
        ids
    }

    fn collect_source_pages(
        index: &VerifiedIndex,
        source: &SourceKey,
        limit: usize,
    ) -> Vec<EventRecord> {
        let mut cursor = None;
        let mut records = Vec::new();
        loop {
            let page = index
                .source_event_page(source, cursor.as_ref(), limit)
                .unwrap();
            records.extend(page.items);
            if page.terminal {
                assert!(page.next_cursor.is_none());
                return records;
            }
            cursor = Some(page.next_cursor.unwrap());
        }
    }

    fn publish_unchecked_generation(
        root: &Path,
        index: &Index,
        manifest: GenerationManifest,
        delete_sources: &[SourceKey],
        documents: Vec<TantivyDocument>,
    ) {
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
            .unwrap();
        let source_key = required_field(&index.schema(), "source_key").unwrap();
        for source in delete_sources {
            writer.delete_term(Term::from_field_text(source_key, &source_token(source)));
        }
        for document in documents {
            writer.add_document(document).unwrap();
        }
        let generation_id = manifest.generation_id().unwrap();
        write_manifest(root, &generation_id, &manifest).unwrap();
        let mut prepared = writer.prepare_commit().unwrap();
        prepared.set_payload(
            &serde_json::to_string(&CommitPayload {
                version: COMMIT_PAYLOAD_VERSION,
                generation_id,
            })
            .unwrap(),
        );
        prepared.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        sync_directory(root).unwrap();
    }

    #[test]
    fn commit_binds_manifest_and_searchable_documents() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_document(document(&source, 1, "atomic generation"))
            .unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        let receipt = writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(index.generation_id(), receipt.generation_id);
        assert_eq!(index.manifest().indexed_documents, 1);
        assert_eq!(index.count_term("atomic").unwrap(), 1);
    }

    #[test]
    fn writer_exposes_the_base_manifest_captured_under_its_lock() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        assert!(first.base_manifest().is_none());
        first.begin_source(source.clone()).unwrap();
        first.add_document(document(&source, 1, "base")).unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        let receipt = first.commit(|_| true).unwrap();

        let writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let base = writer.base_manifest().unwrap();
        assert_eq!(base.generation_id().unwrap(), receipt.generation_id);
        assert_eq!(base.sources.len(), 1);
        assert_eq!(base.sources[0].observation().source(), &source);

        let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
            Ok(_) => panic!("competing writer unexpectedly acquired the writer lock"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IndexError::Tantivy(tantivy::TantivyError::LockFailure(_, _))
        ));
        assert_eq!(
            writer.base_manifest().unwrap().generation_id().unwrap(),
            receipt.generation_id
        );
    }

    #[test]
    fn writer_rejects_a_nonempty_payloadless_index() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first.add_document(document(&source, 1, "body")).unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        first.commit(|_| true).unwrap();

        let directory = DurableMmapDirectory::open(temp.path()).unwrap();
        let index = Index::open(directory.clone()).unwrap();
        let mut metas = index.load_metas().unwrap();
        metas.payload = None;
        directory
            .atomic_write(Path::new("meta.json"), &serde_json::to_vec(&metas).unwrap())
            .unwrap();

        let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
            Ok(_) => panic!("nonempty payloadless index unexpectedly opened for writing"),
            Err(error) => error,
        };
        assert!(matches!(error, IndexError::UnboundIndexState));
    }

    #[test]
    fn stored_document_identities_use_canonical_fixed_bytes() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let expected = document(&source, 1, "body");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(expected.clone()).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        let fields = fields_from_schema(index.searcher.schema()).unwrap();
        let address = index
            .searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stored: TantivyDocument = index.searcher.doc(address).unwrap();
        let event_bytes = stored
            .get_first(fields.event_identity)
            .and_then(|value| value.as_bytes())
            .unwrap();
        let session_bytes = stored
            .get_first(fields.session_identity)
            .and_then(|value| value.as_bytes())
            .unwrap();

        assert_eq!(event_bytes.len(), StableEntityId::CANONICAL_LEN);
        assert_eq!(
            event_bytes,
            expected.event_id.encode_canonical().unwrap().as_slice()
        );
        assert_eq!(session_bytes.len(), StableEntityId::CANONICAL_LEN);
        assert_eq!(
            session_bytes,
            expected.session_id.encode_canonical().unwrap().as_slice()
        );
        assert_eq!(
            index
                .event_by_id(expected.event_id.as_uuid())
                .unwrap()
                .unwrap()
                .event_id,
            expected.event_id
        );
    }

    #[test]
    fn pinned_query_api_returns_typed_records_in_deterministic_order() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let first = document(&source, 1, "atomic generation");
        let second = document(&source, 2, "atomic generation");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(second.clone()).unwrap();
        writer.add_document(first.clone()).unwrap();
        writer.certify_source(certificate(&source, 1, 2)).unwrap();
        writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        let candidates = index
            .search_event_candidates("atomic:generation", 10)
            .unwrap();
        let mut expected_search_ids = vec![first.event_id.as_uuid(), second.event_id.as_uuid()];
        expected_search_ids.sort();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.event.event_id.as_uuid())
                .collect::<Vec<_>>(),
            expected_search_ids
        );
        assert_eq!(candidates[0].score, candidates[1].score);

        let exact = index
            .event_by_id(first.event_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(exact.event_id, first.event_id);
        assert_eq!(exact.session_id, first.session_id);
        assert_eq!(exact.locator, first.locator);
        assert_eq!(exact.provider, "codex");
        assert_eq!(exact.source_format, "codex_session_jsonl");
        assert_eq!(exact.provider_session_id.as_deref(), Some("session"));
        assert_eq!(exact.event_sequence, 1);
        assert_eq!(exact.occurred_at_unix_ms, first.occurred_at_unix_ms);
        assert_eq!(exact.event_type, "message");
        assert_eq!(exact.role.as_deref(), Some("user"));
        assert_eq!(exact.workspace.as_deref(), Some("ctx"));
        assert_eq!(exact.cwd.as_deref(), Some("/work/ctx"));
        assert_eq!(exact.touched_files, vec!["src/lib.rs"]);

        let event_id = first.event_id.to_string();
        let event_prefix = &event_id[..8];
        assert_eq!(
            index.events_by_id_prefix(event_prefix).unwrap()[0].event_id,
            first.event_id
        );

        let ordered = index
            .events_for_session(first.session_id.as_uuid())
            .unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|event| event.event_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let session = index
            .session_by_id(first.session_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(session.session_id, first.session_id);
        assert_eq!(session.provider, "codex");
        assert_eq!(session.source_format, "codex_session_jsonl");
        assert_eq!(session.provider_session_id.as_deref(), Some("session"));
        assert_eq!(session.first_event_sequence, 1);

        let session_id = first.session_id.to_string();
        let session_prefix = &session_id[..8];
        assert_eq!(
            index.sessions_by_id_prefix(session_prefix).unwrap(),
            vec![session]
        );
    }

    #[test]
    fn source_event_pages_order_across_segments_isolate_and_do_not_duplicate() {
        let temp = tempdir().unwrap();
        let target = source("paged-source.jsonl");
        let other = source("other-source.jsonl");
        let target_first = document(&target, 1, "target first");
        let target_second = document(&target, 2, "target second");
        let target_third = document(&target, 3, "target third");
        let target_fourth = document(&target, 4, "target fourth");
        let other_first = document(&other, 1, "other first");
        let other_second = document(&other, 2, "other second");

        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first
            .writer
            .set_merge_policy(Box::<NoMergePolicy>::default());
        first.begin_source(target.clone()).unwrap();
        first.add_document(target_fourth.clone()).unwrap();
        first.add_document(target_first.clone()).unwrap();
        first
            .certify_source(appendable_certificate(&target, 1, 2, 20))
            .unwrap();
        first.begin_source(other.clone()).unwrap();
        first.add_document(other_second.clone()).unwrap();
        first.add_document(other_first.clone()).unwrap();
        first.certify_source(certificate(&other, 1, 2)).unwrap();
        first.commit(|_| true).unwrap();

        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        append
            .writer
            .set_merge_policy(Box::<NoMergePolicy>::default());
        let base = append.begin_source_append(target.clone()).unwrap().clone();
        append.add_document(target_third.clone()).unwrap();
        append.add_document(target_second.clone()).unwrap();
        let proof = CertifiedSourceAppend::certify(
            &base,
            appendable_certificate(&target, 2, 4, 40),
            20,
            [1; 32],
        )
        .unwrap();
        append.certify_source_append(proof).unwrap();
        append.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert!(
            index.searcher.segment_readers().len() >= 2,
            "test requires a multi-segment generation"
        );
        let mut expected = vec![
            target_first.event_id,
            target_second.event_id,
            target_third.event_id,
            target_fourth.event_id,
        ];
        expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

        let first_page = index.source_event_page(&target, None, 2).unwrap();
        assert_eq!(first_page.generation_id, index.generation_id());
        assert!(first_page.source.exact_descriptor_eq(&target));
        assert!(!first_page.terminal);
        assert_eq!(
            first_page
                .items
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            expected[..2]
        );
        assert!(first_page
            .items
            .iter()
            .all(|event| event.locator.source().exact_descriptor_eq(&target)));

        let serialized = serde_json::to_vec(first_page.next_cursor.as_ref().unwrap()).unwrap();
        let cursor: SourceEventCursor = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(cursor.generation_id(), index.generation_id());
        assert!(cursor.source().exact_descriptor_eq(&target));
        assert_eq!(cursor.after(), expected[1]);
        let final_page = index.source_event_page(&target, Some(&cursor), 2).unwrap();
        assert!(final_page.terminal);
        assert!(final_page.next_cursor.is_none());
        assert_eq!(
            final_page
                .items
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            expected[2..]
        );

        let all = collect_source_pages(&index, &target, 1);
        assert_eq!(
            all.iter().map(|event| event.event_id).collect::<Vec<_>>(),
            expected
        );
        let unique = all
            .iter()
            .map(|event| event.event_id)
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), expected.len());
        assert!(all
            .iter()
            .all(|event| event.locator.source().exact_descriptor_eq(&target)));

        let other_page = index.source_event_page(&other, None, 10).unwrap();
        let mut expected_other = vec![other_first.event_id, other_second.event_id];
        expected_other.sort_by_key(|identity| identity.encode_canonical().unwrap());
        assert_eq!(
            other_page
                .items
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            expected_other
        );
        assert!(matches!(
            index.source_event_page(&other, Some(&cursor), 1),
            Err(IndexError::SourceEventCursorSourceMismatch)
        ));
        let invalid_identity =
            SourceEventCursor::new(index.generation_id(), target.clone(), other_first.event_id);
        assert!(matches!(
            index.source_event_page(&target, Some(&invalid_identity), 1),
            Err(IndexError::InvalidSourceEventCursorIdentity)
        ));
    }

    #[test]
    fn source_event_pages_bind_generation_descriptor_and_bounds() {
        const { assert!(MAX_SOURCE_EVENT_PAGE_ITEMS <= 4_096) };
        let temp = tempdir().unwrap();
        let source = source("rewrite-delete-pages.jsonl");
        let old_first = document(&source, 1, "old first");
        let old_second = document(&source, 2, "old second");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first.add_document(old_second.clone()).unwrap();
        first.add_document(old_first.clone()).unwrap();
        first.certify_source(certificate(&source, 1, 2)).unwrap();
        first.commit(|_| true).unwrap();
        let old_pin = VerifiedIndex::open(temp.path()).unwrap();
        let old_cursor = old_pin
            .source_event_page(&source, None, 1)
            .unwrap()
            .next_cursor
            .unwrap();

        assert!(matches!(
            old_pin.source_event_page(&source, None, 0),
            Err(IndexError::InvalidSourceEventPageSize { .. })
        ));
        assert!(matches!(
            old_pin.source_event_page(&source, None, MAX_SOURCE_EVENT_PAGE_ITEMS + 1),
            Err(IndexError::InvalidSourceEventPageSize { .. })
        ));
        assert!(
            old_pin
                .source_event_page(&source, None, MAX_SOURCE_EVENT_PAGE_ITEMS)
                .unwrap()
                .terminal
        );

        let mut rewritten_first = document(&source, 1, "rewritten first");
        rewritten_first.workspace = Some("rewritten".to_owned());
        let replacement = document(&source, 3, "replacement");
        let mut rewriting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        rewriting.begin_source(source.clone()).unwrap();
        rewriting.add_document(replacement.clone()).unwrap();
        rewriting.add_document(rewritten_first.clone()).unwrap();
        rewriting
            .certify_source(certificate(&source, 2, 2))
            .unwrap();
        rewriting.commit(|_| true).unwrap();
        let rewritten_pin = VerifiedIndex::open(temp.path()).unwrap();

        assert!(matches!(
            rewritten_pin.source_event_page(&source, Some(&old_cursor), 1),
            Err(IndexError::SourceEventCursorGenerationMismatch { .. })
        ));
        let rewritten = collect_source_pages(&rewritten_pin, &source, 1);
        assert_eq!(rewritten.len(), 2);
        assert!(rewritten.iter().any(|event| {
            event.event_id == rewritten_first.event_id
                && event.workspace.as_deref() == Some("rewritten")
        }));
        assert!(rewritten
            .iter()
            .any(|event| event.event_id == replacement.event_id));
        assert!(rewritten
            .iter()
            .all(|event| event.event_id != old_second.event_id));
        let old = collect_source_pages(&old_pin, &source, 1);
        assert_eq!(old.len(), 2);
        assert!(old.iter().any(|event| event.event_id == old_first.event_id));

        let changed_descriptor = source_for_provider(
            "codex",
            "codex_prompt_history_jsonl",
            "rewrite-delete-pages.jsonl",
        );
        assert_eq!(changed_descriptor, source);
        assert!(!changed_descriptor.exact_descriptor_eq(&source));
        assert!(matches!(
            rewritten_pin.source_event_page(&changed_descriptor, None, 1),
            Err(IndexError::SourceEventSourceDescriptorMismatch(_))
        ));

        let (deletion, inventory) = deletion_evidence(&source, 3);
        let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        deleting.delete_source(deletion, inventory).unwrap();
        deleting.commit(|_| true).unwrap();
        let deleted_pin = VerifiedIndex::open(temp.path()).unwrap();
        assert!(matches!(
            deleted_pin.source_event_page(&source, Some(&old_cursor), 1),
            Err(IndexError::SourceEventCursorGenerationMismatch { .. })
        ));
        assert!(matches!(
            deleted_pin.source_event_page(&source, None, 1),
            Err(IndexError::SourceEventSourceNotRetained(_))
        ));
        assert_eq!(collect_source_pages(&old_pin, &source, 1).len(), 2);
        assert_eq!(collect_source_pages(&rewritten_pin, &source, 1).len(), 2);
    }

    #[test]
    fn semantic_event_pages_follow_full_identity_order_and_explicit_eligibility() {
        let temp = tempdir().unwrap();
        let source = source("semantic-pages.jsonl");
        let first = document(&source, 1, "first eligible user message");
        let mut assistant = document(&source, 2, "assistant message");
        assistant.role = Some("assistant".to_owned());
        let mut tool = document(&source, 3, "user-shaped tool call");
        tool.event_type = "tool_call".to_owned();
        let mut control = document(
            &source,
            4,
            "  <environment_context>not a semantic turn</environment_context>  ",
        );
        control.event_type = "notice".to_owned();
        let second = document(&source, 5, "second eligible user message");
        let third = document(&source, 6, "third eligible user message");
        let mut aborted = document(&source, 7, "<turn_aborted>interrupted</turn_aborted>");
        aborted.event_type = "notice".to_owned();
        let mut notification = document(
            &source,
            8,
            "<subagent_notification>completed</subagent_notification>",
        );
        notification.event_type = "notice".to_owned();
        let mut warning = document(
            &source,
            9,
            "Warning: The maximum number of unified exec processes has been reached",
        );
        warning.event_type = "notice".to_owned();
        let discussion = document(
            &source,
            10,
            "How should an embedded <environment_context> marker be rendered?",
        );

        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        for document in [
            third.clone(),
            assistant,
            first.clone(),
            control,
            tool,
            second.clone(),
            aborted,
            notification,
            warning,
            discussion.clone(),
        ] {
            writer.add_document(document).unwrap();
        }
        writer.certify_source(certificate(&source, 1, 10)).unwrap();
        writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        let mut expected = [
            first.event_id,
            second.event_id,
            third.event_id,
            discussion.event_id,
        ];
        expected.sort_by_key(|identity| identity.encode_canonical().unwrap());

        let first_page = index.semantic_event_page(None, 2).unwrap();
        assert_eq!(first_page.generation_id, index.generation_id());
        assert_eq!(
            first_page.eligibility,
            SemanticEligibility::UserMessageCandidateV2
        );
        assert_eq!(first_page.eligible_total, 4);
        assert_eq!(first_page.eligible_count(), 2);
        assert!(!first_page.terminal);
        assert_eq!(
            first_page
                .items
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            expected[..2]
        );
        assert_eq!(first_page.items[0].locator.source(), &source);
        assert_eq!(
            first_page.items[0].root_session_id,
            first_page.items[0].session_id
        );

        let cursor = first_page.next_cursor.unwrap();
        assert_eq!(cursor.generation_id(), index.generation_id());
        assert_eq!(cursor.eligibility(), SemanticEligibility::CURRENT);
        assert_eq!(cursor.after(), expected[1]);

        let final_page = index.semantic_event_page(Some(&cursor), 2).unwrap();
        assert_eq!(final_page.eligible_total, 4);
        assert_eq!(final_page.eligible_count(), 2);
        assert_eq!(
            final_page
                .items
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            expected[2..]
        );
        assert!(final_page.terminal);
        assert!(final_page.next_cursor.is_none());
        assert_eq!(index.semantic_eligible_event_count().unwrap(), 4);
    }

    #[test]
    fn semantic_event_pages_handle_empty_final_and_generation_bound_cursors() {
        let temp = tempdir().unwrap();
        GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .commit(|_| true)
            .unwrap();
        let empty = VerifiedIndex::open(temp.path()).unwrap();
        let page = empty.semantic_event_page(None, 1).unwrap();
        assert_eq!(page.eligible_total, 0);
        assert!(page.items.is_empty());
        assert!(page.terminal);
        assert!(page.next_cursor.is_none());

        let source = source("final-page.jsonl");
        let expected = document(&source, 1, "only eligible event");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(expected.clone()).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();
        let index = VerifiedIndex::open(temp.path()).unwrap();

        let final_page = index.semantic_event_page(None, 1).unwrap();
        assert_eq!(final_page.items.len(), 1);
        assert!(final_page.terminal);
        assert!(final_page.next_cursor.is_none());

        let after_last = SemanticEventCursor::new(index.generation_id(), expected.event_id);
        let empty_final = index.semantic_event_page(Some(&after_last), 1).unwrap();
        assert_eq!(empty_final.eligible_total, 1);
        assert!(empty_final.items.is_empty());
        assert!(empty_final.terminal);
        assert!(empty_final.next_cursor.is_none());

        let foreign = SemanticEventCursor::new("0".repeat(64), expected.event_id);
        assert!(matches!(
            index.semantic_event_page(Some(&foreign), 1),
            Err(IndexError::SemanticEventCursorGenerationMismatch { .. })
        ));
        assert!(matches!(
            index.semantic_event_page(None, 0),
            Err(IndexError::InvalidSemanticEventPageSize { .. })
        ));
        assert!(matches!(
            index.semantic_event_page(None, MAX_SEMANTIC_EVENT_PAGE_ITEMS + 1),
            Err(IndexError::InvalidSemanticEventPageSize { .. })
        ));
    }

    #[test]
    fn semantic_event_pages_keep_old_pins_isolated_from_rewrite_and_deletion() {
        fn page_all(index: &VerifiedIndex) -> Vec<EventRecord> {
            let mut cursor = None;
            let mut records = Vec::new();
            loop {
                let page = index.semantic_event_page(cursor.as_ref(), 1).unwrap();
                records.extend(page.items);
                if page.terminal {
                    return records;
                }
                cursor = Some(page.next_cursor.unwrap());
            }
        }

        let temp = tempdir().unwrap();
        let source = source("rewrite-delete.jsonl");
        let old_first = document(&source, 1, "old first event");
        let old_second = document(&source, 2, "old second event");
        let mut first_writer =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first_writer.begin_source(source.clone()).unwrap();
        first_writer.add_document(old_second.clone()).unwrap();
        first_writer.add_document(old_first.clone()).unwrap();
        first_writer
            .certify_source(certificate(&source, 1, 2))
            .unwrap();
        first_writer.commit(|_| true).unwrap();
        let old_pin = VerifiedIndex::open(temp.path()).unwrap();
        let old_cursor = old_pin
            .semantic_event_page(None, 1)
            .unwrap()
            .next_cursor
            .unwrap();

        let mut rewritten_first = document(&source, 1, "rewritten first event");
        rewritten_first.workspace = Some("rewritten-workspace".to_owned());
        let replacement = document(&source, 3, "replacement third event");
        let mut replacement_writer =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        replacement_writer.begin_source(source.clone()).unwrap();
        replacement_writer
            .add_document(replacement.clone())
            .unwrap();
        replacement_writer
            .add_document(rewritten_first.clone())
            .unwrap();
        replacement_writer
            .certify_source(certificate(&source, 2, 2))
            .unwrap();
        replacement_writer.commit(|_| true).unwrap();
        let rewritten_pin = VerifiedIndex::open(temp.path()).unwrap();

        let old_records = page_all(&old_pin);
        assert_eq!(old_records.len(), 2);
        assert!(old_records
            .iter()
            .any(|event| event.event_id == old_first.event_id));
        assert!(old_records
            .iter()
            .any(|event| event.event_id == old_second.event_id));

        let rewritten_records = page_all(&rewritten_pin);
        assert_eq!(rewritten_records.len(), 2);
        assert!(rewritten_records.iter().any(|event| {
            event.event_id == rewritten_first.event_id
                && event.workspace.as_deref() == Some("rewritten-workspace")
        }));
        assert!(rewritten_records
            .iter()
            .all(|event| event.event_id != old_second.event_id));
        assert!(rewritten_records
            .iter()
            .any(|event| event.event_id == replacement.event_id));
        assert_ne!(old_pin.generation_id(), rewritten_pin.generation_id());
        assert!(matches!(
            rewritten_pin.semantic_event_page(Some(&old_cursor), 1),
            Err(IndexError::SemanticEventCursorGenerationMismatch { .. })
        ));

        let mut deletion_writer =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let (deletion, inventory) = deletion_evidence(&source, 3);
        deletion_writer.delete_source(deletion, inventory).unwrap();
        deletion_writer.commit(|_| true).unwrap();
        let deleted_pin = VerifiedIndex::open(temp.path()).unwrap();

        assert!(page_all(&deleted_pin).is_empty());
        assert_eq!(page_all(&old_pin).len(), 2);
        assert_eq!(page_all(&rewritten_pin).len(), 2);
    }

    #[test]
    fn filtered_search_covers_relationship_and_public_metadata_contracts() {
        let temp = tempdir().unwrap();
        let codex_root = source("codex-root");
        let codex_child = source("codex-child");
        let claude = source_for_provider(
            "claude_code",
            "claude_projects_jsonl_tree",
            "claude-sessions",
        );

        let mut root = document_for_session(&codex_root, "root-thread", 1, "shared needle");
        root.workspace = Some("Ctx-Rich-Fixture".to_owned());
        root.cwd = Some("/work/ctx-root".to_owned());
        root.source_path = Some("/history/ctx[root].jsonl".to_owned());
        root.occurred_at_unix_ms = Some(100);
        let root_session_id = root.session_id;
        root.root_session_id = root_session_id;

        let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
        child.parent_session_id = Some(root_session_id);
        child.root_session_id = root_session_id;
        child.branch = Some("feature/query-seam".to_owned());
        child.workspace = Some("ChildSpace".to_owned());
        child.cwd = Some("/work/child".to_owned());
        child.source_path = Some("/history/child.jsonl".to_owned());
        child.agent_type = "subagent".to_owned();
        child.is_primary = false;
        child.event_type = "tool_call".to_owned();
        child.role = Some("assistant".to_owned());
        child.occurred_at_unix_ms = Some(200);
        child.touched_files = vec!["crates/Query.rs".to_owned()];
        let child_session_id = child.session_id;

        let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
        other.workspace = Some("Elsewhere".to_owned());
        other.branch = Some("release".to_owned());
        other.occurred_at_unix_ms = Some(300);
        let other_session_id = other.session_id;
        other.root_session_id = other_session_id;

        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(codex_root.clone()).unwrap();
        writer.add_document(root).unwrap();
        writer
            .certify_source(certificate(&codex_root, 1, 1))
            .unwrap();
        writer.begin_source(codex_child.clone()).unwrap();
        writer.add_document(child).unwrap();
        writer
            .certify_source(certificate(&codex_child, 1, 1))
            .unwrap();
        writer.begin_source(claude.clone()).unwrap();
        writer.add_document(other).unwrap();
        writer.certify_source(certificate(&claude, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();
        let index = VerifiedIndex::open(temp.path()).unwrap();

        let all = sorted_uuids(vec![
            root_session_id.as_uuid(),
            child_session_id.as_uuid(),
            other_session_id.as_uuid(),
        ]);
        assert_eq!(
            filtered_session_ids(&index, EventSearchFilters::default()),
            all
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    provider: Some("claude_code".to_owned()),
                    ..EventSearchFilters::default()
                }
            ),
            vec![other_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    workspace: Some("CTX[ROOT]".to_owned()),
                    ..EventSearchFilters::default()
                }
            ),
            vec![root_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    since_unix_ms: Some(250),
                    ..EventSearchFilters::default()
                }
            ),
            vec![other_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    event_type: Some("tool_call".to_owned()),
                    role: Some("assistant".to_owned()),
                    agent_type: Some("subagent".to_owned()),
                    ..EventSearchFilters::default()
                }
            ),
            vec![child_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    agent_scope: AgentScope::Primary,
                    ..EventSearchFilters::default()
                }
            ),
            sorted_uuids(vec![root_session_id.as_uuid(), other_session_id.as_uuid()])
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    session_id: Some(child_session_id.as_uuid()),
                    agent_scope: AgentScope::Primary,
                    ..EventSearchFilters::default()
                }
            ),
            vec![child_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    parent_session_id: Some(root_session_id.as_uuid()),
                    root_session_id: Some(root_session_id.as_uuid()),
                    provider_session_id: Some("child-thread".to_owned()),
                    branch: Some("feature/query-seam".to_owned()),
                    file: Some("QUERY.RS".to_owned()),
                    ..EventSearchFilters::default()
                }
            ),
            vec![child_session_id.as_uuid()]
        );
        assert_eq!(
            filtered_session_ids(
                &index,
                EventSearchFilters {
                    exclude_session_tree: Some(ExcludedSessionTree {
                        provider: "codex".to_owned(),
                        provider_session_id: "root-thread".to_owned(),
                        session_id: Some(root_session_id.as_uuid()),
                    }),
                    ..EventSearchFilters::default()
                }
            ),
            vec![other_session_id.as_uuid()]
        );

        let child = index
            .session_by_id(child_session_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_session_id, Some(root_session_id));
        assert_eq!(child.root_session_id, root_session_id);
        assert_eq!(child.provider_session_id.as_deref(), Some("child-thread"));
        assert_eq!(child.branch.as_deref(), Some("feature/query-seam"));
        assert_eq!(child.source_path.as_deref(), Some("/history/child.jsonl"));
        assert_eq!(child.agent_type, "subagent");
        assert!(!child.is_primary);
    }

    #[test]
    fn full_body_is_searchable_but_never_stored_or_returned() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let body = format!("{} tailonlyneedle", "界".repeat(16_384));
        let expected = document(&source, 1, &body);
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(expected.clone()).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        let record = index
            .event_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(record.locator, expected.locator);
        assert_eq!(
            index.search_event_candidates("tailonlyneedle", 10).unwrap()[0]
                .event
                .event_id,
            expected.event_id
        );

        let fields = fields_from_schema(index.searcher.schema()).unwrap();
        let address = index
            .searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stored: TantivyDocument = index.searcher.doc(address).unwrap();
        assert!(stored.get_first(fields.body_search).is_none());
    }

    #[test]
    fn empty_or_invalid_programmatic_queries_are_safe() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();
        let index = VerifiedIndex::open(temp.path()).unwrap();

        assert!(index.search_event_candidates("", 10).unwrap().is_empty());
        assert!(index.search_event_candidates("body", 0).unwrap().is_empty());
        assert!(matches!(
            index.search_event_candidates_with_filters(
                "body",
                &EventSearchFilters {
                    provider: Some("  ".to_owned()),
                    ..EventSearchFilters::default()
                },
                10,
            ),
            Err(IndexError::EmptyQueryFilter { field: "provider" })
        ));
        assert!(matches!(
            index.events_by_id_prefix("not-a-uuid"),
            Err(IndexError::InvalidIdPrefix)
        ));
        assert!(matches!(
            index.sessions_by_id_prefix(""),
            Err(IndexError::InvalidIdPrefix)
        ));
    }

    #[test]
    fn failed_final_revalidation_keeps_the_previous_generation() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first
            .add_document(document(&source, 1, "previous generation"))
            .unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        let first_receipt = first.commit(|_| true).unwrap();

        let mut replacement =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        replacement.begin_source(source.clone()).unwrap();
        replacement
            .add_document(document(&source, 1, "uncommitted replacement"))
            .unwrap();
        replacement
            .certify_source(certificate(&source, 2, 1))
            .unwrap();
        let error = replacement.commit(|_| false).unwrap_err();
        assert!(matches!(error, IndexError::SourceInvalidated(_)));

        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(index.generation_id(), first_receipt.generation_id);
        assert_eq!(index.count_term("previous").unwrap(), 1);
        assert_eq!(index.count_term("uncommitted").unwrap(), 0);
    }

    #[test]
    fn deletion_requires_final_inventory_revalidation() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first
            .add_document(document(&source, 1, "retained"))
            .unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        first.commit(|_| true).unwrap();

        let mut rejected = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let (deletion, inventory) = deletion_evidence(&source, 2);
        rejected.delete_source(deletion, inventory).unwrap();
        let error = rejected
            .commit(|target| matches!(target, RevalidationTarget::Source(_)))
            .unwrap_err();
        assert!(matches!(error, IndexError::SourceInvalidated(_)));
        assert_eq!(
            VerifiedIndex::open(temp.path())
                .unwrap()
                .count_term("retained")
                .unwrap(),
            1
        );

        let mut accepted = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let (deletion, inventory) = deletion_evidence(&source, 3);
        accepted.delete_source(deletion, inventory).unwrap();
        accepted.commit(|_| true).unwrap();
        let current = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(current.count_term("retained").unwrap(), 0);
        assert!(current.manifest().sources.is_empty());
        assert_eq!(current.manifest().removals.len(), 1);
        assert_eq!(current.manifest().removals[0].source(), &source);
        assert!(current.manifest().removals[0]
            .deletion()
            .verifies(current.manifest().removals[0].inventory()));
    }

    #[test]
    fn generation_removals_persist_until_the_exact_lineage_returns() {
        let temp = tempdir().unwrap();
        let removed = source("removed.jsonl");
        let retained = source("retained.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(removed.clone()).unwrap();
        first
            .add_document(document(&removed, 1, "removed body"))
            .unwrap();
        first.certify_source(certificate(&removed, 1, 1)).unwrap();
        first.begin_source(retained.clone()).unwrap();
        first
            .add_document(document(&retained, 1, "retained body"))
            .unwrap();
        first.certify_source(certificate(&retained, 1, 1)).unwrap();
        first.commit(|_| true).unwrap();

        let (deletion, inventory) = deletion_evidence(&removed, 2);
        let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        deleting.delete_source(deletion, inventory).unwrap();
        let deleted_receipt = deleting.commit(|_| true).unwrap();
        let deleted = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(deleted.manifest().sources.len(), 1);
        assert_eq!(
            deleted.manifest().sources[0].observation().source(),
            &retained
        );
        assert_eq!(deleted.manifest().removals.len(), 1);
        let durable_removal = deleted.manifest().removals[0].clone();
        assert_eq!(durable_removal.source(), &removed);

        let mut unrelated = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        unrelated.begin_source(retained.clone()).unwrap();
        unrelated
            .add_document(document(&retained, 2, "rewritten retained body"))
            .unwrap();
        unrelated
            .certify_source(certificate(&retained, 3, 1))
            .unwrap();
        let unrelated_receipt = unrelated.commit(|_| true).unwrap();
        let carried = VerifiedIndex::open(temp.path()).unwrap();
        assert_ne!(
            deleted_receipt.generation_id,
            unrelated_receipt.generation_id
        );
        assert_eq!(carried.manifest().removals, vec![durable_removal]);

        let returning = source_for_provider("codex", "codex_prompt_history_jsonl", "removed.jsonl");
        assert_eq!(returning, removed);
        assert!(!returning.exact_descriptor_eq(&removed));
        let mut republishing =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        republishing.begin_source(returning.clone()).unwrap();
        republishing
            .add_document(document(&returning, 4, "returned body"))
            .unwrap();
        republishing
            .certify_source(certificate(&returning, 4, 1))
            .unwrap();
        republishing.commit(|_| true).unwrap();

        let returned = VerifiedIndex::open(temp.path()).unwrap();
        assert!(returned.manifest().removals.is_empty());
        assert!(returned.manifest().sources.iter().any(|source| source
            .observation()
            .source()
            .exact_descriptor_eq(&returning)));
    }

    #[test]
    fn generation_removal_validation_binds_inventory_order_and_membership() {
        let first = source("first-removed.jsonl");
        let second = source("second-removed.jsonl");
        let (first_deletion, first_inventory) = deletion_evidence(&first, 1);
        let (_, wrong_inventory) = deletion_evidence(&first, 2);
        assert!(matches!(
            GenerationRemoval::new(first_deletion.clone(), wrong_inventory),
            Err(IndexError::InvalidGenerationRemoval(_))
        ));

        let first_removal = GenerationRemoval::new(first_deletion, first_inventory).unwrap();
        let (second_deletion, second_inventory) = deletion_evidence(&second, 3);
        let second_removal = GenerationRemoval::new(second_deletion, second_inventory).unwrap();
        let canonical = GenerationManifest::from_parts(
            Vec::new(),
            vec![second_removal.clone(), first_removal.clone()],
        )
        .unwrap();
        assert!(canonical
            .removals
            .windows(2)
            .all(|pair| { source_sort_key(pair[0].source()) < source_sort_key(pair[1].source()) }));
        assert_ne!(
            GenerationManifest::from_sources(Vec::new())
                .unwrap()
                .generation_id()
                .unwrap(),
            canonical.generation_id().unwrap()
        );

        let mut duplicate = canonical.clone();
        duplicate.removals.push(duplicate.removals[0].clone());
        duplicate
            .removals
            .sort_by_key(|removal| source_sort_key(removal.source()));
        assert!(matches!(
            duplicate.validate_contract(),
            Err(IndexError::NonCanonicalManifestRemovals)
        ));

        let mut out_of_order = canonical.clone();
        out_of_order.removals.reverse();
        assert!(matches!(
            out_of_order.validate_contract(),
            Err(IndexError::NonCanonicalManifestRemovals)
        ));

        assert!(matches!(
            GenerationManifest::from_parts(vec![certificate(&first, 1, 0)], vec![first_removal]),
            Err(IndexError::ManifestSourceRemovalOverlap(_))
        ));
    }

    #[test]
    fn replacement_atomically_removes_old_source_documents() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first
            .add_document(document(&source, 1, "retired content"))
            .unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        first.commit(|_| true).unwrap();

        let mut replacement =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        replacement.begin_source(source.clone()).unwrap();
        replacement
            .add_document(document(&source, 1, "current content"))
            .unwrap();
        replacement
            .certify_source(certificate(&source, 2, 1))
            .unwrap();
        replacement.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(index.count_term("retired").unwrap(), 0);
        assert_eq!(index.count_term("current").unwrap(), 1);
        assert_eq!(index.manifest().sources.len(), 1);
    }

    #[test]
    fn certified_append_indexes_only_the_delta() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first.add_document(document(&source, 1, "base")).unwrap();
        first
            .certify_source(appendable_certificate(&source, 1, 1, 10))
            .unwrap();
        first.commit(|_| true).unwrap();

        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        let base = append.begin_source_append(source.clone()).unwrap().clone();
        append.add_document(document(&source, 2, "delta")).unwrap();
        let proof = CertifiedSourceAppend::certify(
            &base,
            appendable_certificate(&source, 2, 2, 20),
            10,
            [1; 32],
        )
        .unwrap();
        append.certify_source_append(proof).unwrap();
        append.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(index.document_count(), 2);
        assert_eq!(index.count_term("base").unwrap(), 1);
        assert_eq!(index.count_term("delta").unwrap(), 1);
        assert_eq!(index.manifest().sources[0].counts().indexed_documents, 2);
    }

    #[test]
    fn append_rejects_an_identity_already_in_the_base() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first.add_document(document(&source, 1, "base")).unwrap();
        first
            .certify_source(appendable_certificate(&source, 1, 1, 10))
            .unwrap();
        first.commit(|_| true).unwrap();

        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        append.begin_source_append(source.clone()).unwrap();
        let error = append
            .add_document(document(&source, 1, "duplicate"))
            .unwrap_err();
        assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    }

    #[test]
    fn verified_reader_remains_pinned_to_its_generation() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut first = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        first.begin_source(source.clone()).unwrap();
        first
            .add_document(document(&source, 1, "old pinned generation"))
            .unwrap();
        first.certify_source(certificate(&source, 1, 1)).unwrap();
        first.commit(|_| true).unwrap();
        let old_reader = VerifiedIndex::open(temp.path()).unwrap();

        let mut replacement =
            GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        replacement.begin_source(source.clone()).unwrap();
        replacement
            .add_document(document(&source, 1, "new committed generation"))
            .unwrap();
        replacement
            .certify_source(certificate(&source, 2, 1))
            .unwrap();
        replacement.commit(|_| true).unwrap();

        assert_eq!(old_reader.count_term("old").unwrap(), 1);
        assert_eq!(old_reader.count_term("new").unwrap(), 0);
        let new_reader = VerifiedIndex::open(temp.path()).unwrap();
        assert_eq!(new_reader.count_term("old").unwrap(), 0);
        assert_eq!(new_reader.count_term("new").unwrap(), 1);
        assert_ne!(old_reader.generation_id(), new_reader.generation_id());
    }

    #[test]
    fn a_partial_unreferenced_manifest_does_not_poison_retry() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let certificate = certificate(&source, 1, 1);
        let manifest = GenerationManifest::from_sources(vec![certificate.clone()]).unwrap();
        let generation_id = manifest.generation_id().unwrap();
        let path = manifest_path(temp.path(), &generation_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"partial").unwrap();

        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate).unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        assert_eq!(receipt.generation_id, generation_id);
        assert!(VerifiedIndex::open(temp.path()).is_ok());
        assert!(fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-")));
    }

    #[test]
    fn manifest_corruption_fails_closed() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        fs::write(
            manifest_path(temp.path(), &receipt.generation_id),
            b"corrupt",
        )
        .unwrap();

        let error = match VerifiedIndex::open(temp.path()) {
            Ok(_) => panic!("corrupt manifest unexpectedly opened"),
            Err(error) => error,
        };
        assert!(matches!(error, IndexError::ManifestDigestMismatch { .. }));
    }

    #[test]
    fn stale_schema_manifest_fails_closed_at_generation_boundary() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let index = VerifiedIndex::open(temp.path()).unwrap();
        let mut stale_manifest = index.manifest().clone();
        stale_manifest.lexical_schema_version = 3;
        let stale_generation_id = stale_manifest.generation_id().unwrap();
        write_manifest(temp.path(), &stale_generation_id, &stale_manifest).unwrap();
        let mut stale_metas = index.searcher.index().load_metas().unwrap();
        stale_metas.payload = Some(
            serde_json::to_string(&CommitPayload {
                version: COMMIT_PAYLOAD_VERSION,
                generation_id: stale_generation_id,
            })
            .unwrap(),
        );

        let error = load_manifest_for_metas(temp.path(), &stale_metas).unwrap_err();
        assert!(matches!(
            error,
            IndexError::GenerationContractMismatch {
                identity: IDENTITY_VERSION,
                schema: 3,
                analyzer: LEXICAL_ANALYZER_VERSION,
            }
        ));
    }

    #[test]
    fn current_manifest_roundtrips_with_exact_policy_hash() {
        let source = source("manifest-roundtrip.jsonl");
        let manifest = GenerationManifest::from_sources(vec![certificate(&source, 7, 3)]).unwrap();
        let canonical = serde_json::to_vec(&manifest).unwrap();
        let roundtrip: GenerationManifest = serde_json::from_slice(&canonical).unwrap();

        assert_eq!(serde_json::to_vec(&roundtrip).unwrap(), canonical);
        assert_eq!(
            roundtrip.policy_schema_hash,
            current_source_generation_policy_hash().unwrap()
        );
        assert_eq!(
            roundtrip.generation_id().unwrap(),
            manifest.generation_id().unwrap()
        );
    }

    #[test]
    fn policy_field_change_changes_hash_and_generation_id() {
        let manifest = GenerationManifest::from_sources(Vec::new()).unwrap();
        let mut changed_policy = current_source_generation_policy();
        changed_policy.semantic.chunk_overlap_chars += 1;
        let changed_policy_hash = changed_policy.canonical_sha256().unwrap();
        let mut changed_manifest = manifest.clone();
        changed_manifest.policy_schema_hash = changed_policy_hash.clone();

        assert_ne!(manifest.policy_schema_hash, changed_policy_hash);
        assert_ne!(
            manifest.generation_id().unwrap(),
            changed_manifest.generation_id().unwrap()
        );
    }

    #[test]
    fn verified_open_rejects_mismatched_active_policy() {
        let temp = tempdir().unwrap();
        let source = source("policy-mismatch.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let pinned = VerifiedIndex::open(temp.path()).unwrap();
        let mut mismatched_policy = current_source_generation_policy();
        mismatched_policy.lexical.event_projector_revision += 1;
        let mismatched_policy_hash = mismatched_policy.canonical_sha256().unwrap();
        let mut mismatched_manifest = pinned.manifest().clone();
        mismatched_manifest.policy_schema_hash = mismatched_policy_hash.clone();
        let index = pinned.searcher.index().clone();
        publish_unchecked_generation(temp.path(), &index, mismatched_manifest, &[], Vec::new());

        let error = match VerifiedIndex::open(temp.path()) {
            Ok(_) => panic!("mismatched policy generation unexpectedly opened"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IndexError::GenerationPolicyMismatch {
                expected,
                actual,
            } if expected == current_source_generation_policy_hash().unwrap()
                && actual == mismatched_policy_hash
        ));
    }

    #[test]
    fn certificate_count_mismatch_is_rejected_before_commit() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        let error = writer
            .certify_source(certificate(&source, 1, 2))
            .unwrap_err();
        assert!(matches!(
            error,
            IndexError::SourceDocumentCountMismatch { .. }
        ));
    }

    #[test]
    fn duplicate_event_identity_is_rejected_before_commit() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        let duplicate = document(&source, 1, "first");
        writer.add_document(duplicate.clone()).unwrap();
        let error = writer.add_document(duplicate).unwrap_err();
        assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    }

    #[test]
    fn verified_generation_rejects_a_forged_duplicate_event_identity() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_document(document(&source, 1, "body")).unwrap();
        writer.certify_source(certificate(&source, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let pinned = VerifiedIndex::open(temp.path()).unwrap();
        let addresses = pinned.searcher.search(&AllQuery, &DocSetCollector).unwrap();
        let duplicate = pinned
            .searcher
            .doc(addresses.into_iter().next().unwrap())
            .unwrap();
        let index = pinned.searcher.index().clone();
        publish_unchecked_generation(
            temp.path(),
            &index,
            GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
            &[],
            vec![duplicate],
        );

        let error = match VerifiedIndex::open(temp.path()) {
            Ok(_) => panic!("duplicate event generation unexpectedly opened"),
            Err(error) => error,
        };
        assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    }

    #[test]
    fn verified_generation_rejects_forged_source_ownership() {
        let temp = tempdir().unwrap();
        let first = source("first.jsonl");
        let second = source("second.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(first.clone()).unwrap();
        writer.add_document(document(&first, 1, "body")).unwrap();
        writer.certify_source(certificate(&first, 1, 1)).unwrap();
        writer.commit(|_| true).unwrap();

        let pinned = VerifiedIndex::open(temp.path()).unwrap();
        let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
        let address = pinned
            .searcher
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let document = pinned.searcher.doc::<TantivyDocument>(address).unwrap();
        let mut forged = TantivyDocument::default();
        for (field, value) in document.field_values() {
            if field != fields.source_key {
                forged.add_field_value(field, value);
            }
        }
        forged.add_text(fields.source_key, source_token(&second));
        let index = pinned.searcher.index().clone();
        publish_unchecked_generation(
            temp.path(),
            &index,
            GenerationManifest::from_sources(vec![certificate(&second, 2, 1)]).unwrap(),
            std::slice::from_ref(&first),
            vec![forged],
        );

        let error = match VerifiedIndex::open(temp.path()) {
            Ok(_) => panic!("source ownership mismatch unexpectedly opened"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IndexError::InvalidStoredDocumentField("native_locator")
        ));
    }

    #[test]
    fn document_identity_kinds_are_checked() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        let mut invalid = document(&source, 1, "body");
        invalid.event_id = invalid.session_id;
        let error = writer.add_document(invalid).unwrap_err();
        assert!(matches!(error, IndexError::InvalidEventIdentityKind(_)));
    }

    #[test]
    fn document_identities_must_belong_to_the_document_source() {
        let temp = tempdir().unwrap();
        let first = source("first");
        let second = source("second");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(second.clone()).unwrap();
        let mut invalid = document(&first, 1, "body");
        invalid.locator = document(&second, 2, "other").locator;
        invalid.source = second;
        let error = writer.add_document(invalid).unwrap_err();
        assert!(matches!(error, IndexError::IdentitySourceMismatch(_)));
    }

    #[test]
    fn empty_body_is_rejected_without_an_index_side_length_limit() {
        let temp = tempdir().unwrap();
        let source = source("session.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        let error = writer.add_document(document(&source, 1, "")).unwrap_err();
        assert!(matches!(
            error,
            IndexError::EmptyDocumentField { field: "body" }
        ));
    }

    #[test]
    fn invalid_memory_budget_has_no_filesystem_side_effect() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("not-created");
        let error = match GenerationWriter::open(
            &root,
            WriterOptions {
                indexer_threads: 2,
                memory_bytes: 1,
            },
        ) {
            Ok(_) => panic!("invalid memory budget unexpectedly opened an index"),
            Err(error) => error,
        };
        assert!(matches!(error, IndexError::IndexMemoryTooSmall { .. }));
        assert!(!root.exists());
    }
}
