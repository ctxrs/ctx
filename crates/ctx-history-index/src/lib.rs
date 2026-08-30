//! Atomic self-contained lexical Core generations.
//!
//! A Tantivy commit names a durable immutable source-revision manifest, so
//! readers observe either the previous complete generation or the next one.

mod commit_contract;
mod identity;
mod merge_policy;
mod preparation;
mod publication;
mod staging;
mod writer_deletion;
mod writer_options;
mod writer_publication;
mod writer_routes;
mod writer_support;

pub use ctx_history_index_generation::durable_atomic_replace_file;

/// Repairs legacy generation control permissions before a mutating refresh
/// captures immutable file identities.
pub fn ensure_generation_control_state_private(root: &std::path::Path) -> Result<()> {
    let canonical_root =
        ctx_history_index_generation::ensure_generation_control_state_private(root)?;
    ctx_history_index_format::clear_manifest_cache_for_root(&canonical_root)?;
    Ok(())
}

pub use publication::{
    acquire_generation_retention_lease, load_generation_retention_lease,
    release_generation_retention_lease, GenerationRetentionLease,
};

pub use commit_contract::{
    CommitReceipt, PublicationDisposition, PublicationMetadataContext, PublicationStage,
    PublishedGeneration, RevalidationTarget,
};

pub use ctx_history_core::CoreRecord;
pub use ctx_history_index_format::policy;
pub use ctx_history_index_format::project_body_search;
#[cfg(test)]
pub(crate) use ctx_history_index_format::required_field;
pub(crate) use ctx_history_index_format::{
    accumulate_core_record, core_record_accumulator_leaf, core_record_leaf, implicit_source_routes,
    source_sort_key, INDEX_MEMORY_MIN_PER_THREAD,
};
#[cfg(test)]
pub(crate) use ctx_history_index_format::{
    current_core_record_contract_fingerprint, CommitPayload, COMMIT_PAYLOAD_VERSION,
};
pub use ctx_history_index_format::{
    current_semantic_generation_policy, current_semantic_generation_policy_hash,
    current_source_generation_policy, current_source_generation_policy_hash,
    EmbeddingGenerationPolicy, LexicalBodySelection, LexicalGenerationPolicy,
    LexicalIndexedBodyLimit, SemanticCoreContentFilter, SemanticGenerationPolicy, SourceEventClass,
    SourceEventRole, SourceGenerationPolicy, StoredSourceContent, LEXICAL_INDEXED_BODY_LIMIT,
    LEXICAL_SCHEMA_REVISION, LEXICAL_TOKENIZER_REVISION, SEMANTIC_CHUNK_OVERLAP_CHARS,
    SEMANTIC_CHUNK_TARGET_CHARS, SEMANTIC_SOURCE_MAX_CHARS,
};
pub(crate) use ctx_history_index_format::{
    fields_from_schema, lexical_schema, provider_source_config_digest, validate_schema, Fields,
};
pub use ctx_history_index_format::{
    source_token, AppliedProviderRoot, AppliedProviderRootSourceMembership,
    CommittedPredecessorMigrationRecovery, ConsecutiveSourceMissingCount,
    DetachedReleasedProviderRootAuthority, GenerationManifest, IndexError, ProviderRootDefinition,
    ProviderRootSourceIdentity, Result, SourceCoreRecordAggregate, SourceMissingObservationPoint,
    SourceRouteIdentity, SourceRouteMissingState, SourceRouteSnapshot, GENERATION_MANIFEST_VERSION,
    LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION, LEXICAL_SEGMENT_MERGE_FAN_IN,
    MAX_DETACHED_RELEASED_PROVIDER_ROOTS, MAX_PUBLICATION_METADATA_BYTES,
};
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support {
    pub use ctx_history_index_generation::{
        AtomicPublicationStage, AtomicReplacementFailureProbe, PublicationIoProbe,
        PublicationIoProbeGuard,
    };

    use super::IndexError;

    pub fn publication_io_error(error: &IndexError) -> Option<&std::io::Error> {
        match error {
            IndexError::Io(error) => Some(error),
            IndexError::Tantivy(tantivy::TantivyError::IoError(error)) => Some(error),
            _ => None,
        }
    }
}
#[cfg(test)]
pub(crate) use ctx_history_index_generation::sha256_hex;
pub(crate) use ctx_history_index_generation::{hex, is_generation_id, MANIFEST_DIRECTORY};
pub use ctx_history_index_query::VerifiedIndex;
pub use ctx_history_index_query::{
    AgentScope, CompiledSearchFilter, CopiedEventLineage, CopiedEventLineageOccurrence,
    CopiedEventLineagePolicy, CopiedEventLineageRelationshipCount, CopiedEventLineageResolution,
    CoreEventBatch, CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeDirection,
    CoreEventRangeDomain, CoreEventRangeError, CoreEventRangeFilters, CoreEventRangePage,
    CoreEventRangeScope, CoreEventRangeSelection, CoreEventRecord, CoreSemanticEventPage,
    CoreSessionEventPage, CoreSourceEventPage, CoreSourceEventPagePlan, EventRecord,
    EventSearchCandidate, EventSearchFilters, ExcludedSessionTree, LexicalExecution, LexicalMode,
    LexicalQueryLimits, RankedEventRef, SearchContentScope, SemanticEligibility,
    SemanticEventCursor, SemanticEventPage, SemanticFilterProjection, SessionEventCoordinate,
    SessionEventCursor, SessionRecord, SourceEventCursor, SourceEventPage, StoredCoreEventRecord,
    StoredCoreRecordJson, StoredCoreSourceEventPage, DEFAULT_CORE_EVENT_PAGE_BUDGET,
    LEXICAL_QUERY_LIMITS, MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS,
    MAX_COPIED_EVENT_LINEAGE_OCCURRENCES, MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS,
    MAX_CORE_EVENT_RANGE_PAGE_ITEMS, MAX_LEXICAL_QUERY_RESULTS, MAX_SEMANTIC_EVENT_PAGE_ITEMS,
    MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
    MAX_SESSION_EVENT_PAGE_ITEMS, MAX_SOURCE_EVENT_PAGE_ITEMS, SHOW_COPIED_EVENT_LINEAGE_POLICY,
};
pub(crate) use identity::{
    prior_session_identity_facts, register_compact_identity, BaseWitnessSource,
};
#[cfg(test)]
pub(crate) use identity::{
    prior_session_identity_lookup_work, reset_prior_session_identity_lookup_work,
    PriorSessionIdentityLookupWork, MAX_SESSION_WITNESS_SEGMENT_PROBES, MAX_SESSION_WITNESS_VISITS,
};
pub use preparation::{
    CoreRecordPreparer, PreparedCoreRecord, PreparedCoreRecordDraft,
    PreparedCoreRecordMaterialization,
};
#[cfg(test)]
pub(crate) use publication::publish_active_generation_pointer;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use publication::republish_current_for_qualification;
#[cfg(test)]
pub(crate) use publication::verify_searcher;
pub(crate) use publication::{
    best_effort_post_republish_cleanup, canonical_commit_payload,
    certify_candidate_physical_integrity, create_candidate_generation,
    load_active_generation_pointer, meta_generation, open_slot_index, payload_generation_id,
    prepare_successor_manifest, prime_candidate_physical_proof,
    publish_active_generation_pointer_validated, reclaim_inactive_generation_directories,
    reclaim_unreferenced_certifications, reclaim_unreferenced_manifests, reconcile_commit_error,
    republish_current_with_publication_metadata, searcher_generation, sync_directory,
    sync_generation, validate_candidate_managed_files, write_prepared_manifest,
    ActiveGenerationPointer, CandidateActivationFence, CandidatePhysicalProof,
    CurrentRepublishOutcome, GenerationSlot, PointerPublicationOutcome,
    GENERATION_WRITER_LOCK_FILE, INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use publication::{
    load_publication_for_metas, manifest_path, physical_integrity_digest, write_manifest,
};
pub use writer_options::WriterOptions;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use writer_publication::{
    manifest_materialization_visits, reset_manifest_materialization_visits,
};
pub use writer_support::BaseEventIdentityLookup;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
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
use tantivy::ReloadPolicy;
#[cfg(test)]
use tantivy::TantivyDocument;
use tantivy::{
    collector::Count,
    directory::{error::LockError, Directory, DirectoryLock, Lock},
    query::TermQuery,
    schema::{Field, IndexRecordOption},
    Index, IndexWriter, Searcher, Term,
};
use uuid::Uuid;

#[cfg(test)]
use ctx_history_index_format::core_content_bytes;
use ctx_history_index_format::{
    load_active_publication_authority, open_pinned_publication, ActivePublicationAuthority,
    IndexDocument, OpenedPinnedPublication, PinnedPublication,
};
use ctx_history_index_generation::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};
use merge_policy::LexicalMergePolicy;
use preparation::PreparedSessionIdentityFacts;
use staging::{finish_identical_staging, PendingSource as StagedPendingSource, PendingSourceMode};
use writer_options::CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;
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

/// Opaque authority for one certificate borrowed from this writer's exact
/// pinned base generation.
///
/// Carrying this value through a provider scan lets append staging consume the
/// already-resolved certificate without searching the complete base manifest
/// again. The generation binding prevents a certificate resolved by another
/// writer (or an older base) from authorizing an append.
#[derive(Debug, Clone)]
pub struct GenerationBaseCertifiedSource {
    generation_id: String,
    route_identity: SourceRouteIdentity,
    certificate: CertifiedSource,
}

impl GenerationBaseCertifiedSource {
    pub fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }
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

#[derive(Debug, Clone, Default)]
struct PartialSourceRouteDelta {
    upserts: BTreeMap<[u8; 32], SourceKey>,
    deletions: BTreeSet<[u8; 32]>,
}

struct SourceRouteStageCheckpoint {
    route_identity: SourceRouteIdentity,
    source_route_plan: SourceRoutePlan,
    complete_inventories: Vec<CertifiedSourceInventory>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, PendingDeletion>,
    route_deletions: HashSet<SourceKey>,
    observed_missing_routes: HashMap<SourceRouteIdentity, SourceRouteSnapshot>,
    route_publication_revalidation_len: usize,
    partially_reconciled_routes: BTreeSet<SourceRouteIdentity>,
    partial_source_route_deltas: BTreeMap<SourceRouteIdentity, PartialSourceRouteDelta>,
    source_identities: HashMap<Uuid, [u8; 32]>,
    changed_session_insertions: Vec<Uuid>,
    changed_session_updates: Vec<(Uuid, PreparedSessionIdentityFacts)>,
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
/// Core fingerprint mismatches are deliberately excluded: current-schema
/// generations with an unknown or retired fingerprint fail closed, while an
/// obsolete schema is detected first and rebuilt without interpreting rows.
pub fn generation_incompatibility_requires_rebuild(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::UnsupportedActiveGenerationPointer(_)
            | IndexError::UnsupportedCommitPayload(_)
            | IndexError::UnsupportedManifest(_)
            | IndexError::GenerationContractMismatch { .. }
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::GenerationPolicyMismatch { .. }
            | IndexError::SchemaMismatch(_)
            | IndexError::IndexSettingsMismatch(_)
            | IndexError::ChecksumMismatch
    )
}

/// Returns whether a generation error identifies an obsolete or incompatible
/// source projection that recovery may replace from source authority. Physical
/// integrity failures are intentionally excluded even though the writer-side
/// classifier may mark them for a fresh candidate after its own checks.
pub fn generation_incompatibility_requires_recovery_rebuild(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::UnsupportedActiveGenerationPointer(_)
            | IndexError::UnsupportedCommitPayload(_)
            | IndexError::UnsupportedManifest(_)
            | IndexError::GenerationContractMismatch { .. }
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::GenerationPolicyMismatch { .. }
            | IndexError::SchemaMismatch(_)
            | IndexError::IndexSettingsMismatch(_)
    )
}

fn classify_active_integrity_failure(
    root: &Path,
    active: &GenerationSlot,
    error: IndexError,
) -> IndexError {
    let detail = match writer_support::mark_active_generation_for_rebuild(root, active) {
        Ok(()) => error.to_string(),
        Err(marker_error) => {
            format!("{error}; persisting the rebuild decision also failed: {marker_error}")
        }
    };
    IndexError::ActiveGenerationNeedsRebuild {
        generation_id: active.generation_id().to_owned(),
        detail,
    }
}

fn prior_session_identity_lookup_failure_is_passthrough(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::CompactIdentityCollision { .. }
            | IndexError::SessionAuthorityWorkLimitExceeded { .. }
    )
}

#[cfg(test)]
type GenerationPathHook = Box<dyn FnOnce(&Path) + Send>;

/// Sole authority for constructing and publishing a lexical generation.
///
/// Feature-enabled downstream code cannot obtain the raw Tantivy writer or
/// submit an `IndexDocument` without its unsplittable preparation facts:
///
/// ```compile_fail
/// use ctx_history_index::GenerationWriter;
///
/// fn expose_raw_writer(writer: &mut GenerationWriter) {
///     let _ = writer.test_writer_mut();
/// }
/// ```
///
/// ```compile_fail
/// use ctx_history_index::GenerationWriter;
/// use ctx_history_index_format::IndexDocument;
///
/// fn expose_internal_writer(writer: &mut GenerationWriter) {
///     let _: &mut tantivy::IndexWriter<IndexDocument> = writer.writer_mut().unwrap();
/// }
/// ```
///
/// ```compile_fail
/// use ctx_history_index::GenerationWriter;
/// use ctx_history_index_format::IndexDocument;
///
/// fn submit_raw_document(writer: &mut GenerationWriter, document: IndexDocument) {
///     writer.add_prepared_core_record(document).unwrap();
/// }
/// ```
pub struct GenerationWriter {
    root: PathBuf,
    index: Index,
    active_pointer: Option<ActiveGenerationPointer>,
    active_pointer_fence: ctx_history_index_generation::ActiveGenerationPointerFence,
    candidate_directory_name: Option<String>,
    candidate_physical_proof: Option<CandidatePhysicalProof>,
    candidate_activation_fence: Option<CandidateActivationFence>,
    preflight_lock: Option<DirectoryLock>,
    writer: Option<IndexWriter<IndexDocument>>,
    writer_options: WriterOptions,
    fields: Fields,
    base_publication: Option<PinnedPublication>,
    base_opstamp: u64,
    core_record_preparer: CoreRecordPreparer,
    complete_inventories: Vec<CertifiedSourceInventory>,
    pending: HashMap<String, PendingSource>,
    deletions: HashMap<SourceKey, PendingDeletion>,
    route_deletions: HashSet<SourceKey>,
    present_source_routes: Option<Vec<SourceRouteSnapshot>>,
    applied_provider_roots: Option<(bool, String, Vec<AppliedProviderRoot>)>,
    authorized_topology_route_retirements: Option<BTreeSet<SourceRouteIdentity>>,
    observed_missing_routes: HashMap<SourceRouteIdentity, SourceRouteSnapshot>,
    route_publication_revalidations:
        Vec<(SourceRouteIdentity, Box<dyn Fn() -> bool + Send + 'static>)>,
    partially_reconciled_routes: BTreeSet<SourceRouteIdentity>,
    partial_source_route_deltas: BTreeMap<SourceRouteIdentity, PartialSourceRouteDelta>,
    source_identities: HashMap<Uuid, [u8; 32]>,
    changed_sessions: HashMap<Uuid, PreparedSessionIdentityFacts>,
    changed_session_registry_memory_bytes: usize,
    source_route_plan: Option<SourceRoutePlan>,
    active_source_route_stage: Option<SourceRouteStageCheckpoint>,
    active_source_route_cohort_stage: Option<SourceRouteStageCheckpoint>,
    reusable_base_rebuild_detail: Option<String>,
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

/// Compatibility result for opening a generation writer.
///
/// New opens return `Ready`; the committed migration variants remain available
/// so downstream callers do not need a protocol-level transition for this
/// internal cleanup.
pub enum GenerationWriterOpenOutcome {
    Ready(GenerationWriter),
    RecoveredCommittedMigration {
        writer: GenerationWriter,
        recovery: CommittedPredecessorMigrationRecovery,
    },
    CommittedMigrationRecoveryRequired {
        recovery: CommittedPredecessorMigrationRecovery,
    },
}

impl GenerationWriterOpenOutcome {
    pub fn committed_migration_recovery(&self) -> Option<&CommittedPredecessorMigrationRecovery> {
        match self {
            Self::Ready(_) => None,
            Self::RecoveredCommittedMigration { recovery, .. }
            | Self::CommittedMigrationRecoveryRequired { recovery } => Some(recovery),
        }
    }

    pub fn into_writer(
        self,
    ) -> std::result::Result<GenerationWriter, CommittedPredecessorMigrationRecovery> {
        match self {
            Self::Ready(writer) | Self::RecoveredCommittedMigration { writer, .. } => Ok(writer),
            Self::CommittedMigrationRecoveryRequired { recovery } => Err(recovery),
        }
    }
}

mod writer_open;

impl GenerationWriter {
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
        let base = self
            .base_manifest()
            .and_then(|manifest| {
                manifest
                    .sources
                    .iter()
                    .find(|candidate| candidate.observation().source() == &source)
            })
            .cloned()
            .ok_or_else(|| IndexError::SourceNotAppendable(source.identity().to_string()))?;
        self.begin_source_append_with_base(source, base)
    }

    /// Resolves one exact source from the immutable base and binds it to this
    /// writer's pinned generation for later constant-time append admission.
    pub fn generation_base_certified_source(
        &self,
        route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<GenerationBaseCertifiedSource> {
        let generation_id = self.active_pointer.as_ref()?.active().generation_id();
        let source_key = source.identity().digest();
        let manifest = self.base_manifest()?;
        let route = manifest.source_route(route_identity)?;
        let member = route
            .sources()
            .binary_search_by_key(&source_key, |candidate| candidate.identity().digest())
            .ok()
            .and_then(|index| route.sources().get(index))
            .filter(|candidate| candidate.exact_descriptor_eq(source))?;
        let certificate = manifest
            .sources
            .binary_search_by_key(&source_key, |candidate| {
                candidate.observation().source().identity().digest()
            })
            .ok()
            .and_then(|index| manifest.sources.get(index))
            .filter(|candidate| candidate.observation().source().exact_descriptor_eq(member))?
            .clone();
        Some(GenerationBaseCertifiedSource {
            generation_id: generation_id.to_owned(),
            route_identity: route_identity.clone(),
            certificate,
        })
    }

    /// Starts an exact append using a certificate previously resolved from
    /// this writer's pinned base generation.
    pub fn begin_source_append_from_base(
        &mut self,
        base: GenerationBaseCertifiedSource,
    ) -> Result<&CertifiedSource> {
        let expected_generation_id = self
            .active_pointer
            .as_ref()
            .map(|pointer| pointer.active().generation_id());
        if expected_generation_id != Some(base.generation_id.as_str()) {
            return Err(IndexError::AppendBaseMismatch);
        }
        self.require_active_source_route(&base.route_identity)?;
        let source = base.certificate.observation().source().clone();
        self.begin_source_append_with_base(source, base.certificate)
    }

    fn begin_source_append_with_base(
        &mut self,
        source: SourceKey,
        base: CertifiedSource,
    ) -> Result<&CertifiedSource> {
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
        if base.frontier().is_none() || !base.observation().source().exact_descriptor_eq(&source) {
            return Err(IndexError::SourceNotAppendable(
                source.identity().to_string(),
            ));
        }
        self.deletions.remove(&source);
        self.route_deletions.remove(&source);
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
        if matches!(&pending_source.mode, PendingSourceMode::Retain { .. }) {
            return Err(IndexError::DocumentSourceNotActive);
        }
        if matches!(&pending_source.mode, PendingSourceMode::Append { .. })
            && self.base_publication.is_none()
        {
            return Err(IndexError::AppendBaseMismatch);
        }
        let preparation::PreparedCoreRecordParts {
            record_accumulator_leaf,
            identity_facts,
            document,
        } = prepared.into_parts();
        let candidate = identity_facts.session;
        let source_owner = pending_source.source.identity().digest();
        if candidate.source_owner != source_owner
            || identity_facts.event_id.source_digest() != source_owner
            || candidate.session_id.source_digest() != source_owner
        {
            return Err(IndexError::WriterInvariant(
                "prepared Core identity facts do not match their staged source",
            ));
        }
        let session_uuid = candidate.session_id.as_uuid();
        let (merged, first_for_candidate, prior) =
            match self.changed_sessions.get(&session_uuid).copied() {
                Some(existing) => (
                    merge_session_identity_facts(existing, candidate)?,
                    false,
                    None,
                ),
                None => {
                    self.ensure_changed_session_registry_capacity()?;
                    let prior = if let Some(base) = self.base_publication.as_ref() {
                        let lookup = prior_session_identity_facts(
                            base.searcher(),
                            self.fields,
                            candidate.session_id,
                            |owner, core| self.base_witness_source_state(owner, core),
                        );
                        match lookup {
                            Ok(prior) => prior,
                            Err(error)
                                if prior_session_identity_lookup_failure_is_passthrough(&error) =>
                            {
                                return Err(error);
                            }
                            Err(error) => match self.active_pointer.as_ref() {
                                Some(pointer) => {
                                    let classified = classify_active_integrity_failure(
                                        &self.root,
                                        pointer.active(),
                                        error,
                                    );
                                    self.reusable_base_rebuild_detail =
                                        Some(classified.to_string());
                                    return Err(classified);
                                }
                                None => return Err(error),
                            },
                        }
                    } else {
                        None
                    };
                    // Candidate-vs-base contradictions are intentionally outside
                    // the active-integrity classification boundary above.
                    let merged = match prior {
                        Some(existing) => merge_session_identity_facts(existing, candidate)?,
                        None => candidate,
                    };
                    (merged, true, prior)
                }
            };
        let advances = self.changed_sessions.get(&session_uuid).copied() != Some(merged);
        let mut document = document;
        let is_replacement = matches!(&pending_source.mode, PendingSourceMode::Replace);
        if (first_for_candidate && (is_replacement || prior.is_none() || prior != Some(merged)))
            || (!first_for_candidate && advances)
        {
            document.add_session_authority(self.fields);
        }
        self.writer_mut()?.add_document(document)?;
        if first_for_candidate {
            self.changed_sessions.insert(session_uuid, merged);
            if let Some(checkpoint) = self.active_source_route_stage.as_mut() {
                checkpoint.changed_session_insertions.push(session_uuid);
            }
        } else if advances {
            let previous = self.changed_sessions.insert(session_uuid, merged).ok_or(
                IndexError::WriterInvariant("changed-session merge lost its prior registry entry"),
            )?;
            if let Some(checkpoint) = self.active_source_route_stage.as_mut() {
                checkpoint
                    .changed_session_updates
                    .push((session_uuid, previous));
            }
        }
        let pending = self
            .pending
            .get_mut(&token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        accumulate_core_record(
            &mut pending.core_record_accumulator,
            &record_accumulator_leaf,
        );
        pending.staged_documents = pending
            .staged_documents
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn ensure_changed_session_registry_capacity(&self) -> Result<()> {
        let maximum_entries = self.changed_session_registry_memory_bytes
            / CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;
        let attempted_entries = self.changed_sessions.len().saturating_add(1);
        if attempted_entries > maximum_entries {
            return Err(IndexError::ChangedSessionRegistryMemoryLimitExceeded {
                attempted_entries,
                required_bytes: attempted_entries
                    .saturating_mul(CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES),
                maximum_bytes: self.changed_session_registry_memory_bytes,
                maximum_entries,
            });
        }
        Ok(())
    }

    /// Resolves base witness source lifecycle through the candidate overlay.
    fn base_witness_source_state(
        &self,
        owner: ctx_history_core::StableEntityId,
        core: &CoreRecord,
    ) -> Result<BaseWitnessSource> {
        let owner_canonical = owner.encode_canonical()?;
        let owner_digest = owner.digest();
        let owner_token = hex(&owner_digest);
        if let Some(pending) = self.pending.get(&owner_token) {
            let candidate = pending.source.identity();
            if candidate.encode_canonical()? != owner_canonical {
                return Err(IndexError::CompactIdentityCollision {
                    kind: "source",
                    uuid: owner.as_uuid(),
                    existing_digest: hex(&candidate.digest()),
                    new_digest: hex(&owner_digest),
                });
            }
            return Ok(match pending.mode {
                PendingSourceMode::Replace => BaseWitnessSource::Replaced,
                PendingSourceMode::Append { .. } | PendingSourceMode::Retain { .. } => {
                    BaseWitnessSource::Active
                }
            });
        }
        // The certified base guarantees that every live Core document belongs
        // to a manifest source. Tombstoned witnesses never reach this callback,
        // so candidate-local deletion state is the only remaining distinction.
        let source = &core.source;
        if self.deletions.contains_key(source) || self.route_deletions.contains(source) {
            Ok(BaseWitnessSource::Deleted)
        } else {
            Ok(BaseWitnessSource::Active)
        }
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
}

pub(crate) fn merge_session_identity_facts(
    existing: PreparedSessionIdentityFacts,
    candidate: PreparedSessionIdentityFacts,
) -> Result<PreparedSessionIdentityFacts> {
    let uuid = candidate.session_id.as_uuid();
    if existing.session_id.digest() != candidate.session_id.digest() {
        return Err(IndexError::CompactIdentityCollision {
            kind: "session",
            uuid,
            existing_digest: hex(&existing.session_id.digest()),
            new_digest: hex(&candidate.session_id.digest()),
        });
    }
    if existing.session_id.encode_canonical()? != candidate.session_id.encode_canonical()?
        || existing.source_owner != candidate.source_owner
    {
        return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
    }
    Ok(PreparedSessionIdentityFacts {
        relationship: preparation::PreparedSessionRelationship {
            parent_session_id: merge_optional_full_identity(
                existing.relationship.parent_session_id,
                candidate.relationship.parent_session_id,
            )?,
            root_session_id: merge_optional_full_identity(
                existing.relationship.root_session_id,
                candidate.relationship.root_session_id,
            )?,
            kind: merge_optional_claim(existing.relationship.kind, candidate.relationship.kind)?,
        },
        ..existing
    })
}

fn merge_optional_full_identity(
    left: Option<ctx_history_core::StableEntityId>,
    right: Option<ctx_history_core::StableEntityId>,
) -> Result<Option<ctx_history_core::StableEntityId>> {
    match (left, right) {
        (Some(left), Some(right)) if left.encode_canonical()? == right.encode_canonical()? => {
            Ok(Some(left))
        }
        (Some(_), Some(_)) => Err(IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields",
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn merge_optional_claim<T: Copy + Eq>(left: Option<T>, right: Option<T>) -> Result<Option<T>> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Ok(Some(left)),
        (Some(_), Some(_)) => Err(IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields",
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests;
