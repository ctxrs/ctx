//! Atomic self-contained lexical Core generations.
//!
//! A Tantivy commit names a durable immutable source-revision manifest, so
//! readers observe either the previous complete generation or the next one.

mod analyzer;
mod commit_contract;
mod contracts;
mod durable_directory;
mod identity;
mod index_document;
mod merge_policy;
pub mod policy;
mod preparation;
mod publication;
mod query;
mod reader;
mod schema;
mod staging;
mod writer_publication;
mod writer_routes;
mod writer_support;

pub use durable_directory::durable_atomic_replace_file;

pub use commit_contract::{
    CommitReceipt, PublicationDisposition, PublicationMetadataContext, PublishedGeneration,
    RevalidationTarget,
};

pub(crate) use contracts::{
    CommitPayload, COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD, MANIFEST_DIRECTORY,
    MAX_DOCUMENT_METADATA_BYTES,
};
pub use contracts::{
    ConsecutiveSourceMissingCount, GenerationManifest, IndexError, Result,
    SourceCoreRecordAggregate, SourceMissingObservationPoint, SourceRouteIdentity,
    SourceRouteMissingState, SourceRouteSnapshot, WriterOptions, GENERATION_MANIFEST_VERSION,
    LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION, LEXICAL_SEGMENT_MERGE_FAN_IN,
    MAX_PUBLICATION_METADATA_BYTES,
};
pub use ctx_history_core::CoreRecord;
pub(crate) use identity::{
    hex, is_generation_id, prior_core_record, register_compact_identity, sha256_hex,
    source_sort_key, source_token,
};
pub use policy::{
    current_semantic_generation_policy, current_semantic_generation_policy_hash,
    current_source_generation_policy, current_source_generation_policy_hash,
    EmbeddingGenerationPolicy, LexicalBodySelection, LexicalGenerationPolicy,
    LexicalIndexedBodyLimit, SemanticCoreContentFilter, SemanticGenerationPolicy, SourceEventClass,
    SourceEventRole, SourceGenerationPolicy, StoredSourceContent, LEXICAL_INDEXED_BODY_LIMIT,
    LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION, SEMANTIC_CHUNK_OVERLAP_CHARS,
    SEMANTIC_CHUNK_TARGET_CHARS, SEMANTIC_EMBEDDING_CONTRACT_REVISION,
    SEMANTIC_EMBEDDING_DIMENSIONS, SEMANTIC_EMBEDDING_MODEL, SEMANTIC_EMBEDDING_MODEL_REVISION,
    SEMANTIC_EMBEDDING_NORMALIZATION, SEMANTIC_SOURCE_MAX_CHARS,
};
pub use preparation::{CoreRecordPreparer, PreparedCoreRecord};
#[cfg(test)]
pub(crate) use publication::manifest_path;
pub(crate) use publication::{
    canonical_commit_payload, create_candidate_generation, load_active_generation_pointer,
    load_publication_for_metas, meta_generation, open_slot_index, payload_generation_id,
    physical_integrity_digest, publish_active_generation_pointer,
    reclaim_inactive_generation_directories, reclaim_unreferenced_manifests,
    reconcile_commit_error, searcher_generation, sync_directory, sync_generation,
    verify_complete_searcher, verify_physical_integrity, verify_searcher,
    verify_searcher_structure, write_manifest, ActiveGenerationPointer, GenerationSlot,
    INDEX_GENERATIONS_DIRECTORY,
};
pub use query::{
    AgentScope, CoreEventBatch, CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeDirection,
    CoreEventRangeDomain, CoreEventRangeError, CoreEventRangeFilters, CoreEventRangePage,
    CoreEventRangeScope, CoreEventRangeSelection, CoreEventRecord, CoreSemanticEventPage,
    CoreSessionEventPage, CoreSourceEventPage, CoreSourceEventPagePlan, EventRecord,
    EventSearchCandidate, EventSearchFilters, ExcludedSessionTree, LexicalQueryLimits,
    SemanticEligibility, SemanticEventCursor, SemanticEventPage, SessionEventCoordinate,
    SessionEventCursor, SessionRecord, SourceEventCursor, SourceEventPage, StoredCoreEventRecord,
    StoredCoreRecordJson, StoredCoreSourceEventPage, DEFAULT_CORE_EVENT_PAGE_BUDGET,
    LEXICAL_QUERY_LIMITS, MAX_CORE_EVENT_RANGE_PAGE_ITEMS, MAX_LEXICAL_QUERY_RESULTS,
    MAX_SEMANTIC_EVENT_PAGE_ITEMS, MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS,
    MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS, MAX_SESSION_EVENT_PAGE_ITEMS,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
pub use reader::VerifiedIndex;
#[cfg(test)]
pub(crate) use schema::required_field;
pub(crate) use schema::{fields_from_schema, lexical_schema, validate_schema, Fields};
pub use writer_support::BaseEventIdentityLookup;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(test)]
use ctx_history_core::IDENTITY_VERSION;
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    SourceKey,
};
#[cfg(test)]
use tantivy::directory::INDEX_WRITER_LOCK;
#[cfg(test)]
use tantivy::TantivyDocument;
use tantivy::{
    collector::Count,
    directory::{error::LockError, Directory, DirectoryLock, Lock},
    query::TermQuery,
    schema::{Field, IndexRecordOption},
    Index, IndexWriter, ReloadPolicy, Searcher, Term,
};
use uuid::Uuid;

use durable_directory::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};
use index_document::IndexDocument;
#[cfg(test)]
use index_document::{core_content_bytes, IndexSourceFields};
use merge_policy::LexicalMergePolicy;
use staging::{finish_identical_staging, PendingSource as StagedPendingSource, PendingSourceMode};
use writer_support::{
    acquire_generation_writer_lock_with_retry, clear_active_generation_rebuild_marker,
    construct_index_writer_with_retry, load_active_generation_rebuild_marker,
    ExactReplayInventoryWitness, PendingSource,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedMissingRouteOutcome {
    retained_sources: Vec<SourceKey>,
    deleted: bool,
}

impl CertifiedMissingRouteOutcome {
    pub fn retained_sources(&self) -> &[SourceKey] {
        &self.retained_sources
    }

    pub fn deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Debug, Clone)]
struct PendingDeletion {
    proof: CertifiedSourceDeletion,
}

#[derive(Debug, Clone)]
struct SourceRoutePlan {
    selected: BTreeSet<SourceRouteIdentity>,
    carried_from_base: BTreeSet<SourceRouteIdentity>,
    completed: BTreeSet<SourceRouteIdentity>,
}

#[derive(Clone)]
struct SourceRouteStageCheckpoint {
    route_identity: SourceRouteIdentity,
    complete_inventories: Vec<CertifiedSourceInventory>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, PendingDeletion>,
    route_deletions: HashSet<SourceKey>,
    observed_missing_routes: HashMap<SourceRouteIdentity, SourceRouteSnapshot>,
    missing_route_revalidation_len: usize,
    source_identities: HashMap<Uuid, [u8; 32]>,
}

impl PendingDeletion {
    fn new(proof: CertifiedSourceDeletion, inventory: CertifiedSourceInventory) -> Result<Self> {
        proof.validate_contract()?;
        inventory.validate_contract()?;
        if !proof.verifies(&inventory) {
            return Err(IndexError::InvalidCertifiedSourceDeletion(
                proof.source().identity().to_string(),
            ));
        }
        Ok(Self { proof })
    }

    fn source(&self) -> &SourceKey {
        self.proof.source()
    }
}

/// Returns whether an active disposable generation is structurally incompatible
/// with this build and therefore must be replaced from source authority.
///
/// These errors describe versioned pointer, schema, policy, or physical index
/// settings, not damaged control metadata. Callers must not read, clone,
/// migrate, or otherwise interpret the incompatible generation.
pub fn generation_incompatibility_requires_rebuild(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::UnsupportedActiveGenerationPointer(_)
            | IndexError::UnsupportedCommitPayload(_)
            | IndexError::UnsupportedManifest(_)
            | IndexError::GenerationContractMismatch { .. }
            | IndexError::CoreRecordContractMismatch { .. }
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::GenerationPolicyMismatch { .. }
            | IndexError::SchemaMismatch(_)
            | IndexError::IndexSettingsMismatch(_)
            | IndexError::ChecksumMismatch
    )
}

#[cfg(test)]
type GenerationPathHook = Box<dyn FnOnce(&Path) + Send>;

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
    base_publication_metadata: Option<std::sync::Arc<[u8]>>,
    core_record_preparer: CoreRecordPreparer,
    complete_inventories: Vec<CertifiedSourceInventory>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, PendingDeletion>,
    route_deletions: HashSet<SourceKey>,
    present_source_routes: Option<Vec<SourceRouteSnapshot>>,
    observed_missing_routes: HashMap<SourceRouteIdentity, SourceRouteSnapshot>,
    missing_route_revalidations: Vec<(SourceRouteIdentity, Box<dyn Fn() -> bool + Send + 'static>)>,
    source_identities: HashMap<Uuid, [u8; 32]>,
    source_route_plan: Option<SourceRoutePlan>,
    active_source_route_stage: Option<SourceRouteStageCheckpoint>,
    #[cfg(test)]
    index_writer_constructions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    before_writer_handoff: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    before_candidate_commit: Option<GenerationPathHook>,
    #[cfg(test)]
    after_candidate_commit: Option<GenerationPathHook>,
    #[cfg(test)]
    return_commit_error_after_visibility: bool,
    #[cfg(test)]
    before_pointer_switch: Option<GenerationPathHook>,
    #[cfg(test)]
    before_pointer_publication: Option<GenerationPathHook>,
    #[cfg(test)]
    after_pointer_switch: Option<GenerationPathHook>,
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

        let (active_pointer, pointer_requires_rebuild) = match load_active_generation_pointer(&root)
        {
            Ok(pointer) => (pointer, false),
            Err(error) if generation_incompatibility_requires_rebuild(&error) => (None, true),
            Err(error) => return Err(error),
        };
        if !pointer_requires_rebuild {
            reclaim_inactive_generation_directories(&root, active_pointer.as_ref())?;
            let retained_generation_ids = active_pointer
                .iter()
                .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
                .map(|slot| slot.generation_id().to_owned())
                .collect::<Vec<_>>();
            reclaim_unreferenced_manifests(&root, &retained_generation_ids)?;
        }

        let rebuild_marked = if pointer_requires_rebuild {
            // The unsupported pointer remains the sole durable publication
            // authority until a complete current candidate atomically replaces
            // it. Its slots and manifests are intentionally not decoded or
            // reclaimed during staging.
            true
        } else if let Some(marker) = load_active_generation_rebuild_marker(&root)? {
            if active_pointer.as_ref().is_some_and(|pointer| {
                pointer.active().generation_id() == marker.generation_id
                    && pointer.active().directory() == marker.directory
            }) {
                // The prior physical integrity check failed. Keep serving the
                // old pointer until a fresh source-authoritative candidate is
                // verified and atomically replaces it, but do not expose the
                // corrupt generation as reusable base state.
                true
            } else {
                // Publication completed after the marker was written but before
                // its cleanup. It no longer applies to the active generation.
                clear_active_generation_rebuild_marker(&root)?;
                false
            }
        } else {
            false
        };

        let reusable_generation = if !rebuild_marked {
            active_pointer
                .as_ref()
                .map(|pointer| {
                    let index = open_slot_index(&root, pointer.active())?;
                    validate_schema(&index.schema())?;
                    let fields = fields_from_schema(&index.schema())?;
                    let metas = index.load_metas()?;
                    let (manifest, publication_metadata, searcher) = if metas.payload.is_some() {
                        let publication = load_publication_for_metas(&root, &metas)?;
                        let manifest = publication.manifest;
                        if pointer.active().generation_id() != manifest.generation_id()? {
                            return Err(IndexError::InvalidActiveGenerationPointer);
                        }
                        let reader = index
                            .reader_builder()
                            .reload_policy(ReloadPolicy::Manual)
                            .try_into()?;
                        let searcher = reader.searcher();
                        if searcher_generation(&searcher) != meta_generation(&metas) {
                            return Err(IndexError::ConcurrentGenerationChange);
                        }
                        verify_searcher_structure(&searcher, &manifest)?;
                        (Some(manifest), publication.metadata, Some(searcher))
                    } else if metas.segments.is_empty() {
                        (None, None, None)
                    } else {
                        return Err(IndexError::UnboundIndexState);
                    };
                    Ok((
                        index,
                        fields,
                        manifest,
                        metas.opstamp,
                        searcher,
                        publication_metadata,
                    ))
                })
                .transpose()
                .or_else(|error| {
                    if generation_incompatibility_requires_rebuild(&error) {
                        Ok(None)
                    } else {
                        Err(error)
                    }
                })?
        } else {
            None
        };

        let (
            index,
            candidate_directory_name,
            fields,
            base_manifest,
            base_opstamp,
            base_searcher,
            base_publication_metadata,
        ) = if let Some((index, fields, manifest, opstamp, searcher, publication_metadata)) =
            reusable_generation
        {
            (
                index,
                None,
                fields,
                manifest,
                opstamp,
                searcher,
                publication_metadata,
            )
        } else {
            // The active slot is absent, physically rejected, or belongs to
            // an incompatible disposable generation. Build an empty current
            // candidate and retain only the pointer as publication authority.
            let candidate = create_candidate_generation(&root, None)?;
            validate_schema(&candidate.index.schema())?;
            let fields = fields_from_schema(&candidate.index.schema())?;
            let metas = candidate.index.load_metas()?;
            (
                candidate.index,
                Some(candidate.directory_name),
                fields,
                None,
                metas.opstamp,
                None,
                None,
            )
        };
        let preparation_base = active_pointer.as_ref().and_then(|pointer| {
            base_searcher
                .as_ref()
                .filter(|_| base_manifest.is_some())
                .map(|searcher| (root.clone(), pointer.active().clone(), searcher.clone()))
        });
        let core_record_preparer = CoreRecordPreparer::new(
            fields,
            active_pointer
                .as_ref()
                .map(|pointer| pointer.active().generation_id().to_owned()),
            preparation_base,
        );
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
            base_opstamp,
            base_searcher,
            base_publication_metadata,
            core_record_preparer,
            complete_inventories: Vec::new(),
            pending: HashMap::new(),
            deletions: HashMap::new(),
            route_deletions: HashSet::new(),
            present_source_routes: None,
            observed_missing_routes: HashMap::new(),
            missing_route_revalidations: Vec::new(),
            source_identities,
            source_route_plan: None,
            active_source_route_stage: None,
            #[cfg(test)]
            index_writer_constructions: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            before_writer_handoff: None,
            #[cfg(test)]
            before_candidate_commit: None,
            #[cfg(test)]
            after_candidate_commit: None,
            #[cfg(test)]
            return_commit_error_after_visibility: false,
            #[cfg(test)]
            before_pointer_switch: None,
            #[cfg(test)]
            before_pointer_publication: None,
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
        if self.source_route_plan.is_some() && self.active_source_route_stage.is_none() {
            return Err(IndexError::InvalidSourceRoutePlan(
                "complete inventory certification requires an active selected route".to_owned(),
            ));
        }
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
        if !self.observed_missing_routes.is_empty() || !self.route_deletions.is_empty() {
            return Ok(None);
        }
        if self
            .present_source_routes
            .as_ref()
            .is_some_and(|routes| routes.as_slice() != base.source_routes())
        {
            // Missing-state reset and route membership changes are manifest
            // mutations even when every Core source is otherwise unchanged.
            return Ok(None);
        }

        // A no-work candidate is a full-inventory claim except for routes
        // explicitly authenticated as exact carry-forward from this locked
        // base. Do not silently carry any other omitted source.
        if let Some(missing) = base.sources.iter().find(|base_source| {
            !self
                .pending
                .contains_key(&source_token(base_source.observation().source()))
                && !self.source_is_carried_from_base(base_source.observation().source())
        }) {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.observation().source().identity().to_string(),
            });
        }
        let retained_sources_are_exact = self.pending.values().all(|pending| {
            base.sources
                .iter()
                .find(|base_source| {
                    base_source
                        .observation()
                        .source()
                        .exact_descriptor_eq(&pending.source)
                })
                .is_some_and(|base_source| {
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
        if self.complete_inventories.is_empty()
            && (!self.pending.is_empty() || self.source_route_plan.is_none())
        {
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
            !self.source_is_carried_from_base(source.observation().source())
                && !covered_sources.contains(&source.observation().source().identity().digest())
        }) {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.observation().source().identity().to_string(),
            });
        }
        Ok(Some(ExactReplayInventoryWitness { base }))
    }

    /// Starts replacing every lexical document owned by `source`.
    ///
    /// Documents can then be submitted as they are parsed; no whole-source or
    /// whole-batch DTO is retained by this writer.
    pub fn begin_source(&mut self, source: SourceKey) -> Result<()> {
        self.reject_carried_source_mutation(&source)?;
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
        self.route_deletions.remove(&source);
        self.pending.insert(
            token,
            PendingSource {
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
        self.reject_carried_source_mutation(&source)?;
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
    pub fn add_core_record(&mut self, record: CoreRecord) -> Result<()> {
        let prepared = self.core_record_preparer().prepare(record)?;
        self.add_prepared_core_record(prepared)
    }

    /// Returns a cloneable immutable preparation context pinned to this
    /// writer's base-generation lookup authority.
    pub fn core_record_preparer(&self) -> CoreRecordPreparer {
        self.core_record_preparer.clone()
    }

    /// Enqueues one canonical record prepared by this writer's exact base
    /// context. Preparation has already completed certificate reuse, encoding,
    /// and lexical projection; this method never mutates or re-encodes it.
    pub fn add_prepared_core_record(&mut self, prepared: PreparedCoreRecord) -> Result<()> {
        let expected_base_generation_id = self
            .active_pointer
            .as_ref()
            .map(|pointer| pointer.active().generation_id());
        if prepared.base_generation_id() != expected_base_generation_id {
            return Err(IndexError::PreparedCoreRecordContextMismatch);
        }
        let token = prepared.source_token().to_owned();
        let pending_source = match self.pending.get(&token) {
            Some(pending) if pending.source.exact_descriptor_eq(prepared.source()) => pending,
            _ => return Err(IndexError::DocumentSourceNotActive),
        };
        let is_append = matches!(&pending_source.mode, PendingSourceMode::Append { .. });
        if matches!(&pending_source.mode, PendingSourceMode::Retain { .. }) {
            return Err(IndexError::DocumentSourceNotActive);
        }
        if is_append && self.base_searcher.is_none() {
            return Err(IndexError::AppendBaseMismatch);
        }
        let preparation::PreparedCoreRecordParts {
            record_accumulator_leaf,
            document,
        } = prepared.into_parts();
        self.writer_mut()?.add_document(document)?;
        let pending = self
            .pending
            .get_mut(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        staging::accumulate_core_record(
            &mut pending.core_record_accumulator,
            &record_accumulator_leaf,
        );
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
        let deletion = PendingDeletion::new(proof, inventory)?;
        let source = deletion.source();
        self.reject_carried_source_mutation(source)?;
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
        self.route_deletions.remove(source);
        self.deletions.insert(source.clone(), deletion);
        Ok(())
    }

    /// Defines every route conclusively present in the candidate snapshot.
    /// Missing routes are added separately by `observe_certified_missing_route`.
    pub fn set_present_source_routes(&mut self, routes: Vec<SourceRouteSnapshot>) -> Result<()> {
        if routes.iter().any(|route| route.missing_state().is_some()) {
            return Err(IndexError::WriterInvariant(
                "present source routes cannot carry missing state",
            ));
        }
        let mut canonical = routes;
        if let Some(plan) = &self.source_route_plan {
            if let Some(route) = canonical.iter().find(|route| {
                !plan.completed.contains(route.route_identity())
                    || plan.carried_from_base.contains(route.route_identity())
            }) {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "present route {} is not a completed selected route",
                    route.route_identity().as_str()
                )));
            }
            if let Some(base) = &self.base_manifest {
                canonical.extend(
                    base.source_routes()
                        .iter()
                        .filter(|route| plan.carried_from_base.contains(route.route_identity()))
                        .cloned(),
                );
            }
        }
        canonical.sort_by(|left, right| left.route_identity().cmp(right.route_identity()));
        if canonical
            .windows(2)
            .any(|pair| pair[0].route_identity() == pair[1].route_identity())
        {
            return Err(IndexError::NonCanonicalSourceRoutes);
        }
        self.present_source_routes = Some(canonical);
        Ok(())
    }

    /// Advances durable grace for one whole route whose absence is
    /// conclusive and can be revalidated immediately before publication.
    pub fn observe_certified_missing_route<F>(
        &mut self,
        route_identity: SourceRouteIdentity,
        observed_at_unix_ms: u64,
        delete_after_consecutive_observations: u32,
        revalidate_missing: F,
    ) -> Result<CertifiedMissingRouteOutcome>
    where
        F: Fn() -> bool + Send + 'static,
    {
        if self.source_route_plan.is_some() {
            self.require_active_source_route(&route_identity)?;
        }
        if delete_after_consecutive_observations < 2 {
            return Err(IndexError::InvalidSourceRouteDeletionGraceThreshold);
        }
        if self.observed_missing_routes.contains_key(&route_identity)
            || self
                .missing_route_revalidations
                .iter()
                .any(|(candidate, _)| candidate == &route_identity)
        {
            return Err(IndexError::DuplicateSourceRouteMissingObservation(
                route_identity.as_str().to_owned(),
            ));
        }
        let Some(base) = self.base_manifest.as_ref() else {
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        };
        let Some(base_route) = base.source_route(&route_identity).cloned() else {
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        };
        if base_route.sources().is_empty() {
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        }
        let base_generation = base.generation_id()?;
        let observation = SourceMissingObservationPoint::new(base_generation, observed_at_unix_ms)?;
        let state = match base_route.missing_state() {
            Some(previous) => previous.advance(observation)?,
            None => SourceRouteMissingState::first(observation),
        };
        let retained_sources = base_route.sources().to_vec();
        self.missing_route_revalidations
            .push((route_identity.clone(), Box::new(revalidate_missing)));
        if state.consecutive_missing().get() >= delete_after_consecutive_observations {
            let source_key_field = self.fields.source_key;
            for source in &retained_sources {
                let token = source_token(source);
                self.writer_mut()?
                    .delete_term(Term::from_field_text(source_key_field, &token));
                self.route_deletions.insert(source.clone());
            }
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources,
                deleted: true,
            });
        }
        let snapshot =
            SourceRouteSnapshot::missing(route_identity.clone(), retained_sources.clone(), state)?;
        self.observed_missing_routes
            .insert(route_identity, snapshot);
        Ok(CertifiedMissingRouteOutcome {
            retained_sources,
            deleted: false,
        })
    }
}

#[cfg(test)]
mod tests;
