mod generation;
mod manifest;
mod verification;

pub(crate) use generation::{
    create_candidate_generation, load_active_generation_pointer, open_slot_index,
    publish_active_generation_pointer, reclaim_inactive_generation_directories, sync_generation,
    ActiveGenerationPointer, GenerationSlot, INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use manifest::manifest_path;
pub(crate) use manifest::{
    load_manifest_for_metas, meta_generation, payload_generation_id,
    reclaim_unreferenced_manifests, reconcile_commit_error, searcher_generation, sync_directory,
    write_manifest,
};
pub(crate) use verification::{verify_searcher, verify_searcher_structure};

#[cfg(test)]
pub(crate) use verification::{
    verify_searcher_reference, verify_searcher_reference_with_metrics, verify_searcher_with_metrics,
};
