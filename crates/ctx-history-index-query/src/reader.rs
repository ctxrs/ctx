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
    acquire_generation_read_lease, acquire_retained_generation_read_lease,
    cache_recertified_physical_integrity, load_active_generation_pointer,
    load_generation_retention_lease, open_slot_index,
    verify_candidate_physical_integrity_read_only, verify_physical_integrity_read_only,
    ActiveGenerationPointer, CertifiedPhysicalIntegrity, GenerationReadLease,
    GenerationRetentionLease, GenerationSlot,
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
    pub(crate) semantic_eligibility_postings: OnceLock<crate::SemanticEligibilityPostings>,
    _reader_leases: Option<ReaderLeaseBundle>,
}

/// Metadata-only authority for one fully verified generation.
///
/// Unlike [`VerifiedIndex`], this snapshot owns no Tantivy searcher or
/// generation read lease. It keeps the exact generation identity and the
/// already decoded immutable manifest available to refresh orchestration.
#[derive(Clone)]
pub struct VerifiedGenerationSnapshot {
    generation_id: String,
    manifest: Arc<GenerationManifest>,
}

impl VerifiedGenerationSnapshot {
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
}

/// Query-reader authority for the selected generation and, only when the
/// caller explicitly requested a stable pair, its one retained peer.
struct ReaderLeaseBundle {
    target: GenerationReadLease,
    peer: Option<GenerationReadLease>,
}

// Keep the target reclamation lease alive until every generation-backed
// resource and peer authority has completed destruction. Explicit `drop`
// calls make this independent of struct field declaration order.
fn drop_generation_backed_resources_before_target_lease<
    TantivyResources,
    SemanticResources,
    PeerLease,
    TargetLease,
>(
    tantivy_resources: TantivyResources,
    semantic_resources: SemanticResources,
    peer_lease: PeerLease,
    target_lease: TargetLease,
) {
    drop(tantivy_resources);
    drop(semantic_resources);
    drop(peer_lease);
    drop(target_lease);
}

struct ReaderGenerationSelection {
    target: GenerationSlot,
    durable_authority: Option<GenerationRetentionLease>,
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
        Self::open_pinned_with_peer(root, false)
    }

    /// Opens the active generation and retains its current pointer peer for
    /// the reader lifetime when one is available.
    ///
    /// Callers that present compact-generation references must use this paired
    /// open instead of acquiring an ordinary reader and asking for a peer
    /// later. Acquiring both locks from one observed pointer preserves a
    /// stable active/previous pair across concurrent publication.
    pub fn open_pinned_with_retained_peer(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_pinned_with_peer(root, true)
    }

    fn open_pinned_with_peer(root: impl AsRef<Path>, retain_peer: bool) -> Result<Self> {
        Self::open_pinned_with_loader(
            root.as_ref(),
            |root| load_active_generation_pointer(root).map_err(IndexError::from),
            retain_peer,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_pinned_with_pointer_loader<F>(
        root: impl AsRef<Path>,
        load_pointer: F,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        Self::open_pinned_with_loader(root.as_ref(), load_pointer, false)
    }

    fn open_pinned_with_loader<F>(
        root: &Path,
        mut load_pointer: F,
        retain_peer: bool,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        Self::open_query_reader_with_loader(
            root,
            &mut load_pointer,
            |pointer, _| {
                Ok(ReaderGenerationSelection {
                    target: pointer.active().clone(),
                    durable_authority: None,
                })
            },
            None,
            retain_peer,
        )
    }

    fn open_query_reader_with_loader<F, S>(
        root: &Path,
        load_pointer: &mut F,
        mut select: S,
        expected_generation_id: Option<&str>,
        retain_peer: bool,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
        S: FnMut(
            &ActiveGenerationPointer,
            Option<&GenerationRetentionLease>,
        ) -> Result<ReaderGenerationSelection>,
    {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();

        let first_pointer =
            load_pointer(&root)?.ok_or(IndexError::MissingActiveGenerationPointer)?;
        let first_result = {
            let durable_authority = load_generation_retention_lease(&root)?;
            select(&first_pointer, durable_authority.as_ref()).and_then(|selection| {
                Self::open_query_reader_attempt(
                    &root,
                    &first_pointer,
                    selection,
                    expected_generation_id,
                    retain_peer,
                )
            })
        };
        let observed_pointer = load_pointer(&root)?;
        if observed_pointer.as_ref() == Some(&first_pointer) {
            return first_result;
        }

        let retry_pointer = observed_pointer.ok_or(IndexError::MissingActiveGenerationPointer)?;
        let retry_result = {
            let durable_authority = load_generation_retention_lease(&root)?;
            select(&retry_pointer, durable_authority.as_ref()).and_then(|selection| {
                Self::open_query_reader_attempt(
                    &root,
                    &retry_pointer,
                    selection,
                    expected_generation_id,
                    retain_peer,
                )
            })
        };
        if load_pointer(&root)?.as_ref() != Some(&retry_pointer) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result
    }

    fn open_query_reader_attempt(
        root: &Path,
        pointer: &ActiveGenerationPointer,
        selection: ReaderGenerationSelection,
        expected_generation_id: Option<&str>,
        retain_peer: bool,
    ) -> Result<Self> {
        let leases = Self::acquire_reader_leases(root, pointer, &selection, retain_peer)?;
        let target = &leases.target;
        let (mut index, recertified) = target
            .with_root_access(|root| {
                Self::open_slot_with_certification(
                    root,
                    pointer,
                    target.target(),
                    ReopenPhysicalVerification::VerifyOrCertify,
                    |actual_generation_id| match expected_generation_id {
                        Some(expected_generation_id) => IndexError::PinnedGenerationMismatch {
                            expected_generation_id: expected_generation_id.to_owned(),
                            actual_generation_id,
                        },
                        None => IndexError::InvalidActiveGenerationPointer,
                    },
                )
            })
            .map_err(IndexError::from)??;
        if let Some(certified) = recertified {
            let _ = cache_recertified_physical_integrity(
                root,
                pointer,
                target.target(),
                index.searcher.index(),
                &certified,
            );
        }
        index._reader_leases = Some(leases);
        Ok(index)
    }

    fn acquire_reader_leases(
        root: &Path,
        pointer: &ActiveGenerationPointer,
        selection: &ReaderGenerationSelection,
        retain_peer: bool,
    ) -> Result<ReaderLeaseBundle> {
        let target = match selection.durable_authority.as_ref() {
            Some(authority) => acquire_retained_generation_read_lease(root, authority)?,
            None => acquire_generation_read_lease(root, selection.target.generation_id())?,
        };
        let peer = retain_peer
            .then(|| {
                if selection.target.generation_id() == pointer.active().generation_id() {
                    pointer.previous()
                } else if pointer
                    .previous()
                    .is_some_and(|slot| slot.generation_id() == selection.target.generation_id())
                {
                    Some(pointer.active())
                } else {
                    None
                }
            })
            .flatten()
            .map(|peer| acquire_generation_read_lease(root, peer.generation_id()))
            .transpose()?;
        Ok(ReaderLeaseBundle { target, peer })
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
        let (generation_id, manifest) = publication.into_parts();
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
            semantic_eligibility_postings: OnceLock::new(),
            _reader_leases: None,
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
        Self::open_pinned_generation_with_peer(root, expected_generation_id, false)
    }

    /// Opens exactly the requested retained generation and its current pointer
    /// peer as one stable reader pair when available.
    pub fn open_pinned_generation_with_retained_peer(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        Self::open_pinned_generation_with_peer(root, expected_generation_id, true)
    }

    fn open_pinned_generation_with_peer(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
        retain_peer: bool,
    ) -> Result<Self> {
        Self::open_pinned_generation_with_loader(
            root.as_ref(),
            expected_generation_id,
            |root| load_active_generation_pointer(root).map_err(IndexError::from),
            retain_peer,
        )
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
        Self::open_generation_read_lease_with_verification(
            root.as_ref(),
            lease,
            ReopenPhysicalVerification::ReadOnly,
        )
        .map(|(index, _)| index)
    }

    fn open_generation_read_lease_with_verification(
        root: &Path,
        lease: &GenerationReadLease,
        physical_verification: ReopenPhysicalVerification,
    ) -> Result<(
        Self,
        Option<(ActiveGenerationPointer, CertifiedPhysicalIntegrity)>,
    )> {
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        if lease.root() != root {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }
        let first_pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        let first_result = Self::open_slot_with_certification(
            &root,
            &first_pointer,
            lease.target(),
            physical_verification,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: lease.generation_id().to_owned(),
                actual_generation_id,
            },
        );
        let observed_pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        if observed_pointer == first_pointer {
            return first_result
                .map(|(index, proof)| (index, proof.map(|proof| (first_pointer, proof))));
        }

        let retry_result = Self::open_slot_with_certification(
            &root,
            &observed_pointer,
            lease.target(),
            physical_verification,
            |actual_generation_id| IndexError::PinnedGenerationMismatch {
                expected_generation_id: lease.generation_id().to_owned(),
                actual_generation_id,
            },
        );
        if load_active_generation_pointer(&root)?.as_ref() != Some(&observed_pointer) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result.map(|(index, proof)| (index, proof.map(|proof| (observed_pointer, proof))))
    }

    /// Moves this reader's already selected retained-peer lease into a peer
    /// reader.
    #[doc(hidden)]
    pub fn take_retained_generation_peer_for_reader(&mut self) -> Result<Option<Self>> {
        let Some(lease) = self
            ._reader_leases
            .as_mut()
            .and_then(|leases| leases.peer.take())
        else {
            return Ok(None);
        };
        let (mut peer, recertified) = lease
            .with_root_access(|root| {
                Self::open_generation_read_lease_with_verification(
                    root,
                    &lease,
                    ReopenPhysicalVerification::VerifyOrCertify,
                )
            })
            .map_err(IndexError::from)??;
        if let Some((pointer, certified)) = recertified {
            let _ = cache_recertified_physical_integrity(
                lease.root(),
                &pointer,
                lease.target(),
                peer.searcher.index(),
                &certified,
            );
        }
        peer._reader_leases = Some(ReaderLeaseBundle {
            target: lease,
            peer: None,
        });
        Ok(Some(peer))
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
            false,
        )
    }

    fn open_pinned_generation_with_loader<F>(
        root: &Path,
        expected_generation_id: &str,
        mut load_pointer: F,
        retain_peer: bool,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        if !is_generation_id(expected_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        Self::open_query_reader_with_loader(
            root,
            &mut load_pointer,
            |pointer, durable_authority| {
                let (target, durable_authority) =
                    if pointer.active().generation_id() == expected_generation_id {
                        (pointer.active().clone(), None)
                    } else if let Some(previous) = pointer
                        .previous()
                        .filter(|slot| slot.generation_id() == expected_generation_id)
                    {
                        (previous.clone(), None)
                    } else if let Some(authority) = durable_authority.filter(|authority| {
                        authority.target().generation_id() == expected_generation_id
                    }) {
                        (authority.target().clone(), Some(authority.clone()))
                    } else {
                        return Err(IndexError::PinnedGenerationNotRetained {
                            expected_generation_id: expected_generation_id.to_owned(),
                            active_generation_id: pointer.active().generation_id().to_owned(),
                            previous_generation_id: pointer
                                .previous()
                                .map(|slot| slot.generation_id().to_owned()),
                        });
                    };
                Ok(ReaderGenerationSelection {
                    target,
                    durable_authority,
                })
            },
            Some(expected_generation_id),
            retain_peer,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    fn open_inner(root: &Path, physical_verification: ReopenPhysicalVerification) -> Result<Self> {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        Self::open_slot_with_certification(
            &root,
            &pointer,
            pointer.active(),
            physical_verification,
            |_| IndexError::InvalidActiveGenerationPointer,
        )
        .map(|(index, _)| index)
    }

    fn open_slot_with_certification<F>(
        root: &Path,
        pointer: &ActiveGenerationPointer,
        slot: &GenerationSlot,
        physical_verification: ReopenPhysicalVerification,
        generation_mismatch: F,
    ) -> Result<(Self, Option<CertifiedPhysicalIntegrity>)>
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
        let (generation_id, manifest) = publication.into_parts();
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
        let recertified = match physical_verification {
            ReopenPhysicalVerification::VerifyOrCertify => Some(
                verify_or_certify_physical_integrity(root, pointer, slot, &index)?,
            ),
            ReopenPhysicalVerification::ReadOnly => {
                verify_physical_integrity_read_only(root, slot, &index)?;
                None
            }
            #[cfg(any(test, feature = "test-support"))]
            ReopenPhysicalVerification::ScrubAndCertify => {
                scrub_and_certify_physical_integrity(root, pointer, slot, &index)?;
                None
            }
        };
        verify_searcher_structure(&searcher, &manifest)?;
        Ok((
            Self {
                searcher,
                manifest,
                generation_id,
                semantic_eligibility_postings: OnceLock::new(),
                _reader_leases: None,
            },
            recertified,
        ))
    }

    #[doc(hidden)]
    pub fn from_verified_publication(publication: VerifiedPublication) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT
            .with(|count| count.set(count.get().saturating_add(1)));
        let (searcher, manifest, generation_id) = publication.into_parts();
        Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligibility_postings: OnceLock::new(),
            _reader_leases: None,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    /// Consumes this verified reader and retains only generation metadata.
    ///
    /// Reader leases and Tantivy handles are dropped before the snapshot is
    /// returned, so a subsequent writer open cannot overlap their FD lifetime.
    pub fn into_generation_snapshot(self) -> VerifiedGenerationSnapshot {
        let Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligibility_postings,
            _reader_leases,
        } = self;
        let (peer_lease, target_lease) = match _reader_leases {
            Some(ReaderLeaseBundle { target, peer }) => (peer, Some(target)),
            None => (None, None),
        };
        drop_generation_backed_resources_before_target_lease(
            searcher,
            semantic_eligibility_postings,
            peer_lease,
            target_lease,
        );
        VerifiedGenerationSnapshot {
            generation_id,
            manifest,
        }
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

#[cfg(test)]
mod drop_order_tests {
    use super::drop_generation_backed_resources_before_target_lease;
    use std::cell::RefCell;

    struct DropTrace<'a> {
        events: &'a RefCell<Vec<&'static str>>,
        event: &'static str,
    }

    impl Drop for DropTrace<'_> {
        fn drop(&mut self) {
            self.events.borrow_mut().push(self.event);
        }
    }

    #[test]
    fn target_lease_drops_after_all_generation_backed_resources() {
        let events = RefCell::new(Vec::new());
        let traced = |event| DropTrace {
            events: &events,
            event,
        };

        drop_generation_backed_resources_before_target_lease(
            traced("tantivy searcher and mappings"),
            traced("semantic postings"),
            Some(traced("peer lease")),
            Some(traced("target lease")),
        );

        assert_eq!(
            *events.borrow(),
            [
                "tantivy searcher and mappings",
                "semantic postings",
                "peer lease",
                "target lease",
            ]
        );
    }
}
