//! Atomic source-backed lexical generations.
//!
//! A Tantivy commit names a durable immutable source-revision manifest, so
//! readers observe either the previous complete generation or the next one.

mod analyzer;
mod contracts;
mod durable_directory;
mod identity;
pub mod policy;
mod publication;
mod query;
mod reader;
mod schema;
mod staging;

pub(crate) use contracts::{
    CommitPayload, COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD, MANIFEST_DIRECTORY,
    MAX_DOCUMENT_METADATA_BYTES,
};
pub use contracts::{
    CommitReceipt, GenerationManifest, GenerationRemoval, IndexError, LexicalDocument, Result,
    RevalidationTarget, WriterOptions, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION,
    LEXICAL_SCHEMA_VERSION, LEXICAL_SEGMENT_MERGE_FAN_IN,
};
pub(crate) use identity::{
    hex, register_compact_identity, register_event_identity, register_session_identity, sha256_hex,
    source_sort_key, source_token, validate_event_identity_against_base,
    validate_referenced_session_identity_against_base, validate_session_identity_against_base,
};
#[cfg(test)]
pub(crate) use publication::manifest_path;
pub(crate) use publication::{
    classify_publication_failure, load_manifest_for_metas, meta_generation, payload_generation_id,
    reconcile_commit_error, searcher_generation, sync_directory, verify_searcher, write_manifest,
};
pub(crate) use schema::{
    fields_from_schema, lexical_schema, required_field, validate_schema, Fields,
};
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
pub use reader::VerifiedIndex;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    SourceKey, StableEntityId, StableEntityKind,
};
#[cfg(test)]
use ctx_history_core::{SourceRecordLocator, IDENTITY_VERSION};
use tantivy::{
    directory::{Directory, DirectoryLock, Lock, INDEX_WRITER_LOCK},
    indexer::LogMergePolicy,
    Index, IndexMeta, IndexSettings, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term,
};
use uuid::Uuid;

use durable_directory::{reclaim_abandoned_atomic_writes, DurableMmapDirectory};
use staging::finish_identical_staging;

struct PendingSource {
    source: SourceKey,
    mode: PendingSourceMode,
    staged_documents: u64,
    certificate: Option<CertifiedSource>,
}

// Keep the append base inline to avoid allocation and indirection.
#[allow(clippy::large_enum_variant)]
enum PendingSourceMode {
    Replace,
    Append { base: CertifiedSource },
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
    writer: Option<IndexWriter<TantivyDocument>>,
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
            verify_searcher(&searcher, &manifest)?;
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

    fn writer_mut(&mut self) -> Result<&mut IndexWriter<TantivyDocument>> {
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

            let writer = self.index.writer_with_num_threads::<TantivyDocument>(
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
                        (PendingSourceMode::Append { base }, Some(current))
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
        self.writer_mut()?.add_document(target)?;
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
