use std::{path::Path, sync::Arc};

use tantivy::{indexer::NoMergePolicy, Index, ReloadPolicy};

use crate::{
    audit_searcher_core_contract, classify_core_contract_generation,
    current_core_record_contract_fingerprint, durable_directory::DurableMmapDirectory,
    fields_from_schema, validate_schema, writer_support::construct_index_writer_with_retry,
    CommittedPredecessorMigrationRecovery, CoreContractGeneration, GenerationManifest, IndexError,
    Result, WriterOptions,
};

use super::{
    canonical_commit_payload, certify_activated_generation, lexical_index_settings,
    load_active_generation_pointer, load_core_contract_for_metas, load_publication_for_metas,
    meta_generation, open_slot_index, payload_generation_id, physical_integrity_audit,
    publish_active_generation_pointer, reclaim_inactive_generation_directories,
    reclaim_unreferenced_certifications, reclaim_unreferenced_manifests, reconcile_commit_error,
    searcher_generation, slot_path, sync_generation, verify_complete_searcher, verify_searcher,
    write_manifest, ActiveGenerationPointer, GenerationSlot, PhysicalIntegrityAudit,
    PointerPublicationOutcome, INDEX_GENERATIONS_DIRECTORY,
};

mod clone;

use clone::create_authenticated_migration_candidate;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use clone::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
#[cfg(test)]
pub(crate) use clone::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

#[derive(Debug)]
pub(crate) enum PredecessorMigrationOutcome {
    Unchanged(ActiveGenerationPointer),
    Migrated(ActiveGenerationPointer),
    CommittedVisible {
        pointer: ActiveGenerationPointer,
        recovery: CommittedPredecessorMigrationRecovery,
    },
    CommittedRecoveryRequired {
        recovery: CommittedPredecessorMigrationRecovery,
    },
}

/// Under the caller-held root writer lease, upgrades the one allowlisted
/// same-epoch predecessor publication without consulting source authority.
///
/// An ordinary error is returned only while the predecessor pointer remains
/// query authority. Once replacement is visible, durability or reconciliation
/// uncertainty is returned only as a committed outcome. `CommittedVisible`
/// carries a usable, repaired successor pointer; `CommittedRecoveryRequired`
/// requires fix-forward recovery and never masquerades as a failed migration.
/// Post-publication cleanup is best effort.
pub(crate) fn migrate_allowlisted_predecessor(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
) -> Result<PredecessorMigrationOutcome> {
    let contract = (|| {
        let raw_directory = DurableMmapDirectory::open(slot_path(root, pointer.active()))
            .map_err(tantivy::TantivyError::from)?;
        let raw_index = Index::open(raw_directory)?;
        load_core_contract_for_metas(root, &raw_index.load_metas()?)
    })();
    match contract {
        Ok(CoreContractGeneration::Current) => {
            return Ok(PredecessorMigrationOutcome::Unchanged(pointer.clone()))
        }
        Ok(CoreContractGeneration::AllowlistedPredecessor) => {}
        Err(error @ IndexError::CoreRecordContractMismatch { .. }) => return Err(error),
        // Let the established current-generation open/rebuild path classify
        // malformed or superseded non-fingerprint state with its existing
        // typed behavior. Once the exact predecessor is identified, every
        // later error below is propagated and can never enter source rebuild.
        Err(_) => return Ok(PredecessorMigrationOutcome::Unchanged(pointer.clone())),
    }

    let predecessor_index = open_slot_index(root, pointer.active())?;
    validate_schema(&predecessor_index.schema())?;
    fields_from_schema(&predecessor_index.schema())?;
    let predecessor_metas = predecessor_index.load_metas()?;
    let predecessor_publication = load_publication_for_metas(root, &predecessor_metas)?;
    if predecessor_publication.generation_id != pointer.active().generation_id() {
        return Err(IndexError::InvalidActiveGenerationPointer);
    }
    if predecessor_publication.core_contract != CoreContractGeneration::AllowlistedPredecessor {
        return Err(IndexError::WriterInvariant(
            "predecessor migration lost its allowlisted Core contract",
        ));
    }

    migration_checkpoint(MigrationStage::BeforePredecessorVerification, None)?;
    let predecessor_reader = predecessor_index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let predecessor_searcher = predecessor_reader.searcher();
    if searcher_generation(&predecessor_searcher) != meta_generation(&predecessor_metas) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    audit_searcher_core_contract(
        &predecessor_searcher,
        CoreContractGeneration::AllowlistedPredecessor,
    )?;
    verify_complete_searcher(
        &predecessor_searcher,
        &predecessor_publication.manifest,
        &slot_path(root, pointer.active()),
        pointer.active().physical_integrity_digest(),
    )?;
    migration_checkpoint(MigrationStage::AfterPredecessorVerification, None)?;

    let mut current_manifest = predecessor_publication.manifest.clone();
    current_manifest.core_record_contract_fingerprint = current_core_record_contract_fingerprint();
    if classify_core_contract_generation(&current_manifest.core_record_contract_fingerprint)?
        != CoreContractGeneration::Current
    {
        return Err(IndexError::WriterInvariant(
            "predecessor migration target is not the current Core contract",
        ));
    }
    current_manifest.validate_contract()?;
    let current_generation_id = current_manifest.generation_id()?;
    if current_generation_id == predecessor_publication.generation_id {
        return Err(IndexError::WriterInvariant(
            "predecessor migration did not change generation identity",
        ));
    }

    migration_checkpoint(MigrationStage::BeforeCandidateCreation, None)?;
    let candidate =
        create_authenticated_migration_candidate(root, pointer.active(), &predecessor_index)?;
    let candidate_path = root
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(&candidate.directory_name);
    let candidate_directory_name = candidate.directory_name.clone();
    let migration = migrate_candidate(
        root,
        pointer,
        options,
        &candidate,
        &candidate_path,
        &candidate_directory_name,
        &predecessor_metas,
        predecessor_publication.metadata,
        current_manifest,
        current_generation_id,
    );
    if migration.is_err()
        && load_active_generation_pointer(root).ok().flatten().as_ref() == Some(pointer)
    {
        candidate.discard();
    }
    migration
}

/// Replays final migration publication over an already-current generation for
/// the controlled migration/current-format qualification pair.
#[cfg(test)]
pub(crate) fn republish_current_for_qualification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
) -> Result<PredecessorMigrationOutcome> {
    let current_index = open_slot_index(root, pointer.active())?;
    validate_schema(&current_index.schema())?;
    fields_from_schema(&current_index.schema())?;
    let current_metas = current_index.load_metas()?;
    let current_publication = load_publication_for_metas(root, &current_metas)?;
    if current_publication.generation_id != pointer.active().generation_id()
        || current_publication.core_contract != CoreContractGeneration::Current
    {
        return Err(IndexError::WriterInvariant(
            "qualification republish requires a current Core generation",
        ));
    }
    let current_reader = current_index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let current_searcher = current_reader.searcher();
    audit_searcher_core_contract(&current_searcher, CoreContractGeneration::Current)?;
    verify_complete_searcher(
        &current_searcher,
        &current_publication.manifest,
        &slot_path(root, pointer.active()),
        pointer.active().physical_integrity_digest(),
    )?;

    let candidate =
        create_authenticated_migration_candidate(root, pointer.active(), &current_index)?;
    let candidate_path = root
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(&candidate.directory_name);
    let candidate_directory_name = candidate.directory_name.clone();
    let migration = migrate_candidate(
        root,
        pointer,
        options,
        &candidate,
        &candidate_path,
        &candidate_directory_name,
        &current_metas,
        current_publication.metadata,
        current_publication.manifest,
        current_publication.generation_id,
    );
    if migration.is_err()
        && load_active_generation_pointer(root).ok().flatten().as_ref() == Some(pointer)
    {
        candidate.discard();
    }
    migration
}

#[allow(clippy::too_many_arguments)]
fn migrate_candidate(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    options: &WriterOptions,
    candidate: &clone::MigrationCandidate,
    candidate_path: &Path,
    candidate_directory_name: &str,
    predecessor_metas: &tantivy::IndexMeta,
    publication_metadata: Option<Arc<[u8]>>,
    current_manifest: GenerationManifest,
    current_generation_id: String,
) -> Result<PredecessorMigrationOutcome> {
    migration_checkpoint(MigrationStage::AfterCandidateCreation, Some(candidate_path))?;
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
        != Some(predecessor_pointer.active().generation_id())
        || meta_generation(&cloned_metas) != meta_generation(predecessor_metas)
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
        migration_checkpoint(MigrationStage::BeforeCandidateCommit, Some(candidate_path))
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
            Some(predecessor_pointer.active().generation_id()),
            error,
        )?;
    }
    migration_checkpoint(MigrationStage::AfterCandidateCommit, Some(candidate_path))?;
    candidate.validate_binding()?;
    migration_checkpoint(MigrationStage::BeforeCandidateSync, Some(candidate_path))?;
    candidate.validate_binding()?;
    sync_generation(candidate_path)?;
    migration_checkpoint(MigrationStage::AfterCandidateSync, Some(candidate_path))?;
    candidate.validate_binding()?;
    migration_checkpoint(
        MigrationStage::BeforeCandidateVerification,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;

    let verified = verify_candidate(
        root,
        candidate_path,
        candidate_directory_name,
        predecessor_metas,
        publication_metadata.as_deref(),
        &current_manifest,
        &current_generation_id,
    )?;
    migration_checkpoint(
        MigrationStage::AfterCandidateVerification,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;
    let next_pointer = ActiveGenerationPointer::new(
        verified.slot.clone(),
        Some(predecessor_pointer.active().clone()),
    )?;
    migration_checkpoint(
        MigrationStage::BeforePointerPublication,
        Some(candidate_path),
    )?;
    candidate.validate_binding()?;
    let outcome = match publish_active_generation_pointer(root, &next_pointer) {
        Ok(PointerPublicationOutcome::Durable) => {
            Ok(PredecessorMigrationOutcome::Migrated(next_pointer))
        }
        Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
            reconcile_committed_pointer(root, &next_pointer, &current_generation_id, detail)
        }
        // The atomic-write contract guarantees that every `Err` occurred
        // before replacement, so the predecessor remains query authority.
        Err(error) => Err(error),
    };
    if let Ok(
        PredecessorMigrationOutcome::Migrated(pointer)
        | PredecessorMigrationOutcome::CommittedVisible { pointer, .. },
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

struct VerifiedMigrationCandidate {
    index: Index,
    slot: GenerationSlot,
    physical_integrity_audit: PhysicalIntegrityAudit,
}

#[allow(clippy::too_many_arguments)]
fn verify_candidate(
    root: &Path,
    candidate_path: &Path,
    candidate_directory_name: &str,
    predecessor_metas: &tantivy::IndexMeta,
    expected_publication_metadata: Option<&[u8]>,
    expected_manifest: &GenerationManifest,
    expected_generation_id: &str,
) -> Result<VerifiedMigrationCandidate> {
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
    if meta_generation(&metas) != meta_generation(predecessor_metas) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let publication = load_publication_for_metas(root, &metas)?;
    if publication.core_contract != CoreContractGeneration::Current
        || publication.generation_id != expected_generation_id
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
    let physical_integrity_audit = physical_integrity_audit(&reopened, candidate_path)?;
    verify_searcher(&searcher, expected_manifest)?;
    let slot = GenerationSlot::new(
        expected_generation_id.to_owned(),
        candidate_directory_name.to_owned(),
        physical_integrity_audit.digest().to_owned(),
    )?;
    Ok(VerifiedMigrationCandidate {
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
) -> Result<PredecessorMigrationOutcome> {
    let reconciliation = load_pointer_for_migration_reconciliation(root);
    if matches!(reconciliation, Ok(Some(ref pointer)) if pointer == expected) {
        return Ok(PredecessorMigrationOutcome::CommittedVisible {
            pointer: expected.clone(),
            recovery: CommittedPredecessorMigrationRecovery::new(
                generation_id.to_owned(),
                publication_detail,
            ),
        });
    }

    let reconciliation_detail = match reconciliation {
        Ok(pointer) => format!("active pointer was {pointer:?}"),
        Err(error) => format!("pointer reload failed: {error}"),
    };
    match publish_active_generation_pointer(root, expected) {
        Ok(PointerPublicationOutcome::Durable) => {
            Ok(PredecessorMigrationOutcome::CommittedVisible {
                pointer: expected.clone(),
                recovery: CommittedPredecessorMigrationRecovery::new(
                    generation_id.to_owned(),
                    format!(
                        "{publication_detail}; {reconciliation_detail}; successor pointer republished durably"
                    ),
                ),
            })
        }
        Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
            Ok(PredecessorMigrationOutcome::CommittedVisible {
                pointer: expected.clone(),
                recovery: CommittedPredecessorMigrationRecovery::new(
                    generation_id.to_owned(),
                    format!(
                        "{publication_detail}; {reconciliation_detail}; successor pointer republished with durability uncertainty: {detail}"
                    ),
                ),
            })
        }
        Err(repair_error) => Ok(PredecessorMigrationOutcome::CommittedRecoveryRequired {
            recovery: CommittedPredecessorMigrationRecovery::new(
                generation_id.to_owned(),
                format!(
                    "{publication_detail}; {reconciliation_detail}; successor pointer repair failed: {repair_error}"
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
fn load_pointer_for_migration_reconciliation(
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
fn load_pointer_for_migration_reconciliation(
    root: &Path,
) -> Result<Option<ActiveGenerationPointer>> {
    load_active_generation_pointer(root)
}

/// Cleanup after a visible migration is opportunistic. Query authority already
/// changed atomically, so reclamation failures must never be reported as a
/// failed migration.
pub(crate) fn best_effort_post_migration_cleanup(root: &Path, pointer: &ActiveGenerationPointer) {
    if migration_checkpoint(MigrationStage::PostPublicationCleanup, Some(root)).is_err() {
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStage {
    BeforePredecessorVerification,
    AfterPredecessorVerification,
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
#[derive(Debug, Clone, Copy)]
enum MigrationStage {
    BeforePredecessorVerification,
    AfterPredecessorVerification,
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

#[cfg(test)]
type MigrationTestHook = Box<dyn for<'a> FnMut(MigrationStage, Option<&'a Path>) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static MIGRATION_TEST_HOOK: std::cell::RefCell<Option<MigrationTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct MigrationTestHookGuard(Option<MigrationTestHook>);

#[cfg(test)]
impl MigrationTestHookGuard {
    pub(crate) fn set<F>(hook: F) -> Self
    where
        F: for<'a> FnMut(MigrationStage, Option<&'a Path>) -> Result<()> + 'static,
    {
        let previous = MIGRATION_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for MigrationTestHookGuard {
    fn drop(&mut self) {
        MIGRATION_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn migration_checkpoint(stage: MigrationStage, path: Option<&Path>) -> Result<()> {
    MIGRATION_TEST_HOOK.with(|active| {
        let mut active = active.borrow_mut();
        match active.as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        }
    })
}

#[cfg(not(test))]
fn migration_checkpoint(_stage: MigrationStage, _path: Option<&Path>) -> Result<()> {
    Ok(())
}
