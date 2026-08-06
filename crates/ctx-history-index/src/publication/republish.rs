use std::{path::Path, sync::Arc};

use tantivy::{indexer::NoMergePolicy, Index, ReloadPolicy};

use crate::{
    durable_directory::DurableMmapDirectory, fields_from_schema, validate_schema,
    writer_support::construct_index_writer_with_retry, GenerationManifest, IndexError, Result,
    WriterOptions,
};

use super::{
    canonical_commit_payload, certify_activated_generation, lexical_index_settings,
    load_active_generation_pointer, load_publication_for_metas, meta_generation, open_slot_index,
    payload_generation_id, physical_integrity_audit, publish_active_generation_pointer,
    reclaim_inactive_generation_directories, reclaim_unreferenced_certifications,
    reclaim_unreferenced_manifests, reconcile_commit_error, searcher_generation, slot_path,
    sync_generation, verify_complete_searcher, verify_searcher, write_manifest,
    ActiveGenerationPointer, GenerationSlot, PhysicalIntegrityAudit, PointerPublicationOutcome,
    INDEX_GENERATIONS_DIRECTORY,
};

mod clone;

use clone::create_authenticated_republish_candidate;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use clone::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
#[cfg(test)]
pub(crate) use clone::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

#[derive(Debug)]
pub(crate) enum CurrentRepublishOutcome {
    Published(ActiveGenerationPointer),
    CommittedVisible {
        pointer: ActiveGenerationPointer,
        recovery: RepublishRecovery,
    },
    CommittedRecoveryRequired {
        recovery: RepublishRecovery,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RepublishRecovery {
    generation_id: String,
    detail: String,
}

impl RepublishRecovery {
    fn new(generation_id: String, detail: String) -> Self {
        Self {
            generation_id,
            detail,
        }
    }

    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }
}

/// Replays atomic publication over an already-current generation for fault and
/// disk qualification without changing its payload or owner metadata.
#[cfg(test)]
pub(crate) fn republish_current_for_qualification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
) -> Result<CurrentRepublishOutcome> {
    republish_current(root, pointer, options, None)
}

/// Atomically replaces only the owner metadata of an already-current
/// generation while preserving its exact manifest, segments, and generation
/// identity.
pub(crate) fn republish_current_with_publication_metadata(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
    publication_metadata: Arc<[u8]>,
) -> Result<CurrentRepublishOutcome> {
    republish_current(root, pointer, options, Some(publication_metadata))
}

fn republish_current(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
    replacement_publication_metadata: Option<Arc<[u8]>>,
) -> Result<CurrentRepublishOutcome> {
    let current_index = open_slot_index(root, pointer.active())?;
    validate_schema(&current_index.schema())?;
    fields_from_schema(&current_index.schema())?;
    let current_metas = current_index.load_metas()?;
    let current_publication = load_publication_for_metas(root, &current_metas)?;
    if current_publication.generation_id != pointer.active().generation_id() {
        return Err(IndexError::WriterInvariant(
            "qualification republish requires a current Core generation",
        ));
    }
    let current_reader = current_index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let current_searcher = current_reader.searcher();
    verify_complete_searcher(
        &current_searcher,
        &current_publication.manifest,
        &slot_path(root, pointer.active()),
        Some(pointer),
        pointer.active().physical_integrity_digest(),
    )?;

    republish_checkpoint(RepublishStage::BeforeCandidateCreation, None)?;
    let candidate = create_authenticated_republish_candidate(root, pointer, &current_index)?;
    let candidate_path = root
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(&candidate.directory_name);
    let candidate_directory_name = candidate.directory_name.clone();
    let republish = republish_candidate(
        root,
        pointer,
        options,
        &candidate,
        &candidate_path,
        &candidate_directory_name,
        &current_metas,
        replacement_publication_metadata.or(current_publication.metadata),
        current_publication.manifest,
        current_publication.generation_id,
    );
    if republish.is_err()
        && load_active_generation_pointer(root).ok().flatten().as_ref() == Some(pointer)
    {
        candidate.discard();
    }
    republish
}

#[allow(clippy::too_many_arguments)]
fn republish_candidate(
    root: &Path,
    base_pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
    candidate: &clone::RepublishCandidate,
    candidate_path: &Path,
    candidate_directory_name: &str,
    base_metas: &tantivy::IndexMeta,
    publication_metadata: Option<Arc<[u8]>>,
    current_manifest: GenerationManifest,
    current_generation_id: String,
) -> Result<CurrentRepublishOutcome> {
    republish_checkpoint(RepublishStage::AfterCandidateCreation, Some(candidate_path))?;
    candidate.validate_binding()?;
    let candidate_index = &candidate.index;
    validate_schema(&candidate_index.schema())?;
    fields_from_schema(&candidate_index.schema())?;
    if candidate_index.settings() != &lexical_index_settings() {
        return Err(IndexError::IndexSettingsMismatch(
            crate::LEXICAL_SCHEMA_VERSION,
        ));
    }
    let cloned_metas = candidate_index.load_metas()?;
    if payload_generation_id(&cloned_metas)?.as_deref()
        != Some(base_pointer.active().generation_id())
        || meta_generation(&cloned_metas) != meta_generation(base_metas)
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    candidate.validate_binding()?;
    let mut writer = construct_index_writer_with_retry(candidate_index, options)?;
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    let mut prepared = writer.prepare_commit()?;
    candidate.validate_binding()?;
    if let Err(error) = write_manifest(root, &current_generation_id, &current_manifest) {
        let _ = prepared.abort();
        return Err(error);
    }
    let payload =
        match canonical_commit_payload(&current_generation_id, publication_metadata.as_deref()) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = prepared.abort();
                return Err(error);
            }
        };
    prepared.set_payload(&payload);
    if let Err(error) =
        republish_checkpoint(RepublishStage::BeforeCandidateCommit, Some(candidate_path))
    {
        let _ = prepared.abort();
        return Err(error);
    }
    if let Err(error) = candidate.validate_binding() {
        let _ = prepared.abort();
        return Err(error);
    }
    let commit_result = prepared.commit();
    writer.wait_merging_threads()?;
    if let Err(error) = commit_result {
        reconcile_commit_error(
            candidate_index,
            &current_generation_id,
            Some(base_pointer.active().generation_id()),
            error,
        )?;
    }
    republish_checkpoint(RepublishStage::AfterCandidateCommit, Some(candidate_path))?;
    candidate.validate_binding()?;
    republish_checkpoint(RepublishStage::BeforeCandidateSync, Some(candidate_path))?;
    candidate.validate_binding()?;
    sync_generation(candidate_path)?;
    republish_checkpoint(RepublishStage::AfterCandidateSync, Some(candidate_path))?;
    candidate.validate_binding()?;
    republish_checkpoint(
        RepublishStage::BeforeCandidateVerification,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;

    let verified = verify_candidate(
        root,
        base_pointer,
        candidate_path,
        candidate_directory_name,
        base_metas,
        publication_metadata.as_deref(),
        &current_manifest,
        &current_generation_id,
    )?;
    republish_checkpoint(
        RepublishStage::AfterCandidateVerification,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;
    let next_pointer =
        ActiveGenerationPointer::new(verified.slot.clone(), Some(base_pointer.active().clone()))?;
    republish_checkpoint(
        RepublishStage::BeforePointerPublication,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;
    let outcome = match publish_active_generation_pointer(root, &next_pointer) {
        Ok(PointerPublicationOutcome::Durable) => {
            Ok(CurrentRepublishOutcome::Published(next_pointer))
        }
        Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
            reconcile_committed_pointer(root, &next_pointer, &current_generation_id, detail)
        }
        // The atomic-write contract guarantees that every `Err` occurred
        // before replacement, so the predecessor remains query authority.
        Err(error) => Err(error),
    };
    if let Ok(
        CurrentRepublishOutcome::Published(pointer)
        | CurrentRepublishOutcome::CommittedVisible { pointer, .. },
    ) = &outcome
    {
        let _ = certify_activated_generation(
            root,
            pointer,
            pointer.active(),
            &verified.index,
            &verified.physical_integrity_audit,
        );
    }
    outcome
}

struct VerifiedRepublishCandidate {
    index: Index,
    slot: GenerationSlot,
    physical_integrity_audit: PhysicalIntegrityAudit,
}

#[allow(clippy::too_many_arguments)]
fn verify_candidate(
    root: &Path,
    base_pointer: &ActiveGenerationPointer,
    candidate_path: &Path,
    candidate_directory_name: &str,
    base_metas: &tantivy::IndexMeta,
    expected_publication_metadata: Option<&[u8]>,
    expected_manifest: &GenerationManifest,
    expected_generation_id: &str,
) -> Result<VerifiedRepublishCandidate> {
    let reopened_directory =
        DurableMmapDirectory::open(candidate_path).map_err(tantivy::TantivyError::from)?;
    let reopened = Index::open(reopened_directory)?;
    validate_schema(&reopened.schema())?;
    fields_from_schema(&reopened.schema())?;
    if reopened.settings() != &lexical_index_settings() {
        return Err(IndexError::IndexSettingsMismatch(
            crate::LEXICAL_SCHEMA_VERSION,
        ));
    }
    let metas = reopened.load_metas()?;
    if meta_generation(&metas) != meta_generation(base_metas) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let publication = load_publication_for_metas(root, &metas)?;
    if publication.generation_id != expected_generation_id
        || publication.metadata.as_deref() != expected_publication_metadata
        || serde_json::to_vec(&publication.manifest)? != serde_json::to_vec(expected_manifest)?
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let reader = reopened
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    if searcher_generation(&searcher) != meta_generation(&metas) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let physical_integrity_audit =
        physical_integrity_audit(&reopened, candidate_path, Some(base_pointer))?;
    verify_searcher(&searcher, expected_manifest)?;
    let slot = GenerationSlot::new(
        expected_generation_id.to_owned(),
        candidate_directory_name.to_owned(),
        physical_integrity_audit.digest().to_owned(),
    )?;
    Ok(VerifiedRepublishCandidate {
        index: reopened,
        slot,
        physical_integrity_audit,
    })
}

fn reconcile_committed_pointer(
    root: &Path,
    expected: &ActiveGenerationPointer,
    generation_id: &str,
    publication_detail: String,
) -> Result<CurrentRepublishOutcome> {
    let reconciliation = load_pointer_for_republish_reconciliation(root);
    if matches!(reconciliation, Ok(Some(ref pointer)) if pointer == expected) {
        return Ok(CurrentRepublishOutcome::CommittedVisible {
            pointer: expected.clone(),
            recovery: RepublishRecovery::new(generation_id.to_owned(), publication_detail),
        });
    }

    let reconciliation_detail = match reconciliation {
        Ok(pointer) => format!("active pointer was {pointer:?}"),
        Err(error) => format!("pointer reload failed: {error}"),
    };
    match publish_active_generation_pointer(root, expected) {
        Ok(PointerPublicationOutcome::Durable) => {
            Ok(CurrentRepublishOutcome::CommittedVisible {
                pointer: expected.clone(),
                recovery: RepublishRecovery::new(
                    generation_id.to_owned(),
                    format!(
                        "{publication_detail}; {reconciliation_detail}; replacement pointer republished durably"
                    ),
                ),
            })
        }
        Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
            Ok(CurrentRepublishOutcome::CommittedVisible {
                pointer: expected.clone(),
                recovery: RepublishRecovery::new(
                    generation_id.to_owned(),
                    format!(
                        "{publication_detail}; {reconciliation_detail}; replacement pointer republished with durability uncertainty: {detail}"
                    ),
                ),
            })
        }
        Err(repair_error) => Ok(CurrentRepublishOutcome::CommittedRecoveryRequired {
            recovery: RepublishRecovery::new(
                generation_id.to_owned(),
                format!(
                    "{publication_detail}; {reconciliation_detail}; replacement pointer repair failed: {repair_error}"
                ),
            ),
        }),
    }
}

#[cfg(test)]
type PointerReconciliationTestHook =
    Box<dyn FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>>;

#[cfg(test)]
thread_local! {
    static POINTER_RECONCILIATION_TEST_HOOK: std::cell::RefCell<Option<PointerReconciliationTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct PointerReconciliationTestHookGuard(Option<PointerReconciliationTestHook>);

#[cfg(test)]
impl PointerReconciliationTestHookGuard {
    pub(crate) fn set<F>(hook: F) -> Self
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>> + 'static,
    {
        let previous =
            POINTER_RECONCILIATION_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for PointerReconciliationTestHookGuard {
    fn drop(&mut self) {
        POINTER_RECONCILIATION_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn load_pointer_for_republish_reconciliation(
    root: &Path,
) -> Result<Option<ActiveGenerationPointer>> {
    POINTER_RECONCILIATION_TEST_HOOK.with(|active| {
        let mut active = active.borrow_mut();
        match active.as_mut() {
            Some(hook) => hook(root),
            None => load_active_generation_pointer(root),
        }
    })
}

#[cfg(not(test))]
fn load_pointer_for_republish_reconciliation(
    root: &Path,
) -> Result<Option<ActiveGenerationPointer>> {
    load_active_generation_pointer(root)
}

/// Cleanup after a visible republish is opportunistic. Query authority already
/// changed atomically, so reclamation failures must never be reported as a
/// failed republish.
pub(crate) fn best_effort_post_republish_cleanup(root: &Path, pointer: &ActiveGenerationPointer) {
    if republish_checkpoint(RepublishStage::PostPublicationCleanup, Some(root)).is_err() {
        return;
    }
    let _ = reclaim_inactive_generation_directories(root, Some(pointer));
    let retained_generation_ids = std::iter::once(pointer.active())
        .chain(pointer.previous())
        .map(|slot| slot.generation_id().to_owned())
        .collect::<Vec<_>>();
    let _ = reclaim_unreferenced_manifests(root, &retained_generation_ids);
    let _ = reclaim_unreferenced_certifications(root, Some(pointer));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepublishStage {
    BeforeCandidateCreation,
    AfterCandidateCreation,
    BeforeCandidateCommit,
    AfterCandidateCommit,
    BeforeCandidateSync,
    AfterCandidateSync,
    BeforeCandidateVerification,
    AfterCandidateVerification,
    BeforePointerPublication,
    PostPublicationCleanup,
}

#[cfg(not(test))]
fn republish_checkpoint(_stage: RepublishStage, _path: Option<&Path>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
type RepublishTestHook = Box<dyn for<'a> FnMut(RepublishStage, Option<&'a Path>) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static REPUBLISH_TEST_HOOK: std::cell::RefCell<Option<RepublishTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct RepublishTestHookGuard(Option<RepublishTestHook>);

#[cfg(test)]
impl RepublishTestHookGuard {
    pub(crate) fn set<F>(hook: F) -> Self
    where
        F: for<'a> FnMut(RepublishStage, Option<&'a Path>) -> Result<()> + 'static,
    {
        let previous = REPUBLISH_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for RepublishTestHookGuard {
    fn drop(&mut self) {
        REPUBLISH_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn republish_checkpoint(stage: RepublishStage, path: Option<&Path>) -> Result<()> {
    REPUBLISH_TEST_HOOK.with(|active| {
        let mut active = active.borrow_mut();
        match active.as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        }
    })
}
