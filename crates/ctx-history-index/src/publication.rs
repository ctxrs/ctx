mod generation;
mod republish;
mod retention;

pub(crate) use ctx_history_index_format::{
    canonical_commit_payload, load_publication_for_metas, meta_generation, payload_generation_id,
    prepare_successor_manifest, reconcile_commit_error, searcher_generation, write_manifest,
    write_prepared_manifest,
};
#[cfg(test)]
pub(crate) use ctx_history_index_format::{verify_publication_candidate, verify_searcher};
#[cfg(test)]
pub(crate) use ctx_history_index_generation::manifest_path;
#[cfg(test)]
pub(crate) use ctx_history_index_generation::physical_integrity_digest;
pub(crate) use ctx_history_index_generation::sync_directory;
pub(crate) use ctx_history_index_generation::{
    certify_candidate_physical_integrity, reclaim_unreferenced_certifications,
    reclaim_unreferenced_manifests,
};
pub(crate) use ctx_history_index_generation::{
    physical_integrity_audit, validate_candidate_managed_files, verify_physical_integrity,
    PhysicalIntegrityAudit,
};
pub(crate) use ctx_history_index_generation::{
    prime_candidate_physical_proof, CandidateActivationFence, CandidatePhysicalProof,
};
#[cfg(not(windows))]
pub(crate) use generation::publish_active_generation_pointer_validated;
pub(crate) use generation::{
    create_candidate_generation, lexical_index_settings, load_active_generation_pointer,
    open_slot_index, publish_active_generation_pointer, reclaim_inactive_generation_directories,
    slot_path, sync_generation, ActiveGenerationPointer, GenerationSlot, PointerPublicationOutcome,
    INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use generation::{ReclamationStage, ReclamationTestHookGuard};
pub(crate) use republish::{
    best_effort_post_republish_cleanup, republish_current_with_publication_metadata,
    CurrentRepublishOutcome,
};
#[cfg(test)]
pub(crate) use republish::{
    republish_current_for_qualification, PointerReconciliationTestHookGuard, RepublishRecovery,
    RepublishStage, RepublishTestHookGuard,
};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use republish::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
#[cfg(test)]
pub(crate) use republish::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};
pub(crate) use retention::GENERATION_WRITER_LOCK_FILE;
pub use retention::{
    acquire_generation_retention_lease, load_generation_retention_lease,
    release_generation_retention_lease, GenerationRetentionLease,
};

#[cfg(test)]
pub(crate) use ctx_history_index_format::{
    candidate_identity_verification_activity, candidate_lineage_verification_activity,
    candidate_projection_verification_activity, complete_session_id_traversals,
    reset_verification_activity, verification_activity, verify_searcher_with_metrics,
};
#[cfg(test)]
pub(crate) use ctx_history_index_generation::hashed_artifact_bytes;

#[cfg(test)]
pub(crate) use ctx_history_index_generation::{
    candidate_clone_metrics, certification_file_for_active, reset_candidate_clone_metrics,
    CandidateCloneMetrics, MAX_CERTIFICATION_BYTES, MAX_CERTIFIED_ARTIFACTS,
};
