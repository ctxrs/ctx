//! Semantic work graph construction, local Git authority, and bounded query projection.

mod cursors;
pub use ctx_attribution_derivation::envelope;
#[doc(hidden)]
pub mod git_executable;
#[doc(hidden)]
pub mod graph;
#[doc(hidden)]
pub use ctx_attribution_derivation::ingest;
#[doc(hidden)]
pub mod projection;
#[doc(hidden)]
pub mod protocol;
pub mod query;
mod repository_path;
#[cfg(test)]
mod test_support;

pub use git_executable::GitExecutable;
pub use graph::segment_graph::{SegmentGraph, SegmentGraphError};
pub use ingest::{
    CoreProjectionAvailability, CoreProjectionCoverage, CoreProjectionStatus, CoreStoreError,
    PreparedCoreEvidence, PreparedCoreProjectionBatch, PreparedCoreUnit,
    ProducerAuthorityDisposition,
};

mod catch_up;
pub mod core_materialization;
mod errors;
mod evidence;
pub use ctx_attribution_derivation::evidence_preview;
use ctx_attribution_derivation::feed_model;
pub mod materializer;
pub mod presentation;
mod runtime;
pub use ctx_attribution_derivation::worker_budget;
pub use runtime::{
    blame, catch_up, catch_up_with_progress, materialization_progress, query, readiness, status,
};
mod diagnostic;
pub use catch_up::CoreMaterializationSyncOutcome;
pub use evidence::hydrate_evidence_previews;
pub use evidence_preview::{VerifiedEvidenceRecord, project_evidence_previews};

#[cfg(any(test, feature = "test-support"))]
pub mod test_fixtures;
