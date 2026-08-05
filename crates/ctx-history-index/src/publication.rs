mod certification;
mod generation;
mod manifest;
mod migration;
mod verification;

pub(crate) use certification::{
    certify_activated_generation, reclaim_unreferenced_certifications,
    scrub_and_certify_physical_integrity, verify_or_certify_physical_integrity,
};
pub(crate) use generation::{
    create_candidate_generation, lexical_index_settings, load_active_generation_pointer,
    open_slot_index, publish_active_generation_pointer, reclaim_inactive_generation_directories,
    slot_path, sync_generation, ActiveGenerationPointer, GenerationSlot, PointerPublicationOutcome,
    INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use generation::{ReclamationStage, ReclamationTestHookGuard};
#[cfg(test)]
pub(crate) use manifest::manifest_path;
pub(crate) use manifest::{
    canonical_commit_payload, load_core_contract_for_metas, load_publication_for_metas,
    meta_generation, payload_generation_id, reclaim_unreferenced_manifests, reconcile_commit_error,
    searcher_generation, sync_directory, write_manifest,
};
pub(crate) use migration::{
    best_effort_post_migration_cleanup, migrate_allowlisted_predecessor,
    PredecessorMigrationOutcome,
};
#[cfg(test)]
pub(crate) use migration::{
    republish_current_for_qualification, MigrationStage, MigrationTestHookGuard,
    PointerReconciliationTestHookGuard,
};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use migration::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
#[cfg(test)]
pub(crate) use migration::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};
pub(crate) use verification::{
    physical_integrity_audit, physical_integrity_digest, verify_complete_searcher,
    verify_physical_integrity, verify_publication_candidate, verify_searcher,
    verify_searcher_structure, PhysicalIntegrityAudit,
};

#[cfg(test)]
pub(crate) use verification::{
    candidate_identity_verification_activity, candidate_lineage_verification_activity,
    hashed_artifact_bytes, reset_verification_activity, verification_activity,
    verify_searcher_with_metrics,
};

#[cfg(test)]
pub(crate) use certification::{
    certification_file_for_active, MAX_CERTIFICATION_BYTES, MAX_CERTIFIED_ARTIFACTS,
};
