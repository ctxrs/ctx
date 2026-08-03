mod generation;
mod manifest;
mod verification;

pub(crate) use generation::{
    create_candidate_generation, lexical_index_settings, load_active_generation_pointer,
    open_slot_index, publish_active_generation_pointer, reclaim_inactive_generation_directories,
    slot_path, sync_generation, ActiveGenerationPointer, GenerationSlot,
    INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use manifest::manifest_path;
pub(crate) use manifest::{
    canonical_commit_payload, load_publication_for_metas, meta_generation, payload_generation_id,
    reclaim_unreferenced_manifests, reconcile_commit_error, searcher_generation, sync_directory,
    write_manifest,
};
pub(crate) use verification::{
    physical_integrity_digest, verify_complete_searcher, verify_physical_integrity,
    verify_searcher, verify_searcher_structure,
};

#[cfg(test)]
pub(crate) use verification::{
    reset_verification_activity, verification_activity, verify_searcher_with_metrics,
};
