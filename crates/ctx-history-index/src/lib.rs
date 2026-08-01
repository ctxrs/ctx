//! Atomic self-contained lexical Core generations.
//!
//! A Tantivy commit names a durable immutable source-revision manifest, so
//! readers observe either the previous complete generation or the next one.

mod analyzer;
mod contracts;
mod durable_directory;
mod identity;
mod index_document;
pub mod policy;
mod publication;
mod query;
mod reader;
mod schema;
mod staging;
mod writer_publication;
mod writer_support;

pub use durable_directory::durable_atomic_replace_file;

pub(crate) use contracts::{
    CommitPayload, COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD, MANIFEST_DIRECTORY,
    MAX_DOCUMENT_METADATA_BYTES,
};
pub use contracts::{
    CommitReceipt, ConsecutiveSourceMissingCount, GenerationManifest, GenerationRemoval,
    IndexError, Result, RevalidationTarget, SourceCatalogCheckpoint, SourceCatalogMissingState,
    SourceCoreRecordAggregate, SourceMissingObservationPoint, WriterOptions,
    GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
    LEXICAL_SEGMENT_MERGE_FAN_IN,
};
pub use ctx_history_core::CoreRecord;
pub(crate) use identity::{
    hex, prior_core_record, register_compact_identity, register_event_identity,
    register_session_identity, sha256_hex, source_sort_key, source_token,
    validate_event_identity_against_base, validate_referenced_session_identity_against_base,
    validate_session_identity_against_base,
};
pub use policy::{
    current_source_generation_policy, current_source_generation_policy_hash,
    EmbeddingGenerationPolicy, LexicalBodySelection, LexicalGenerationPolicy,
    LexicalIndexedBodyLimit, SemanticCoreContentFilter, SemanticGenerationPolicy, SourceEventClass,
    SourceEventRole, SourceGenerationPolicy, StoredSourceContent, LEXICAL_INDEXED_BODY_LIMIT,
    LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION, SEMANTIC_CHUNK_OVERLAP_CHARS,
    SEMANTIC_CHUNK_TARGET_CHARS, SEMANTIC_EMBEDDING_CONTRACT_REVISION,
    SEMANTIC_EMBEDDING_DIMENSIONS, SEMANTIC_EMBEDDING_MODEL, SEMANTIC_EMBEDDING_MODEL_REVISION,
    SEMANTIC_EMBEDDING_NORMALIZATION, SEMANTIC_SOURCE_MAX_CHARS,
};
#[cfg(test)]
pub(crate) use publication::manifest_path;
pub(crate) use publication::{
    create_candidate_generation, load_active_generation_pointer, load_manifest_for_metas,
    meta_generation, migrate_legacy_generation, open_slot_index, payload_generation_id,
    publish_active_generation_pointer, reclaim_inactive_generation_directories,
    reclaim_unreferenced_manifests, reconcile_commit_error, searcher_generation, sync_directory,
    sync_generation, verify_searcher, verify_searcher_structure, write_manifest,
    ActiveGenerationPointer, GenerationSlot, INDEX_GENERATIONS_DIRECTORY,
};
pub use query::{
    AgentScope, CoreEventBatch, CoreEventPageBudget, CoreEventRecord, CoreSemanticEventPage,
    CoreSourceEventPage, EventRecord, EventSearchCandidate, EventSearchFilters,
    ExcludedSessionTree, LexicalQueryLimits, SemanticEligibility, SemanticEventCursor,
    SemanticEventPage, SessionEventCoordinate, SessionRecord, SourceEventCursor, SourceEventPage,
    DEFAULT_CORE_EVENT_PAGE_BUDGET, LEXICAL_QUERY_LIMITS, MAX_SEMANTIC_EVENT_PAGE_ITEMS,
    MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
pub use reader::VerifiedIndex;
#[cfg(test)]
pub(crate) use schema::required_field;
pub(crate) use schema::{fields_from_schema, lexical_schema, validate_schema, Fields};
pub use writer_support::BaseEventIdentityLookup;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    SourceKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
};
#[cfg(test)]
use ctx_history_core::{StableEntityId, IDENTITY_VERSION};
#[cfg(test)]
use tantivy::TantivyDocument;
use tantivy::{
    collector::Count,
    directory::{error::LockError, Directory, DirectoryLock, Lock, INDEX_WRITER_LOCK},
    indexer::LogMergePolicy,
    query::TermQuery,
    schema::{Field, IndexRecordOption},
    Index, IndexWriter, ReloadPolicy, Searcher, Term,
};
use uuid::Uuid;

use durable_directory::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};
use index_document::{core_content_bytes, IndexDocument, IndexSourceFields, SourceToken};
use staging::{finish_identical_staging, PendingSource as StagedPendingSource, PendingSourceMode};
use writer_support::{
    acquire_generation_writer_lock_with_retry, acquire_preflight_writer_lock_with_retry,
    construct_index_writer_with_retry, ExactReplayInventoryWitness, PendingSource,
};

pub struct GenerationWriter {
    root: PathBuf,
    index: Index,
    active_pointer: Option<ActiveGenerationPointer>,
    candidate_directory_name: Option<String>,
    preflight_lock: Option<DirectoryLock>,
    writer: Option<IndexWriter<IndexDocument>>,
    writer_options: WriterOptions,
    fields: Fields,
    base_manifest: Option<GenerationManifest>,
    base_opstamp: u64,
    base_searcher: Option<Searcher>,
    complete_inventories: Vec<CertifiedSourceInventory>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, GenerationRemoval>,
    observed_missing: HashMap<SourceKey, SourceCatalogMissingState>,
    source_identities: HashMap<Uuid, [u8; 32]>,
    checked_base_sessions: HashSet<Uuid>,
    staged_event_identities: HashMap<Uuid, [u8; 32]>,
    staged_session_identities: HashMap<Uuid, [u8; 32]>,
    #[cfg(test)]
    index_writer_constructions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    before_writer_handoff: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    after_candidate_commit: Option<Box<dyn FnOnce(&Path) + Send>>,
    #[cfg(test)]
    before_pointer_switch: Option<Box<dyn FnOnce(&Path) + Send>>,
    #[cfg(test)]
    after_pointer_switch: Option<Box<dyn FnOnce(&Path) + Send>>,
}

impl GenerationWriter {
    /// Captures an exact event-identity lookup pinned to this writer's base generation.
    pub fn base_event_identity_lookup(&self) -> BaseEventIdentityLookup {
        BaseEventIdentityLookup {
            searcher: self.base_searcher.clone(),
            event_id_field: self.fields.event_id,
        }
    }

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
        let generation_writer_lock = Lock {
            filepath: PathBuf::from(".ctx-generation-writer.lock"),
            is_blocking: false,
        };
        let preflight_lock =
            acquire_generation_writer_lock_with_retry(&directory, &generation_writer_lock)?;
        reclaim_abandoned_atomic_writes(&root)?;
        reclaim_abandoned_atomic_writes(&root.join(MANIFEST_DIRECTORY))?;

        let mut active_pointer = load_active_generation_pointer(&root)?;
        if active_pointer.is_none()
            && Index::exists(&directory).map_err(tantivy::TantivyError::from)?
        {
            let legacy_lock = acquire_preflight_writer_lock_with_retry(&directory)?;
            let legacy_index = Index::open(directory.clone())?;
            analyzer::register_body_analyzer(&legacy_index);
            validate_schema(&legacy_index.schema())?;
            let legacy_metas = legacy_index.load_metas()?;
            if legacy_metas.payload.is_some() {
                let manifest = load_manifest_for_metas(&root, &legacy_metas)?;
                active_pointer = Some(migrate_legacy_generation(&root, &legacy_index, &manifest)?);
            } else if !legacy_metas.segments.is_empty() {
                return Err(IndexError::UnboundIndexState);
            }
            drop(legacy_lock);
        }
        reclaim_inactive_generation_directories(&root, active_pointer.as_ref())?;
        let retained_manifests = active_pointer
            .iter()
            .flat_map(|pointer| {
                std::iter::once(pointer.active()).chain(pointer.previous().into_iter())
            })
            .map(|slot| slot.generation_id().to_owned())
            .collect::<Vec<_>>();
        reclaim_unreferenced_manifests(&root, &retained_manifests)?;

        let (index, candidate_directory_name) = if let Some(pointer) = &active_pointer {
            (open_slot_index(&root, pointer.active())?, None)
        } else {
            let candidate = create_candidate_generation(&root, None)?;
            (candidate.index, Some(candidate.directory_name))
        };
        let fields = fields_from_schema(&index.schema())?;
        validate_schema(&index.schema())?;
        let base_metas = index.load_metas()?;
        let (base_manifest, base_searcher) = if base_metas.payload.is_some() {
            let manifest = load_manifest_for_metas(&root, &base_metas)?;
            if let Some(pointer) = &active_pointer {
                if pointer.active().generation_id() != manifest.generation_id()? {
                    return Err(IndexError::InvalidActiveGenerationPointer);
                }
            }
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            let searcher = reader.searcher();
            if searcher_generation(&searcher) != meta_generation(&base_metas) {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            verify_searcher_structure(&searcher, &manifest)?;
            (Some(manifest), Some(searcher))
        } else if base_metas.segments.is_empty() {
            (None, None)
        } else {
            return Err(IndexError::UnboundIndexState);
        };
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
            active_pointer,
            candidate_directory_name,
            preflight_lock: Some(preflight_lock),
            writer: None,
            writer_options: WriterOptions {
                indexer_threads,
                memory_bytes: options.memory_bytes,
            },
            fields,
            base_manifest,
            base_opstamp: base_metas.opstamp,
            base_searcher,
            complete_inventories: Vec::new(),
            pending: HashMap::new(),
            deletions: HashMap::new(),
            observed_missing: HashMap::new(),
            source_identities,
            checked_base_sessions: HashSet::new(),
            staged_event_identities: HashMap::new(),
            staged_session_identities: HashMap::new(),
            #[cfg(test)]
            index_writer_constructions: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            before_writer_handoff: None,
            #[cfg(test)]
            after_candidate_commit: None,
            #[cfg(test)]
            before_pointer_switch: None,
            #[cfg(test)]
            after_pointer_switch: None,
        })
    }

    /// Returns the base generation captured after this writer acquired
    /// Tantivy's exclusive writer lock.
    pub fn base_manifest(&self) -> Option<&GenerationManifest> {
        self.base_manifest.as_ref()
    }

    /// Registers one complete provider inventory captured by the current
    /// refresh. Exact no-op admission requires these inventories to cover the
    /// full retained/removal set and requires a separate terminal callback to
    /// revalidate each exact certificate.
    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        inventory.validate_contract()?;
        let observation = inventory.observation();
        if self.complete_inventories.iter().any(|existing| {
            let existing = existing.observation();
            existing.provider() == observation.provider()
                && existing.authority_namespace() == observation.authority_namespace()
                && existing.authority_key() == observation.authority_key()
        }) {
            return Err(IndexError::DuplicateCompleteInventoryAuthority {
                provider: observation.provider().to_owned(),
                authority_namespace: observation.authority_namespace().to_owned(),
            });
        }
        self.complete_inventories.push(inventory);
        Ok(())
    }

    fn exact_replay_inventory_witness(&self) -> Result<Option<ExactReplayInventoryWitness<'_>>> {
        if self.writer.is_some() || !self.deletions.is_empty() {
            return Ok(None);
        }
        let Some(base) = self.base_manifest.as_ref() else {
            return Ok(None);
        };
        if !self.observed_missing.is_empty() || !base.source_catalog().is_empty() {
            // Missing-state creation, advancement, and reset are manifest
            // mutations even though last-good Core documents stay untouched.
            return Ok(None);
        }

        // A no-work candidate is a full-inventory claim. Do not silently turn
        // an omitted prior source into an unchanged Tantivy publication.
        if let Some(missing) = base.sources.iter().find(|base_source| {
            !self
                .pending
                .contains_key(&source_token(base_source.observation().source()))
        }) {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.observation().source().identity().to_string(),
            });
        }
        if self.pending.len() != base.sources.len() {
            return Ok(None);
        }

        let retained_sources_are_exact = base.sources.iter().all(|base_source| {
            self.pending
                .get(&source_token(base_source.observation().source()))
                .is_some_and(|pending| {
                    matches!(
                        (&pending.mode, &pending.certificate),
                        (
                            PendingSourceMode::Append { base }
                                | PendingSourceMode::Retain { base },
                            Some(current),
                        )
                            if pending.staged_documents == 0
                                && base == base_source
                                && current == base_source
                    )
                })
        });
        if !retained_sources_are_exact {
            return Ok(None);
        }
        if self.complete_inventories.is_empty() {
            // Without a certified current inventory this is not an admissible
            // exact no-op. Preserve compatibility for mutation-oriented
            // callers by taking the ordinary IndexWriter publication path.
            return Ok(None);
        }

        let mut covered_sources = HashSet::new();
        for inventory in &self.complete_inventories {
            let newly_matched = base
                .sources
                .iter()
                .filter(|source| inventory.contains(source.observation().source()))
                .filter(|source| {
                    covered_sources.insert(source.observation().source().identity().digest())
                })
                .count();
            if newly_matched != inventory.observed_sources() {
                return Err(IndexError::ExactReplayInventoryCountMismatch {
                    provider: inventory.observation().provider().to_owned(),
                    observed: inventory.observed_sources(),
                    matched: newly_matched,
                });
            }
        }
        if let Some(missing) = base.sources.iter().find(|source| {
            !covered_sources.contains(&source.observation().source().identity().digest())
        }) {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.observation().source().identity().to_string(),
            });
        }
        if let Some(missing) = base.removals.iter().find(|removal| {
            !self
                .complete_inventories
                .iter()
                .any(|inventory| removal.deletion().verifies(inventory))
        }) {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.source().identity().to_string(),
            });
        }

        Ok(Some(ExactReplayInventoryWitness { base }))
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
        let source_key_field = self.fields.source_key;
        self.writer_mut()?
            .delete_term(Term::from_field_text(source_key_field, &token));
        self.deletions.remove(&source);
        let index_fields = IndexSourceFields::new(&source, &token);
        self.pending.insert(
            token,
            PendingSource {
                index_fields,
                staged: StagedPendingSource {
                    source,
                    mode: PendingSourceMode::Replace,
                    staged_documents: 0,
                    certificate: None,
                    core_record_accumulator: [0; 32],
                },
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
                index_fields: IndexSourceFields::new(&source, &token),
                staged: StagedPendingSource {
                    source,
                    mode: PendingSourceMode::Append { base },
                    staged_documents: 0,
                    certificate: None,
                    core_record_accumulator: [0; 32],
                },
            },
        );
        let pending = self
            .pending
            .get(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        match &pending.mode {
            PendingSourceMode::Append { base } => Ok(base),
            PendingSourceMode::Replace | PendingSourceMode::Retain { .. } => {
                Err(IndexError::DocumentSourceNotActive)
            }
        }
    }

    /// Adds one complete generation-owned Core record.
    ///
    /// This is the canonical write API. No provider read locator is accepted,
    /// synthesized, or persisted by this path.
    pub fn add_core_record(&mut self, mut record: CoreRecord) -> Result<()> {
        if record.normalization_revision != CORE_NORMALIZATION_REVISION
            || record.content.policy_revision != CORE_CONTENT_POLICY_REVISION
        {
            return Err(IndexError::CoreRecordPolicyRevisionMismatch {
                normalization: record.normalization_revision,
                expected_normalization: CORE_NORMALIZATION_REVISION,
                content: record.content.policy_revision,
                expected_content: CORE_CONTENT_POLICY_REVISION,
            });
        }
        let source_digest = record.source.identity().digest();
        let token = SourceToken::new(&source_digest);
        let token = token.as_str()?;
        let pending_source = match self.pending.get(token) {
            Some(pending) if pending.source.exact_descriptor_eq(&record.source) => pending,
            _ => return Err(IndexError::DocumentSourceNotActive),
        };
        let is_append = matches!(&pending_source.mode, PendingSourceMode::Append { .. });
        if matches!(&pending_source.mode, PendingSourceMode::Retain { .. }) {
            return Err(IndexError::DocumentSourceNotActive);
        }
        let mut core_record_bytes = record.encode_stored()?;
        if record.needs_prior_repository_certificate() {
            if let Some(base_searcher) = &self.base_searcher {
                if let Some(prior) =
                    prior_core_record(base_searcher, self.fields, record.event_id, &record.source)?
                {
                    if record.reuse_prior_repository_certificate(&prior) {
                        core_record_bytes = record.encode_stored()?;
                    }
                }
            }
        }
        let record_leaf = staging::core_record_leaf(record.event_id, &core_record_bytes)?;
        let core_content_bytes = core_content_bytes(&record.content)?;
        let index_fields = pending_source.index_fields.clone();
        if let Some(base_searcher) = &self.base_searcher {
            validate_event_identity_against_base(
                base_searcher,
                self.fields,
                record.event_id,
                token,
                !is_append,
            )?;
            if self
                .checked_base_sessions
                .insert(record.session_id.as_uuid())
            {
                validate_session_identity_against_base(
                    base_searcher,
                    self.fields,
                    record.session_id,
                    token,
                )?;
            }
            for related_session_id in record
                .parent_session_id
                .into_iter()
                .chain(std::iter::once(record.root_session_id))
            {
                if related_session_id != record.session_id
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
        register_session_identity(&mut self.staged_session_identities, record.session_id)?;
        if let Some(parent_session_id) = record.parent_session_id {
            register_session_identity(&mut self.staged_session_identities, parent_session_id)?;
        }
        register_session_identity(&mut self.staged_session_identities, record.root_session_id)?;
        register_event_identity(&mut self.staged_event_identities, record.event_id)?;
        let target = IndexDocument::from_core(
            self.fields,
            record,
            core_record_bytes,
            core_content_bytes,
            index_fields,
        )?;
        self.writer_mut()?.add_document(target)?;
        let pending = self
            .pending
            .get_mut(token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        staging::accumulate_core_record(&mut pending.core_record_accumulator, &record_leaf);
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
        let source_key_field = self.fields.source_key;
        self.writer_mut()?
            .delete_term(Term::from_field_text(source_key_field, &token));
        self.deletions.insert(source.clone(), removal);
        Ok(())
    }

    /// Records one automatic-acquisition complete inventory that omitted a
    /// retained source, deleting only after the configured consecutive bound.
    ///
    /// The source remains in the generation while deferred. Its live source
    /// certificate is intentionally not revalidated; the current complete
    /// inventory is the terminal evidence for this refresh.
    pub fn observe_automatic_source_missing(
        &mut self,
        proof: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
        observed_at_unix_ms: u64,
        delete_after_consecutive_inventories: u32,
    ) -> Result<bool> {
        if delete_after_consecutive_inventories < 2 {
            return Err(IndexError::InvalidSourceDeletionGraceThreshold);
        }
        GenerationRemoval::new(proof.clone(), inventory.clone())?;
        let source = proof.source().clone();
        let source_id = source.identity().to_string();
        let token = source_token(&source);
        if self.pending.contains_key(&token)
            || self.deletions.contains_key(&source)
            || self.observed_missing.contains_key(&source)
        {
            return Err(IndexError::DuplicateSourceMissingObservation(source_id));
        }
        let (base_generation, previous) = {
            let base = self.base_manifest.as_ref().ok_or_else(|| {
                IndexError::SourceMissingObservationNotRetained(source_id.clone())
            })?;
            let retained = base.sources.iter().any(|candidate| {
                candidate
                    .observation()
                    .source()
                    .exact_descriptor_eq(&source)
            });
            if !retained {
                return Err(IndexError::SourceMissingObservationNotRetained(source_id));
            }
            (
                base.generation_id()?,
                base.source_catalog().missing_source(&source).cloned(),
            )
        };
        let observation = SourceMissingObservationPoint::new(base_generation, observed_at_unix_ms)?;
        let state = match previous {
            Some(previous) => previous.advance(proof.clone(), observation)?,
            None => SourceCatalogMissingState::first(proof.clone(), observation),
        };
        if state.consecutive_missing().get() >= delete_after_consecutive_inventories {
            self.delete_source(proof, inventory)?;
            return Ok(true);
        }
        self.observed_missing.insert(source, state);
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
