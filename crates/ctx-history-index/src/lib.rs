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

pub use durable_directory::durable_atomic_replace_file;

pub(crate) use contracts::{
    CommitPayload, COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD, MANIFEST_DIRECTORY,
    MAX_DOCUMENT_METADATA_BYTES,
};
pub use contracts::{
    CommitReceipt, GenerationManifest, GenerationRemoval, IndexError, Result, RevalidationTarget,
    WriterOptions, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
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
    classify_publication_failure, load_manifest_for_metas, meta_generation, payload_generation_id,
    reclaim_unreferenced_manifests, reconcile_commit_error, searcher_generation, sync_directory,
    verify_searcher, verify_searcher_structure, write_manifest,
};
pub use query::{
    AgentScope, CoreEventBatch, CoreEventPageBudget, CoreEventRecord, CoreSemanticEventPage,
    CoreSourceEventPage, EventRecord, EventSearchCandidate, EventSearchFilters,
    ExcludedSessionTree, SemanticEligibility, SemanticEventCursor, SemanticEventPage,
    SessionEventCoordinate, SessionRecord, SourceEventCursor, SourceEventPage,
    DEFAULT_CORE_EVENT_PAGE_BUDGET, MAX_SEMANTIC_EVENT_PAGE_ITEMS,
    MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
pub use reader::VerifiedIndex;
pub(crate) use schema::{
    fields_from_schema, lexical_schema, required_field, validate_schema, Fields,
};

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
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
    directory::{Directory, DirectoryLock, Lock, INDEX_WRITER_LOCK},
    indexer::LogMergePolicy,
    query::TermQuery,
    schema::{Field, IndexRecordOption},
    Index, IndexMeta, IndexSettings, IndexWriter, ReloadPolicy, Searcher, Term,
};
use uuid::Uuid;

use durable_directory::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};
use index_document::{core_content_bytes, IndexDocument, IndexSourceFields, SourceToken};
use staging::{finish_identical_staging, PendingSource as StagedPendingSource, PendingSourceMode};

struct PendingSource {
    index_fields: IndexSourceFields,
    staged: StagedPendingSource,
}

impl std::ops::Deref for PendingSource {
    type Target = StagedPendingSource;

    fn deref(&self) -> &Self::Target {
        &self.staged
    }
}

impl std::ops::DerefMut for PendingSource {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.staged
    }
}

fn reclaim_orphaned_managed_files(index: &mut Index, base_metas: &IndexMeta) -> Result<()> {
    let mut living_files = base_metas
        .segments
        .iter()
        .flat_map(|segment| segment.list_files())
        .collect::<HashSet<_>>();
    living_files.insert(PathBuf::from("meta.json"));

    let has_orphaned_managed_files = index
        .directory()
        .list_managed_files()
        .iter()
        .any(|path| !living_files.contains(path));
    if !has_orphaned_managed_files {
        return Ok(());
    }

    // Use Tantivy's managed-file ledger so an interrupted or failed cleanup is
    // retried safely. The live set comes only from the verified active
    // meta.json, so recovery cannot remove any file in the pinned generation.
    let _ = index.directory_mut().garbage_collect(|| living_files)?;
    Ok(())
}

/// Exact replay plus current inventories; the prior manifest is comparison state only.
struct ExactReplayInventoryWitness<'a> {
    base: &'a GenerationManifest,
}

pub struct GenerationWriter {
    root: PathBuf,
    index: Index,
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
    source_identities: HashMap<Uuid, [u8; 32]>,
    checked_base_sessions: HashSet<Uuid>,
    staged_event_identities: HashMap<Uuid, [u8; 32]>,
    staged_session_identities: HashMap<Uuid, [u8; 32]>,
    #[cfg(test)]
    index_writer_constructions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    before_writer_handoff: Option<Box<dyn FnOnce() + Send>>,
}

/// Exact point lookup over the immutable generation captured when a writer opened.
///
/// Provider append adapters use this to resolve a small suffix against existing
/// deterministic identities without enumerating or decoding the validated prefix.
#[derive(Clone)]
pub struct BaseEventIdentityLookup {
    searcher: Option<Searcher>,
    event_id_field: Field,
}

impl BaseEventIdentityLookup {
    /// Returns whether the immutable base generation contains `event_id`.
    pub fn contains(&self, event_id: Uuid) -> Result<bool> {
        let Some(searcher) = self.searcher.as_ref() else {
            return Ok(false);
        };
        let query = TermQuery::new(
            Term::from_field_text(self.event_id_field, &event_id.to_string()),
            IndexRecordOption::Basic,
        );
        match searcher.search(&query, &Count)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(IndexError::DuplicateEventIdentity(event_id.to_string())),
        }
    }
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
        let mut index = if Index::exists(&directory).map_err(tantivy::TantivyError::from)? {
            Index::open(directory.clone())?
        } else {
            Index::create(
                directory.clone(),
                lexical_schema(),
                IndexSettings::default(),
            )?
        };
        analyzer::register_body_analyzer(&index);
        drop(initialization_guard);
        let fields = fields_from_schema(&index.schema())?;
        validate_schema(&index.schema())?;
        // Hold Tantivy's real writer lock while capturing and staging against
        // the base, but defer construction of IndexWriter and its worker/memory
        // floor until the first actual mutation.
        let preflight_lock = directory
            .acquire_lock(&INDEX_WRITER_LOCK)
            .map_err(|error| {
                tantivy::TantivyError::LockFailure(
                    error,
                    Some(
                        "Failed to acquire index lock. If you are using a regular directory, this \
                         means there is already an `IndexWriter` working on this `Directory`, in \
                         this process or in a different process."
                            .to_owned(),
                    ),
                )
            })?;
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
            // The immutable generation passed the exhaustive audit before its
            // publication receipt was returned. Reopening a base only needs
            // to bind its manifest, Tantivy generation, and aggregate count;
            // repeating the O(document-count) identity audit would make an
            // exact no-op refresh scale with corpus size.
            verify_searcher_structure(&searcher, &manifest)?;
            (Some(manifest), Some(searcher))
        } else if base_metas.segments.is_empty() {
            (None, None)
        } else {
            return Err(IndexError::UnboundIndexState);
        };
        // Base verification above and both reclamation paths run while holding
        // Tantivy's real writer lock. Interrupted publications are safe to
        // remove only after the visible base has been proven. The managed-file
        // path remains completely idle for a healthy exact replay.
        reclaim_abandoned_atomic_writes(&root)?;
        reclaim_abandoned_atomic_writes(&root.join(MANIFEST_DIRECTORY))?;
        let visible_generation_id = payload_generation_id(&base_metas)?;
        reclaim_unreferenced_manifests(&root, visible_generation_id.as_deref())?;
        reclaim_orphaned_managed_files(&mut index, &base_metas)?;
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
            source_identities,
            checked_base_sessions: HashSet::new(),
            staged_event_identities: HashMap::new(),
            staged_session_identities: HashMap::new(),
            #[cfg(test)]
            index_writer_constructions: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            before_writer_handoff: None,
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

    fn writer_mut(&mut self) -> Result<&mut IndexWriter<IndexDocument>> {
        if self.writer.is_none() {
            let preflight_lock = self
                .preflight_lock
                .take()
                .ok_or(IndexError::WriterInvariant(
                    "missing preflight lock before lazy writer construction",
                ))?;
            drop(preflight_lock);

            #[cfg(test)]
            if let Some(hook) = self.before_writer_handoff.take() {
                hook();
            }

            let writer = self.index.writer_with_num_threads::<IndexDocument>(
                self.writer_options.indexer_threads,
                self.writer_options.memory_bytes,
            )?;
            #[cfg(test)]
            self.index_writer_constructions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current_metas = self.index.load_metas()?;
            let expected_generation = self
                .base_manifest
                .as_ref()
                .map(GenerationManifest::generation_id)
                .transpose()?;
            let current_generation = payload_generation_id(&current_metas)?;
            let expected_segments = self
                .base_searcher
                .as_ref()
                .map(searcher_generation)
                .unwrap_or_default();
            if current_metas.opstamp != self.base_opstamp
                || current_generation != expected_generation
                || meta_generation(&current_metas) != expected_segments
            {
                return Err(IndexError::ConcurrentGenerationChange);
            }

            let mut merge_policy = LogMergePolicy::default();
            merge_policy.set_min_num_segments(LEXICAL_SEGMENT_MERGE_FAN_IN);
            writer.set_merge_policy(Box::new(merge_policy));

            // Mutation now owns Tantivy's writer lock. Managed-file garbage
            // collection is intentionally lazy so a healthy exact replay does
            // no IndexWriter or segment-inventory work.
            let _ = writer.garbage_collect_files().wait()?;
            self.writer = Some(writer);
        }
        self.writer.as_mut().ok_or(IndexError::WriterInvariant(
            "lazy writer construction completed without a writer",
        ))
    }

    fn exact_replay_inventory_witness(&self) -> Result<Option<ExactReplayInventoryWitness<'_>>> {
        if self.writer.is_some() || !self.deletions.is_empty() {
            return Ok(None);
        }
        let Some(base) = self.base_manifest.as_ref() else {
            return Ok(None);
        };

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

    /// Publishes one atomic lexical generation.
    ///
    /// `revalidate` runs after Tantivy has flushed all staged indexing workers
    /// and immediately before the immutable manifest and `meta.json` commit.
    pub fn commit<F>(self, revalidate: F) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
    {
        self.commit_with_complete_inventory_revalidation(revalidate, |_| false)
    }

    /// Publishes one atomic lexical generation with terminal revalidation for
    /// each current complete-inventory certificate registered on the writer.
    ///
    /// The second callback is a distinct target because an authoritative
    /// inventory remains meaningful when it contains zero retained sources
    /// and zero removals.
    pub fn commit_with_complete_inventory_revalidation<F, I>(
        mut self,
        mut revalidate: F,
        mut revalidate_inventory: I,
    ) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        if let Some(witness) = self.exact_replay_inventory_witness()? {
            for certificate in &witness.base.sources {
                if !revalidate(RevalidationTarget::Source(certificate)) {
                    return Err(IndexError::SourceInvalidated(
                        certificate.observation().source().identity().to_string(),
                    ));
                }
            }
            for inventory in &self.complete_inventories {
                if !revalidate_inventory(inventory) {
                    return Err(IndexError::CompleteInventoryInvalidated {
                        provider: inventory.observation().provider().to_owned(),
                        authority_namespace: inventory
                            .observation()
                            .authority_namespace()
                            .to_owned(),
                    });
                }
            }
            return CommitReceipt::from_manifest(self.base_opstamp, witness.base.clone());
        }

        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }

        let manifest = self.next_manifest()?;
        if let Some(receipt) = finish_identical_staging(
            &mut self,
            &manifest,
            &mut revalidate,
            &mut revalidate_inventory,
        )? {
            return Ok(receipt);
        }

        self.writer_mut()?;
        let previous_generation_id = self
            .base_manifest
            .as_ref()
            .map(GenerationManifest::generation_id)
            .transpose()?;
        let root = self.root.clone();
        let mut prepared = self
            .writer
            .as_mut()
            .ok_or(IndexError::WriterInvariant(
                "mutating commit is missing its lazy writer",
            ))?
            .prepare_commit()?;
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
        for inventory in &self.complete_inventories {
            if !revalidate_inventory(inventory) {
                let error = IndexError::CompleteInventoryInvalidated {
                    provider: inventory.observation().provider().to_owned(),
                    authority_namespace: inventory.observation().authority_namespace().to_owned(),
                };
                prepared.abort()?;
                return Err(error);
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
        let writer = self.writer.take().ok_or(IndexError::WriterInvariant(
            "published commit is missing its lazy writer",
        ))?;
        if let Err(error) = writer.wait_merging_threads() {
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
        let verified = (|| -> Result<VerifiedIndex> {
            // Every document admitted by this writer has already been checked
            // against the audited base and the current staging identities.
            // Rewalking the immutable base made tiny appends O(total documents).
            // Bind the generation structurally, then verify changed postings;
            // the previously audited base remains byte-immutable.
            let verified = VerifiedIndex::open_pinned(&root)?;
            staging::verify_published_mutations(&self, &verified)?;
            Ok(verified)
        })()
        .map_err(|error| IndexError::CommittedGenerationNeedsRecovery {
            generation_id: generation_id.clone(),
            stage: "generation verification",
            detail: error.to_string(),
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

        CommitReceipt::from_manifest(opstamp, manifest)
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

#[cfg(test)]
mod tests;
