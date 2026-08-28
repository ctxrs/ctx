use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use crate::{IndexError, Result};
use ctx_history_index_format::{
    is_generation_id, load_publication_for_metas, meta_generation, payload_generation_id,
    register_body_analyzer, searcher_generation, validate_schema,
    verify_or_certify_physical_integrity, verify_searcher_structure, DurableMmapDirectory,
    GenerationManifest, VerifiedPublication,
};
#[cfg(any(test, feature = "test-support"))]
use ctx_history_index_format::{scrub_and_certify_physical_integrity, verify_searcher};
use ctx_history_index_generation::{
    load_active_generation_pointer, load_generation_retention_lease, open_slot_index,
    verify_candidate_physical_integrity_read_only, verify_physical_integrity_read_only,
    ActiveGenerationPointer, GenerationReadLease, GenerationRetentionLease, GenerationSlot,
};
use tantivy::{ReloadPolicy, Searcher};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static VERIFIED_INDEX_REOPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A verified reader pinned to one immutable lexical generation.
///
/// Feature-enabled downstream code cannot extract its raw Tantivy searcher or
/// index handle:
///
/// ```compile_fail
/// use ctx_history_index_query::VerifiedIndex;
///
/// fn expose_raw_searcher(index: &VerifiedIndex) {
///     let _ = index.test_searcher();
/// }
/// ```
pub struct VerifiedIndex {
    pub(crate) searcher: Searcher,
    pub(crate) manifest: Arc<GenerationManifest>,
    pub(crate) generation_id: String,
    pub(crate) publication_metadata: Option<Arc<[u8]>>,
    pub(crate) semantic_eligibility_postings: OnceLock<crate::SemanticEligibilityPostings>,
}

#[derive(Clone, Copy)]
enum ReopenPhysicalVerification {
    VerifyOrCertify,
    ReadOnly,
    #[cfg(any(test, feature = "test-support"))]
    ScrubAndCertify,
}

impl VerifiedIndex {
    /// Returns the generation named by the validated active pointer, commit
    /// payload, and current Core manifest contract.
    pub fn active_generation_id(root: impl AsRef<Path>) -> Result<Option<String>> {
        if !root.as_ref().is_dir() {
            return Ok(None);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let Some(pointer) = load_active_generation_pointer(&root)? else {
            return Ok(None);
        };
        let index = open_slot_index(&root, pointer.active())?;
        let metas = index.load_metas()?;
        let generation_id =
            payload_generation_id(&metas)?.ok_or(IndexError::MissingCommitPayload)?;
        if generation_id != pointer.active().generation_id() {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        let publication = load_publication_for_metas(&root, &metas)?;
        if publication.generation_id() != generation_id {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        Ok(Some(publication.generation_id().to_owned()))
    }

    /// Test and qualification oracle that performs the bounded production open
    /// and then audits every stored Core record and logical projection.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let verified = Self::open_pinned(root)?;
        verify_searcher(&verified.searcher, &verified.manifest)?;
        Ok(verified)
    }

    /// Test and qualification oracle that forces a complete physical scrub and
    /// exhaustive logical audit.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn scrub(root: impl AsRef<Path>) -> Result<Self> {
        let verified =
            Self::open_inner(root.as_ref(), ReopenPhysicalVerification::ScrubAndCertify)?;
        verify_searcher(&verified.searcher, &verified.manifest)?;
        Ok(verified)
    }

    /// Opens a previously audited immutable generation for querying.
    ///
    /// The pointer, manifest, generation payload, schema/policy contract,
    /// Tantivy generation pin, certified artifact identities, and total
    /// document count are verified on every open. Artifact bodies are rehashed
    /// only when the durable certification is unavailable or stale. The
    /// publication-time O(document-count) identity audit is not repeated for
    /// current generations.
    pub fn open_pinned(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), ReopenPhysicalVerification::VerifyOrCertify)
    }

    /// Reopens an exact candidate from durable state before its active-pointer
    /// replacement. This is intentionally separate from the writer's in-memory
    /// candidate searcher and accepts an absent authority for initial publish.
    #[doc(hidden)]
    pub fn open_certified_candidate_before_activation(
        root: impl AsRef<Path>,
        predecessor_fence: &ctx_history_index_generation::ActiveGenerationPointerFence,
        slot: &GenerationSlot,
    ) -> Result<Self> {
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        predecessor_fence.validate(&root)?;
        let index = open_slot_index(&root, slot)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let publication = load_publication_for_metas(&root, &metas)?;
        let (generation_id, manifest, publication_metadata) = publication.into_parts();
        if generation_id != slot.generation_id() {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_REOPEN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        verify_candidate_physical_integrity_read_only(&root, predecessor_fence, slot, &index)?;
        verify_searcher_structure(&searcher, &manifest)?;
        predecessor_fence.validate(&root)?;
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            publication_metadata,
            semantic_eligibility_postings: OnceLock::new(),
        })
    }

    /// Opens exactly the requested active or retained previous generation.
    ///
    /// Resolution is bounded to the two immutable slots named by the
    /// publication pointer. If that pointer changes while the generation is
    /// being resolved, the complete resolution is retried once against the
    /// new pointer and then fails closed on any second change.
    ///
    /// Like [`Self::open_pinned`], this performs reopen-time certified physical
    /// identity and structural verification of the selected manifest, payload,
    /// schema/policy contract, Tantivy generation pin, and total document
    /// count. It does not repeat the O(document-count) stored-Core identity and
    /// source audit for current generations.
    pub fn open_pinned_generation(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        Self::open_pinned_generation_with_loader(root.as_ref(), expected_generation_id, |root| {
            load_active_generation_pointer(root).map_err(IndexError::from)
        })
    }

    /// Opens exactly the generation held by a process-scoped read lease using
    /// only an existing publication-time physical certification and bounded
    /// metadata validation. It never hashes the full generation and never
    /// installs, refreshes, or recovers durable index state; unavailable or
    /// stale certification fails closed.
    #[doc(hidden)]
    pub fn open_generation_read_lease(
        root: impl AsRef<Path>,
        lease: &GenerationReadLease,
    ) -> Result<Self> {
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        if lease.root() != root {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }

        let first_pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        let first_result = Self::open_slot(
            &root,
            &first_pointer,
            lease.target(),
            ReopenPhysicalVerification::ReadOnly,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: lease.generation_id().to_owned(),
                actual_generation_id,
            },
        );
        let observed_pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        if observed_pointer == first_pointer {
            return first_result;
        }

        let retry_result = Self::open_slot(
            &root,
            &observed_pointer,
            lease.target(),
            ReopenPhysicalVerification::ReadOnly,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: lease.generation_id().to_owned(),
                actual_generation_id,
            },
        );
        if load_active_generation_pointer(&root)?.as_ref() != Some(&observed_pointer) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result
    }

    /// Opens the one other generation retained beside an already pinned
    /// active or previous generation.
    ///
    /// Compact rendered references use this peer to remain unambiguous across
    /// one publication transition. Resolution is limited to the two slots in
    /// the active pointer, retries once if that pointer changes, and fails
    /// closed if the caller's pinned generation is no longer retained.
    pub fn open_retained_generation_peer(
        root: impl AsRef<Path>,
        pinned_generation_id: &str,
    ) -> Result<Option<Self>> {
        Self::open_retained_generation_peer_with_loader(
            root.as_ref(),
            pinned_generation_id,
            |root| load_active_generation_pointer(root).map_err(IndexError::from),
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_retained_generation_peer_with_pointer_loader<F>(
        root: impl AsRef<Path>,
        pinned_generation_id: &str,
        load_pointer: F,
    ) -> Result<Option<Self>>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        Self::open_retained_generation_peer_with_loader(
            root.as_ref(),
            pinned_generation_id,
            load_pointer,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_pinned_generation_with_pointer_loader<F>(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
        load_pointer: F,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        Self::open_pinned_generation_with_loader(
            root.as_ref(),
            expected_generation_id,
            load_pointer,
        )
    }

    fn open_pinned_generation_with_loader<F>(
        root: &Path,
        expected_generation_id: &str,
        mut load_pointer: F,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        if !is_generation_id(expected_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();

        let first_pointer = load_pointer(&root)?;
        let first_lease = load_generation_retention_lease(&root)?;
        let first_result = Self::open_expected_generation(
            &root,
            first_pointer.as_ref(),
            first_lease.as_ref(),
            expected_generation_id,
        );
        let observed_pointer = load_pointer(&root)?;
        let observed_lease = load_generation_retention_lease(&root)?;
        if observed_pointer == first_pointer && observed_lease == first_lease {
            return first_result;
        }

        let retry_result = Self::open_expected_generation(
            &root,
            observed_pointer.as_ref(),
            observed_lease.as_ref(),
            expected_generation_id,
        );
        if load_pointer(&root)? != observed_pointer
            || load_generation_retention_lease(&root)? != observed_lease
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result
    }

    fn open_retained_generation_peer_with_loader<F>(
        root: &Path,
        pinned_generation_id: &str,
        mut load_pointer: F,
    ) -> Result<Option<Self>>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        if !is_generation_id(pinned_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();

        let first_pointer = load_pointer(&root)?;
        let first_result =
            Self::open_generation_peer(&root, first_pointer.as_ref(), pinned_generation_id);
        let observed_pointer = load_pointer(&root)?;
        if observed_pointer == first_pointer {
            return first_result;
        }

        let retry_result =
            Self::open_generation_peer(&root, observed_pointer.as_ref(), pinned_generation_id);
        if load_pointer(&root)? != observed_pointer {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result
    }

    fn open_generation_peer(
        root: &Path,
        pointer: Option<&ActiveGenerationPointer>,
        pinned_generation_id: &str,
    ) -> Result<Option<Self>> {
        let pointer = pointer.ok_or(IndexError::MissingActiveGenerationPointer)?;
        let peer = if pointer.active().generation_id() == pinned_generation_id {
            pointer.previous()
        } else if pointer
            .previous()
            .is_some_and(|slot| slot.generation_id() == pinned_generation_id)
        {
            Some(pointer.active())
        } else {
            return Err(IndexError::PinnedGenerationNotRetained {
                expected_generation_id: pinned_generation_id.to_owned(),
                active_generation_id: pointer.active().generation_id().to_owned(),
                previous_generation_id: pointer
                    .previous()
                    .map(|slot| slot.generation_id().to_owned()),
            });
        };
        let Some(peer) = peer else {
            return Ok(None);
        };
        let expected_peer_generation_id = peer.generation_id().to_owned();
        Self::open_slot(
            root,
            pointer,
            peer,
            ReopenPhysicalVerification::VerifyOrCertify,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: expected_peer_generation_id,
                actual_generation_id,
            },
        )
        .map(Some)
    }

    fn open_expected_generation(
        root: &Path,
        pointer: Option<&ActiveGenerationPointer>,
        lease: Option<&GenerationRetentionLease>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        let pointer = pointer.ok_or(IndexError::MissingActiveGenerationPointer)?;
        let slot = if pointer.active().generation_id() == expected_generation_id {
            pointer.active()
        } else if let Some(previous) = pointer
            .previous()
            .filter(|slot| slot.generation_id() == expected_generation_id)
        {
            previous
        } else if let Some(leased) = lease
            .map(GenerationRetentionLease::target)
            .filter(|slot| slot.generation_id() == expected_generation_id)
        {
            leased
        } else {
            return Err(IndexError::PinnedGenerationNotRetained {
                expected_generation_id: expected_generation_id.to_owned(),
                active_generation_id: pointer.active().generation_id().to_owned(),
                previous_generation_id: pointer
                    .previous()
                    .map(|slot| slot.generation_id().to_owned()),
            });
        };
        Self::open_slot(
            root,
            pointer,
            slot,
            ReopenPhysicalVerification::VerifyOrCertify,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: expected_generation_id.to_owned(),
                actual_generation_id,
            },
        )
    }

    fn open_inner(root: &Path, physical_verification: ReopenPhysicalVerification) -> Result<Self> {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        Self::open_slot(
            &root,
            &pointer,
            pointer.active(),
            physical_verification,
            |_| IndexError::InvalidActiveGenerationPointer,
        )
    }

    fn open_slot<F>(
        root: &Path,
        pointer: &ActiveGenerationPointer,
        slot: &GenerationSlot,
        physical_verification: ReopenPhysicalVerification,
        generation_mismatch: F,
    ) -> Result<Self>
    where
        F: FnOnce(String) -> IndexError,
    {
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_REOPEN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        let index = open_slot_index(root, slot)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let publication = load_publication_for_metas(root, &metas)?;
        let (generation_id, manifest, publication_metadata) = publication.into_parts();
        if slot.generation_id() != generation_id {
            return Err(generation_mismatch(generation_id));
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        match physical_verification {
            ReopenPhysicalVerification::VerifyOrCertify => {
                verify_or_certify_physical_integrity(root, pointer, slot, &index)?;
            }
            ReopenPhysicalVerification::ReadOnly => {
                verify_physical_integrity_read_only(root, slot, &index)?;
            }
            #[cfg(any(test, feature = "test-support"))]
            ReopenPhysicalVerification::ScrubAndCertify => {
                scrub_and_certify_physical_integrity(root, pointer, slot, &index)?;
            }
        }
        verify_searcher_structure(&searcher, &manifest)?;
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            publication_metadata,
            semantic_eligibility_postings: OnceLock::new(),
        })
    }

    #[doc(hidden)]
    pub fn from_verified_publication(publication: VerifiedPublication) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT
            .with(|count| count.set(count.get().saturating_add(1)));
        let (searcher, manifest, generation_id, publication_metadata) = publication.into_parts();
        Self {
            searcher,
            manifest,
            generation_id,
            publication_metadata,
            semantic_eligibility_postings: OnceLock::new(),
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_shared_manifest(&self) -> &Arc<GenerationManifest> {
        &self.manifest
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_with_searcher(mut self, searcher: Searcher) -> Self {
        self.searcher = searcher;
        self.semantic_eligibility_postings = OnceLock::new();
        self
    }

    /// Returns refresh-owned opaque bytes bound to this exact generation's
    /// canonical Tantivy commit payload.
    pub fn publication_metadata(&self) -> Option<&[u8]> {
        self.publication_metadata.as_deref()
    }

    pub fn document_count(&self) -> u64 {
        self.searcher.num_docs()
    }

    pub fn validate_checksums(&self) -> Result<HashSet<PathBuf>> {
        Ok(self.searcher.index().validate_checksum()?)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn count_term(&self, term_text: &str) -> Result<usize> {
        use tantivy::{collector::Count, query::TermQuery, schema::IndexRecordOption, Term};

        let body = ctx_history_index_format::required_field(self.searcher.schema(), "body_search")?;
        let query = TermQuery::new(
            Term::from_field_text(body, term_text),
            IndexRecordOption::Basic,
        );
        Ok(self.searcher.search(&query, &Count)?)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_verified_index_reopen_count() {
    VERIFIED_INDEX_REOPEN_COUNT.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn verified_index_reopen_count() -> usize {
    VERIFIED_INDEX_REOPEN_COUNT.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_verified_index_publication_construction_count() {
    VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn verified_index_publication_construction_count() -> usize {
    VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT.with(std::cell::Cell::get)
}
