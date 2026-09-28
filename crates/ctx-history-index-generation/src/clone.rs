use std::path::Path;

use tantivy::Index;

use crate::{ActiveGenerationPointer, CandidateGeneration, GenerationError as IndexError, Result};

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd"
)))]
compile_error!("predecessor clone is only qualified on ctx release targets");

mod candidate;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod exact_copy;
mod metrics;
#[cfg(any(
    test,
    feature = "test-support",
    target_os = "windows",
    target_os = "freebsd"
))]
mod portable;
mod resource;

pub use candidate::CandidateActivationFence;
use metrics::record_candidate_clone_metrics;
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) use metrics::CandidateCloneMetrics;
#[cfg(any(test, feature = "test-support"))]
pub use metrics::{candidate_clone_metrics, reset_candidate_clone_metrics, CandidateCloneMetrics};
use resource::{admit_clone_resource, validate_single_component};

pub(super) const MAX_REPUBLISH_CLONE_FILES: usize = 4_096;
pub(super) const MAX_REPUBLISH_CLONE_BYTES: u64 = crate::physical::MAX_MANAGED_GENERATION_BYTES;
const MAX_REPUBLISH_DIRECTORY_ENTRIES: usize = 4_096;
const REPUBLISH_HEADROOM_RESERVE_BYTES: u64 = 16 * 1024 * 1024;
const MANAGED_FILE: &str = ".managed.json";
const TANTIVY_LOCK_FILES: [&str; 2] = [".tantivy-meta.lock", ".tantivy-writer.lock"];

/// Admission reserves a writer buffer and control-file scratch, not a rewrite
/// of every retained segment. Incremental writers share immutable segments;
/// actual append/merge output can still exhaust the volume and must fail before
/// candidate activation. This probe is not a reservation or a peak-space bound.
fn writer_output_headroom(writer_memory_bytes: u64) -> Result<u64> {
    writer_memory_bytes
        .checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES)
        .ok_or(IndexError::CountOverflow)
}

/// Observe this candidate volume using the same platform probe as clone admission.
pub(crate) fn candidate_available_bytes(root: &Path) -> Result<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        return portable::candidate_available_bytes(root);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::candidate_available_bytes(root)
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        portable::candidate_available_bytes(root)
    }
}

/// Diagnostic observation only; failure to sample must preserve the original error.
pub fn observed_low_candidate_space(root: &Path) -> Option<u64> {
    candidate_available_bytes(root)
        .ok()
        .filter(|available| *available < REPUBLISH_HEADROOM_RESERVE_BYTES)
}

pub fn create_authenticated_candidate_generation(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
    writer_memory_bytes: u64,
) -> Result<CandidateGeneration> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        return portable::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        )
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        portable::create_authenticated_candidate_generation(
            root,
            predecessor_pointer,
            predecessor_index,
            writer_memory_bytes,
        )
    }
}

pub(crate) fn bind_candidate_activation_fence(
    root: &Path,
    directory_name: &Path,
) -> Result<CandidateActivationFence> {
    #[cfg(any(test, feature = "test-support"))]
    if portable::forced_for_test() {
        return portable::CandidateGuard::bind(root, directory_name)
            .map(CandidateActivationFence::portable);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::CandidateGuard::bind(root, directory_name)
            .map(CandidateActivationFence::descriptor_clone)
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        portable::CandidateGuard::bind(root, directory_name).map(CandidateActivationFence::portable)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;

#[cfg(all(
    any(test, feature = "test-support"),
    any(target_os = "linux", target_os = "macos")
))]
pub use unix::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};

#[cfg(any(test, feature = "test-support"))]
pub use portable::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

#[cfg(test)]
mod tests;
